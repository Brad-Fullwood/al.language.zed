//! .NET bridge for CodeAnalysis API — semantic analysis, analyzers, compilation.
//!
//! Hosts the .NET CLR in-process via `netcorehost` and communicates with a thin
//! C# bridge DLL using JSON-in/JSON-out over function pointers. No subprocess.
//!
//! Architecture: Rust → netcorehost → Bridge.dll → CodeAnalysis.dll

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::host::DotNetHost;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeRequest {
    pub file: PathBuf,
    pub source: String,
    pub analyzers: Vec<String>,
    pub package_cache: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileResult {
    pub success: bool,
    pub diagnostics: Vec<DiagnosticEntry>,
    pub app_path: Option<PathBuf>,
    /// Raw compiler stdout+stderr (capped bridge-side). Present so a
    /// failed compile with no parseable diagnostics is never silent.
    #[serde(default)]
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEntry {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub severity: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TypeInfo {
    pub name: String,
    pub kind: String,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    pub kind: String,
    pub detail: Option<String>,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinType {
    pub name: String,
    #[serde(default)]
    pub methods: Vec<BuiltinMethod>,
    #[serde(default)]
    pub enum_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinMethod {
    pub name: String,
    #[serde(default)]
    pub parameters: Vec<MethodParameter>,
    #[serde(default)]
    pub return_type: Option<String>,
    #[serde(default)]
    pub documentation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MethodParameter {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub type_name: String,
    #[serde(default)]
    pub is_var: bool,
}

impl std::fmt::Display for MethodParameter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_var {
            write!(f, "var ")?;
        }
        write!(f, "{}: {}", self.name, self.type_name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorCodeInfo {
    pub code: String,
    pub message: String,
    pub severity: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error("Failed to initialize .NET host: {0}")]
    HostInit(String),

    #[error("Bridge not initialized")]
    NotInitialized,

    #[error("RPC response timed out after {0:?}")]
    Timeout(Duration),

    #[error("RPC error (code {code}): {message}")]
    RpcError { code: i32, message: String },

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Bridge Mutex is poisoned -- CLR state may be corrupt after a panic")]
    Poisoned,

    /// The bridge is in a temporary cooldown window after a prior call timed
    /// out or hung. Distinct from `Poisoned` (true mutex poison) so callers
    /// can choose to retry after the cooldown elapses without triggering a
    /// full bridge restart.
    #[error("Bridge in cooldown after recent timeout/hang ({0})")]
    Cooldown(&'static str),

    /// The caller-supplied document buffer exceeds `MAX_TEXT_BYTES`. Rejected
    /// at the bridge boundary before JSON serialization so a pathologically
    /// large open document can't balloon the bridge's memory footprint.
    #[error("Input document too large ({size} bytes, max {max})")]
    InputTooLarge { size: usize, max: usize },
}

/// Maximum size (in bytes) of a caller-supplied unsaved-text buffer accepted by
/// position-based bridge calls (`type_at` / `completions_at`). Buffers larger
/// than this are rejected before JSON serialization to bound the bridge's
/// memory use; a real AL source file is orders of magnitude smaller. This is a
/// transport/resource limit, not an AL language value.
pub const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// Reject a caller-supplied unsaved-text buffer that exceeds [`MAX_TEXT_BYTES`].
/// Pulled out of `type_at` / `completions_at` so the boundary check is shared
/// and unit-testable without a live CLR.
fn check_text_size(text: Option<&str>) -> Result<(), SemanticError> {
    if let Some(t) = text {
        if t.len() > MAX_TEXT_BYTES {
            return Err(SemanticError::InputTooLarge {
                size: t.len(),
                max: MAX_TEXT_BYTES,
            });
        }
    }
    Ok(())
}

/// Async bridge to the .NET CodeAnalysis API.
///
/// Uses in-process .NET hosting via `netcorehost`. The .NET runtime is loaded
/// once and stays alive for the lifetime of the bridge. All calls go through
/// function pointers — no subprocess, no JSON-RPC, no stdio.
///
/// Thread-safe: can be shared across tokio tasks via `Arc`. Concurrent bridge
/// calls are serialized by a `std::sync::Mutex` around `DotNetHost` — the CLR
/// response buffer is shared, so only one call may be in flight at a time.
pub struct SemanticBridge {
    host: Arc<std::sync::Mutex<DotNetHost>>,
    version: String,
    /// Unix timestamp (seconds) when the most recent timeout fired, or 0 if
    /// no timeout has occurred. Combined with `TIMEOUT_COOLDOWN`, this drives
    /// the retry-after-cooldown behaviour: a single transient timeout no
    /// longer permanently disables semantic analysis for the rest of the
    /// process's life.
    last_timeout_secs: std::sync::atomic::AtomicU64,
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Cooldown after a timeout: bridge calls are short-circuited to `Poisoned`
/// for this duration, then we let them through again. A still-hung Mutex
/// will simply re-trip the timeout and reset the cooldown; a transient
/// timeout (slow GC, paging) gets a chance to recover.
const TIMEOUT_COOLDOWN: Duration = Duration::from_secs(60);

/// Carry a timeout cooldown stamp forward into a (new) bridge generation.
///
/// Only a non-zero `secs` is applied — a fresh stamp must never be cleared by
/// a restart (which would shorten an active cooldown and let an in-flight hung
/// call from the prior generation be bypassed). Extracted as a free function
/// so the advance-only semantics can be unit-tested without loading the CLR.
fn seed_timeout_stamp(stamp: &std::sync::atomic::AtomicU64, secs: u64) {
    if secs != 0 {
        stamp.store(secs, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Outcome of the timeout-cooldown gate (see [`cooldown_gate`]).
///
/// Extracted as a free function over a generic `&Mutex<T>` so the
/// `try_lock()` race (T047) can be exercised deterministically in tests:
/// a test can pre-lock the mutex to model a still-hung CLR call, poison it
/// to model a panicked call, or leave it free — without loading the CLR.
#[derive(Debug)]
enum CooldownDecision {
    Proceed,
    ShortCircuit(SemanticError),
}

/// Pure cooldown-gate decision over the serializing mutex.
///
/// `last_timeout_secs` is the atomic stamp (0 = no recent timeout). `now` is
/// the current Unix time in seconds, `cooldown` the cooldown window. The lock
/// is probed via `try_lock()` only once the cooldown has elapsed. As a side
/// effect this updates `last_timeout_secs` exactly as the inline logic did:
/// cleared to 0 when the lock is free (recovered), re-stamped to `now` when
/// the lock is still held or poisoned (extend cooldown).
fn cooldown_gate<T>(
    last_timeout_secs: &std::sync::atomic::AtomicU64,
    host: &std::sync::Mutex<T>,
    now: u64,
    cooldown: Duration,
    method: &str,
) -> CooldownDecision {
    let last = last_timeout_secs.load(std::sync::atomic::Ordering::Relaxed);
    if last == 0 {
        return CooldownDecision::Proceed;
    }
    let elapsed = now.saturating_sub(last);
    if elapsed < cooldown.as_secs() {
        tracing::trace!(
            method,
            elapsed,
            cooldown_secs = cooldown.as_secs(),
            "semantic bridge: short-circuiting (cooldown window)"
        );
        return CooldownDecision::ShortCircuit(SemanticError::Cooldown(
            "cooldown window after timeout",
        ));
    }
    match host.try_lock() {
        Ok(_guard) => {
            last_timeout_secs.store(0, std::sync::atomic::Ordering::Relaxed);
            tracing::info!(
                method,
                elapsed,
                "semantic bridge: cooldown elapsed, lock free — resuming"
            );
            CooldownDecision::Proceed
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            last_timeout_secs.store(now, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(
                method,
                elapsed,
                "semantic bridge: cooldown elapsed but lock STILL held — extending cooldown"
            );
            CooldownDecision::ShortCircuit(SemanticError::Cooldown(
                "hung call still holding the lock",
            ))
        }
        Err(std::sync::TryLockError::Poisoned(_)) => {
            last_timeout_secs.store(now, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(
                method,
                "semantic bridge: Mutex poisoned — extending cooldown"
            );
            CooldownDecision::ShortCircuit(SemanticError::Poisoned)
        }
    }
}

impl SemanticBridge {
    /// Initialize the .NET bridge with explicit paths.
    ///
    /// - `code_analysis`: path to `Microsoft.Dynamics.Nav.CodeAnalysis.dll`
    /// - `version`: toolchain version string (used for cache keying)
    ///
    /// This loads the CLR in-process and initializes the bridge DLL.
    pub fn new(code_analysis: &Path, version: &str) -> Result<Self, SemanticError> {
        let (bridge_dll, runtime_config) = super::host::find_bridge_dll()?;
        let host = DotNetHost::new(&bridge_dll, &runtime_config, code_analysis)?;

        Ok(Self {
            host: Arc::new(std::sync::Mutex::new(host)),
            version: version.to_string(),
            last_timeout_secs: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The Unix-seconds stamp of the most recent timeout (0 = none). Used by
    /// `restart_bridge` to carry the cooldown window across a bridge restart:
    /// a fresh `SemanticBridge` starts with `last_timeout_secs = 0`, but a
    /// hung CLR call from the old bridge may still be in flight (its
    /// `spawn_blocking` thread keeps the old host's `Arc<Mutex>` alive). Losing
    /// the stamp would let the new bridge's first call bypass the cooldown gate.
    pub(crate) fn last_timeout_secs(&self) -> u64 {
        self.last_timeout_secs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Seed this bridge's timeout stamp from a prior bridge generation. Only
    /// advances the stamp (a non-zero prior value), never clears an existing
    /// one, so a restart cannot accidentally shorten an active cooldown.
    pub(crate) fn seed_last_timeout_secs(&self, secs: u64) {
        seed_timeout_stamp(&self.last_timeout_secs, secs);
    }

    fn parse_response<T: serde::de::DeserializeOwned>(
        value: serde_json::Value,
    ) -> Result<T, SemanticError> {
        serde_json::from_value(value).map_err(|e| SemanticError::SerializationError(e.to_string()))
    }

    /// Internal: call with timeout on a blocking thread.
    ///
    /// The `std::sync::Mutex` is acquired inside `spawn_blocking` (a sync context)
    /// to serialize concurrent calls — the CLR response buffer is shared and is
    /// not safe to access from multiple threads simultaneously.
    ///
    /// # Timeout behaviour
    ///
    /// The `tokio::time::timeout` wraps `spawn_blocking`. If the timeout fires
    /// **after** `spawn_blocking` has started (i.e. the Mutex is already held),
    /// the blocking task continues running — the timeout does NOT release the
    /// Mutex lock or interrupt the .NET call. The lock will remain held until
    /// the bridge returns or the process exits.
    ///
    /// T047: when the cooldown window elapses, the gate isn't released
    /// blindly — we first `try_lock()` the Mutex. If the Mutex is still
    /// held (the previous CLR call hasn't returned), we re-stamp
    /// `last_timeout_secs` and keep the gate closed for another
    /// TIMEOUT_COOLDOWN. This prevents an indefinitely-hung CLR call from
    /// being followed by a fresh thundering herd of attempts every 60s.
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, SemanticError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match cooldown_gate(
            &self.last_timeout_secs,
            &self.host,
            now,
            TIMEOUT_COOLDOWN,
            method,
        ) {
            CooldownDecision::Proceed => {}
            CooldownDecision::ShortCircuit(err) => return Err(err),
        }

        let host = self.host.clone();
        let method = method.to_string();

        let result = tokio::time::timeout(
            DEFAULT_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                let mut guard = host.lock().map_err(|_| SemanticError::Poisoned)?;
                guard.call(&method, params)
            }),
        )
        .await;

        match result {
            Ok(Ok(inner)) => inner,
            Ok(Err(join_err)) => Err(SemanticError::HostInit(format!(
                "Bridge call panicked: {join_err}"
            ))),
            Err(_) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                self.last_timeout_secs
                    .store(now, std::sync::atomic::Ordering::Relaxed);
                Err(SemanticError::Timeout(DEFAULT_TIMEOUT))
            }
        }
    }

    pub async fn analyze(
        &self,
        req: AnalyzeRequest,
    ) -> Result<Vec<DiagnosticEntry>, SemanticError> {
        check_text_size(Some(&req.source))?;
        let params = serde_json::to_value(&req)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))?;
        let result = self.call("analyze", params).await?;
        Self::parse_response(result)
    }

    /// Resolve the type of the symbol at the given position.
    ///
    /// **Position contract (F-036):** `pos` is `(line, column)` in the same
    /// 0-based UTF-16 coordinate system that LSP uses. The C# bridge's
    /// `LineColToOffset` walks `cur < line` newlines from the start of the
    /// file, then adds `col` directly to the resulting byte offset, so any
    /// off-by-one done on the Rust side will land at the wrong token. Pass
    /// LSP positions through unchanged.
    ///
    /// **Unsaved-text contract (F-037):** when `text` is `Some`, the bridge
    /// uses that buffer instead of reading `file` from disk. Pass the
    /// document store's current text for open documents so hover answers
    /// reflect the editor buffer, not the last-saved version.
    pub async fn type_at(
        &self,
        file: &Path,
        pos: (u32, u32),
        text: Option<&str>,
    ) -> Result<Option<TypeInfo>, SemanticError> {
        let mut params = serde_json::json!({
            "file": file,
            "line": pos.0,
            "column": pos.1,
        });
        check_text_size(text)?;
        if let Some(t) = text {
            params["text"] = serde_json::Value::String(t.to_string());
        }
        let result = self.call("typeAt", params).await?;
        if result.is_null() {
            return Ok(None);
        }
        let info: TypeInfo = Self::parse_response(result)?;
        Ok(Some(info))
    }

    /// Get completion items at the given position.
    ///
    /// **Position contract (F-036):** identical to [`Self::type_at`] — `pos`
    /// is 0-based UTF-16 `(line, column)`, passed through to the C# bridge
    /// without adjustment.
    ///
    /// **Unsaved-text contract (F-037):** identical to [`Self::type_at`] —
    /// caller-supplied `text` overrides the disk read.
    pub async fn completions_at(
        &self,
        file: &Path,
        pos: (u32, u32),
        text: Option<&str>,
    ) -> Result<Vec<CompletionItem>, SemanticError> {
        let mut params = serde_json::json!({
            "file": file,
            "line": pos.0,
            "column": pos.1,
        });
        check_text_size(text)?;
        if let Some(t) = text {
            params["text"] = serde_json::Value::String(t.to_string());
        }
        let result = self.call("completions", params).await?;
        Self::parse_response(result)
    }

    /// Extract all built-in types and methods from CodeAnalysis.
    ///
    /// Checks disk cache first. On cache miss, calls the bridge and caches the result.
    pub async fn builtin_types(&self) -> Result<Vec<BuiltinType>, SemanticError> {
        if let Some(cached) = super::cache::read_builtins(&self.version) {
            return Ok(cached);
        }

        let result = self.call("builtins", serde_json::Value::Null).await?;
        let types: Vec<BuiltinType> = Self::parse_response(result)?;

        super::cache::write_builtins(&self.version, &types);

        Ok(types)
    }

    /// Set `AL_ERROR_CODES_LIVE=1` for fresh extraction (bypasses disk cache).
    pub async fn error_codes(&self) -> Result<Vec<ErrorCodeInfo>, SemanticError> {
        let live = std::env::var("AL_ERROR_CODES_LIVE").is_ok();

        if !live {
            if let Some(cached) = super::cache::read_error_codes(&self.version) {
                return Ok(cached);
            }
        }

        let result = self.call("errorCodes", serde_json::Value::Null).await?;
        let codes: Vec<ErrorCodeInfo> = Self::parse_response(result)?;

        super::cache::write_error_codes(&self.version, &codes);

        Ok(codes)
    }

    /// Compile an AL project using alc (the Microsoft AL compiler).
    ///
    /// This invokes alc as a subprocess via the .NET bridge, parses the SARIF
    /// error log for structured diagnostics, and returns the path to the .app file.
    pub async fn compile(
        &self,
        project: &Path,
        alc_path: Option<&Path>,
        package_cache: Option<&Path>,
    ) -> Result<CompileResult, SemanticError> {
        let mut params = serde_json::json!({ "project": project });
        if let Some(alc) = alc_path {
            params["alcPath"] = serde_json::Value::String(alc.to_string_lossy().into_owned());
        }
        if let Some(pkg) = package_cache {
            params["packageCachePath"] =
                serde_json::Value::String(pkg.to_string_lossy().into_owned());
        }
        let result = self.call("compile", params).await?;
        Self::parse_response(result)
    }

    pub async fn ping(&self) -> Result<(), SemanticError> {
        let _ = self.call("ping", serde_json::Value::Null).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_request_serialization() {
        let req = AnalyzeRequest {
            file: PathBuf::from("/src/MyTable.al"),
            source: "table 50100 MyTable { }".to_string(),
            analyzers: vec!["CodeCop".to_string(), "UICop".to_string()],
            package_cache: PathBuf::from("/packages"),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["file"], "/src/MyTable.al");
        assert_eq!(json["analyzers"].as_array().unwrap().len(), 2);
        assert!(json.get("packageCache").is_some());
    }

    #[test]
    fn test_compile_result_deserialization() {
        let json = serde_json::json!({
            "success": true,
            "diagnostics": [],
            "appPath": "/output/My.app"
        });
        let result: CompileResult = serde_json::from_value(json).unwrap();
        assert!(result.success);
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.app_path.unwrap(), PathBuf::from("/output/My.app"));
    }

    #[test]
    fn test_compile_result_with_diagnostics() {
        let json = serde_json::json!({
            "success": false,
            "diagnostics": [
                {
                    "file": "/src/test.al",
                    "line": 10,
                    "column": 5,
                    "endLine": 10,
                    "endColumn": 15,
                    "severity": "Error",
                    "code": "AL0001",
                    "message": "Syntax error"
                }
            ],
            "appPath": null
        });
        let result: CompileResult = serde_json::from_value(json).unwrap();
        assert!(!result.success);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, "AL0001");
        assert_eq!(result.diagnostics[0].line, 10);
        assert!(result.app_path.is_none());
    }

    #[test]
    fn test_diagnostic_entry_roundtrip() {
        let entry = DiagnosticEntry {
            file: PathBuf::from("/src/test.al"),
            line: 5,
            column: 1,
            end_line: 5,
            end_column: 20,
            severity: "Warning".to_string(),
            code: "AL0118".to_string(),
            message: "Variable is unused".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: DiagnosticEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.code, "AL0118");
        assert_eq!(parsed.severity, "Warning");
    }

    #[test]
    fn test_type_info_deserialization() {
        let json = serde_json::json!({
            "name": "Record",
            "kind": "Table",
            "documentation": "Represents a database record."
        });
        let info: TypeInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info.name, "Record");
        assert_eq!(info.kind, "Table");
        assert_eq!(info.documentation.unwrap(), "Represents a database record.");
    }

    #[test]
    fn test_type_info_without_documentation() {
        let json = serde_json::json!({
            "name": "Integer",
            "kind": "Primitive",
        });
        let info: TypeInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info.name, "Integer");
        assert!(info.documentation.is_none());
    }

    #[test]
    fn test_completion_item_deserialization() {
        let json = serde_json::json!({
            "label": "Message",
            "kind": "Method",
            "detail": "Message(Text)",
            "documentation": "Displays a message to the user."
        });
        let item: CompletionItem = serde_json::from_value(json).unwrap();
        assert_eq!(item.label, "Message");
        assert_eq!(item.kind, "Method");
        assert!(item.detail.is_some());
    }

    #[test]
    fn test_builtin_type_roundtrip() {
        let bt = BuiltinType {
            name: "Text".to_string(),
            methods: vec![
                BuiltinMethod {
                    name: "StrLen".to_string(),
                    parameters: vec![],
                    return_type: Some("Integer".to_string()),
                    documentation: "Returns the length of the string.".to_string(),
                },
                BuiltinMethod {
                    name: "CopyStr".to_string(),
                    parameters: vec![
                        MethodParameter {
                            name: "Position".to_string(),
                            type_name: "Integer".to_string(),
                            is_var: false,
                        },
                        MethodParameter {
                            name: "Length".to_string(),
                            type_name: "Integer".to_string(),
                            is_var: false,
                        },
                    ],
                    return_type: Some("Text".to_string()),
                    documentation: "Copies a substring.".to_string(),
                },
            ],
            enum_values: vec![],
        };
        let json = serde_json::to_string(&bt).unwrap();
        let parsed: BuiltinType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "Text");
        assert_eq!(parsed.methods.len(), 2);
        assert_eq!(parsed.methods[0].name, "StrLen");
        assert_eq!(parsed.methods[1].parameters.len(), 2);
        assert!(!parsed.methods[1].parameters[0].is_var);
    }

    #[test]
    fn test_builtin_type_with_enum_values() {
        let json = r#"{"name":"TextEncoding","methods":[],"enumValues":["MsDos","UTF8","UTF16","Windows"]}"#;
        let parsed: BuiltinType = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.name, "TextEncoding");
        assert_eq!(
            parsed.enum_values,
            vec!["MsDos", "UTF8", "UTF16", "Windows"]
        );
    }

    #[test]
    fn test_error_code_info_roundtrip() {
        let info = ErrorCodeInfo {
            code: "AL0001".to_string(),
            message: "Syntax error".to_string(),
            severity: "Error".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: ErrorCodeInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.code, "AL0001");
    }

    #[test]
    fn test_semantic_error_display() {
        let err = SemanticError::HostInit("dotnet not found".to_string());
        assert_eq!(
            err.to_string(),
            "Failed to initialize .NET host: dotnet not found"
        );

        let err = SemanticError::NotInitialized;
        assert_eq!(err.to_string(), "Bridge not initialized");

        let err = SemanticError::Timeout(Duration::from_secs(30));
        assert_eq!(err.to_string(), "RPC response timed out after 30s");

        let err = SemanticError::RpcError {
            code: -32601,
            message: "Method not found".to_string(),
        };
        assert_eq!(err.to_string(), "RPC error (code -32601): Method not found");

        let err = SemanticError::SerializationError("invalid json".to_string());
        assert_eq!(err.to_string(), "Serialization error: invalid json");
    }

    #[test]
    fn test_semantic_error_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let err = SemanticError::IoError(io_err);
        assert!(err.to_string().contains("IO error"));
    }

    #[test]
    fn test_method_parameter_is_var() {
        let param = MethodParameter {
            name: "Result".to_string(),
            type_name: "Text".to_string(),
            is_var: true,
        };
        let json = serde_json::to_value(&param).unwrap();
        assert_eq!(json["isVar"], true);

        let parsed: MethodParameter = serde_json::from_value(json).unwrap();
        assert!(parsed.is_var);
    }

    #[test]
    fn test_analyze_request_empty_analyzers() {
        let req = AnalyzeRequest {
            file: PathBuf::from("test.al"),
            source: String::new(),
            analyzers: vec![],
            package_cache: PathBuf::from(".alpackages"),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json["analyzers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_check_text_size_none_ok() {
        assert!(check_text_size(None).is_ok());
    }

    #[test]
    fn test_check_text_size_small_ok() {
        assert!(check_text_size(Some("codeunit 50000 Foo { }")).is_ok());
    }

    #[test]
    fn test_check_text_size_at_limit_ok() {
        // Exactly MAX_TEXT_BYTES is accepted (the check is strictly greater-than).
        let buf = "a".repeat(MAX_TEXT_BYTES);
        assert!(check_text_size(Some(&buf)).is_ok());
    }

    #[test]
    fn test_check_text_size_over_limit_rejected() {
        let buf = "a".repeat(MAX_TEXT_BYTES + 1);
        match check_text_size(Some(&buf)) {
            Err(SemanticError::InputTooLarge { size, max }) => {
                assert_eq!(size, MAX_TEXT_BYTES + 1);
                assert_eq!(max, MAX_TEXT_BYTES);
            }
            other => panic!("expected InputTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn test_analyze_source_over_limit_rejected() {
        // analyze() guards req.source with check_text_size before serialising
        // the buffer into the CLR call — exactly as type_at/completions_at guard
        // their `text`. Constructing a live SemanticBridge needs the CLR, so we
        // assert the same guard the method applies to its `source` field. An
        // oversized editor buffer (diagnostics.rs feeds unsanitised text here)
        // must be rejected with InputTooLarge, not forwarded to the bridge.
        let req = AnalyzeRequest {
            file: std::path::PathBuf::from("/tmp/Over.al"),
            source: "a".repeat(MAX_TEXT_BYTES + 1),
            analyzers: Vec::new(),
            package_cache: std::path::PathBuf::from("/tmp/.alpackages"),
        };
        match check_text_size(Some(&req.source)) {
            Err(SemanticError::InputTooLarge { size, max }) => {
                assert_eq!(size, MAX_TEXT_BYTES + 1);
                assert_eq!(max, MAX_TEXT_BYTES);
            }
            other => panic!("expected InputTooLarge for analyze source, got {other:?}"),
        }
        let ok_req = AnalyzeRequest {
            source: "codeunit 50000 Foo { }".to_string(),
            ..req
        };
        assert!(check_text_size(Some(&ok_req.source)).is_ok());
    }

    #[test]
    fn test_input_too_large_error_display() {
        let err = SemanticError::InputTooLarge { size: 99, max: 16 };
        let msg = err.to_string();
        assert!(msg.contains("99"), "message should contain size: {msg}");
        assert!(msg.contains("16"), "message should contain max: {msg}");
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    const COOLDOWN: Duration = Duration::from_secs(60);

    #[test]
    fn test_cooldown_gate_no_prior_timeout_proceeds() {
        let stamp = AtomicU64::new(0);
        let host = Mutex::new(());
        let decision = cooldown_gate(&stamp, &host, 1_000, COOLDOWN, "typeAt");
        assert!(matches!(decision, CooldownDecision::Proceed));
        assert_eq!(stamp.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_cooldown_gate_inside_window_short_circuits_without_probe() {
        // A timeout fired 10s ago; cooldown is 60s. Still inside the window:
        // short-circuit WITHOUT probing the lock. We prove the lock is never
        // probed by holding it for the duration of the call — a probe would
        // observe WouldBlock and produce the "hung call" message instead.
        let stamp = AtomicU64::new(1_000);
        let host = Mutex::new(());
        let _held = host.lock().unwrap();
        let decision = cooldown_gate(&stamp, &host, 1_010, COOLDOWN, "typeAt");
        match decision {
            CooldownDecision::ShortCircuit(SemanticError::Cooldown(msg)) => {
                assert_eq!(msg, "cooldown window after timeout");
            }
            other => panic!("expected in-window Cooldown, got {other:?}"),
        }
        assert_eq!(stamp.load(Ordering::Relaxed), 1_000);
    }

    #[test]
    fn test_cooldown_gate_elapsed_lock_free_resumes_and_clears() {
        let stamp = AtomicU64::new(1_000);
        let host = Mutex::new(());
        let decision = cooldown_gate(&stamp, &host, 1_070, COOLDOWN, "typeAt");
        assert!(matches!(decision, CooldownDecision::Proceed));
        assert_eq!(stamp.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_cooldown_gate_elapsed_lock_held_extends_cooldown() {
        let stamp = AtomicU64::new(1_000);
        let host = Mutex::new(());
        let _held = host.lock().unwrap();
        let now = 1_070;
        let decision = cooldown_gate(&stamp, &host, now, COOLDOWN, "typeAt");
        match decision {
            CooldownDecision::ShortCircuit(SemanticError::Cooldown(msg)) => {
                assert_eq!(msg, "hung call still holding the lock");
            }
            other => panic!("expected hung-lock Cooldown, got {other:?}"),
        }
        assert_eq!(stamp.load(Ordering::Relaxed), now);
    }

    #[test]
    fn test_cooldown_gate_elapsed_lock_poisoned_extends_cooldown() {
        let stamp = AtomicU64::new(1_000);
        let host = Mutex::new(());
        let _ = std::panic::catch_unwind(|| {
            let _g = host.lock().unwrap();
            panic!("poison the mutex");
        });
        assert!(host.is_poisoned(), "mutex should be poisoned by the panic");
        let now = 1_070;
        let decision = cooldown_gate(&stamp, &host, now, COOLDOWN, "typeAt");
        assert!(matches!(
            decision,
            CooldownDecision::ShortCircuit(SemanticError::Poisoned)
        ));
        assert_eq!(stamp.load(Ordering::Relaxed), now);
    }

    #[test]
    fn test_cooldown_gate_held_lock_recovers_after_release() {
        // Full race-recovery sequence on one stamp+mutex pair:
        //  1. cooldown elapsed but lock held -> extend (re-stamp to now)
        //  2. still within the extended window -> short-circuit
        //  3. lock released + window elapsed -> proceed and clear
        let stamp = AtomicU64::new(1_000);
        let host = Mutex::new(());

        {
            let _held = host.lock().unwrap();
            let d1 = cooldown_gate(&stamp, &host, 1_070, COOLDOWN, "typeAt");
            assert!(matches!(d1, CooldownDecision::ShortCircuit(_)));
            assert_eq!(stamp.load(Ordering::Relaxed), 1_070);

            let d2 = cooldown_gate(&stamp, &host, 1_100, COOLDOWN, "typeAt");
            assert!(matches!(d2, CooldownDecision::ShortCircuit(_)));
        }
        let d3 = cooldown_gate(&stamp, &host, 1_140, COOLDOWN, "typeAt");
        assert!(matches!(d3, CooldownDecision::Proceed));
        assert_eq!(stamp.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_seed_timeout_stamp_carries_nonzero_forward() {
        // Restart path: a brand-new bridge starts at 0, but a hung call from
        // the prior generation left a cooldown stamp. Seeding must carry that
        // stamp forward so the new bridge's first call still hits the gate.
        let fresh = AtomicU64::new(0);
        seed_timeout_stamp(&fresh, 1_000);
        assert_eq!(fresh.load(Ordering::Relaxed), 1_000);
    }

    #[test]
    fn test_seed_timeout_stamp_zero_is_noop() {
        // A healthy old bridge (no recent timeout) must not clobber the new
        // bridge's stamp — seeding 0 is a no-op so we never shorten/erase an
        // active cooldown that may have been set in the meantime.
        let existing = AtomicU64::new(1_000);
        seed_timeout_stamp(&existing, 0);
        assert_eq!(existing.load(Ordering::Relaxed), 1_000);
    }

    #[test]
    fn test_seed_timeout_stamp_only_advances_via_caller_guard() {
        // The free function itself unconditionally stores a non-zero value;
        // restart_bridge only calls it with the prior stamp, so the net effect
        // is "carry forward the prior cooldown". Document that a non-zero seed
        // overwrites whatever was there (the new bridge starts at 0 anyway).
        let new_bridge = AtomicU64::new(0);
        seed_timeout_stamp(&new_bridge, 1_234);
        assert_eq!(new_bridge.load(Ordering::Relaxed), 1_234);
    }

    #[test]
    fn test_parse_response_ok_maps_through() {
        let value = serde_json::json!({
            "success": true,
            "diagnostics": [],
            "appPath": "/out/My.app"
        });
        let parsed: CompileResult =
            SemanticBridge::parse_response(value).expect("valid payload should decode");
        assert!(parsed.success);
        assert_eq!(parsed.app_path.unwrap(), PathBuf::from("/out/My.app"));
    }

    #[test]
    fn test_parse_response_type_mismatch_is_serialization_error() {
        let value = serde_json::json!({
            "success": "yes",          // wrong type
            "diagnostics": [],
            "appPath": null
        });
        match SemanticBridge::parse_response::<CompileResult>(value) {
            Err(SemanticError::SerializationError(msg)) => {
                assert!(!msg.is_empty(), "serde message should be propagated");
            }
            other => panic!("expected SerializationError, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_response_missing_required_field_is_serialization_error() {
        // `diagnostics` is a required (non-defaulted) field of CompileResult.
        // Its absence is a decode failure that must surface as
        // SerializationError, mirroring a malformed bridge response.
        let value = serde_json::json!({ "success": true, "appPath": null });
        assert!(matches!(
            SemanticBridge::parse_response::<CompileResult>(value),
            Err(SemanticError::SerializationError(_))
        ));
    }

    #[test]
    fn test_parse_response_vec_of_diagnostics() {
        let value = serde_json::json!([
            {
                "file": "/src/a.al", "line": 1, "column": 2,
                "endLine": 1, "endColumn": 9,
                "severity": "Error", "code": "AL0001", "message": "boom"
            }
        ]);
        let parsed: Vec<DiagnosticEntry> =
            SemanticBridge::parse_response(value).expect("array should decode");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].code, "AL0001");
    }

    #[test]
    fn test_method_parameter_display_by_value() {
        let p = MethodParameter {
            name: "Position".to_string(),
            type_name: "Integer".to_string(),
            is_var: false,
        };
        assert_eq!(p.to_string(), "Position: Integer");
    }

    #[test]
    fn test_method_parameter_display_by_reference() {
        let p = MethodParameter {
            name: "Result".to_string(),
            type_name: "Text".to_string(),
            is_var: true,
        };
        assert_eq!(p.to_string(), "var Result: Text");
    }

    #[test]
    fn test_builtin_method_defaults_when_fields_omitted() {
        let value = serde_json::json!({ "name": "StrLen" });
        let m: BuiltinMethod =
            serde_json::from_value(value).expect("sparse method should decode via defaults");
        assert_eq!(m.name, "StrLen");
        assert!(m.parameters.is_empty());
        assert!(m.return_type.is_none());
        assert_eq!(m.documentation, "");
    }

    #[test]
    fn test_method_parameter_defaults_when_fields_omitted() {
        let m: MethodParameter =
            serde_json::from_value(serde_json::json!({})).expect("empty param should decode");
        assert_eq!(m.name, "");
        assert_eq!(m.type_name, "");
        assert!(!m.is_var);
        assert_eq!(m.to_string(), ": ");
    }

    #[test]
    fn test_builtin_type_defaults_when_collections_omitted() {
        let bt: BuiltinType = serde_json::from_value(serde_json::json!({ "name": "Boolean" }))
            .expect("sparse builtin type should decode");
        assert_eq!(bt.name, "Boolean");
        assert!(bt.methods.is_empty());
        assert!(bt.enum_values.is_empty());
    }

    #[test]
    fn test_compile_result_missing_app_path_defaults_to_none() {
        let value = serde_json::json!({ "success": true, "diagnostics": [] });
        let r: CompileResult =
            serde_json::from_value(value).expect("missing appPath should decode to None");
        assert!(r.app_path.is_none());
    }

    #[test]
    fn test_cooldown_and_poisoned_error_display() {
        let c = SemanticError::Cooldown("hung call still holding the lock");
        assert!(c.to_string().contains("cooldown"), "cooldown display: {c}");
        assert!(c.to_string().contains("hung call still holding the lock"));

        let p = SemanticError::Poisoned;
        assert!(p.to_string().contains("poisoned"), "poisoned display: {p}");
    }
}
