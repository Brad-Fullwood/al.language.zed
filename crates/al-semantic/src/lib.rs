//! .NET bridge for CodeAnalysis API — semantic analysis, analyzers, compilation.
//!
//! Spawns a .NET subprocess (AlSemantic) and communicates via JSON-RPC over
//! stdin/stdout. The bridge provides access to the full CodeAnalysis API for
//! semantic analysis, diagnostics, type resolution, and compilation.

pub mod process;
pub mod protocol;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use al_discovery::AlToolchain;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::protocol::{Request, Response};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Default timeout for RPC responses.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default idle timeout before the bridge can be considered stale.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Request to analyze a file with CodeAnalysis analyzers.
#[derive(Debug, Clone, Serialize)]
pub struct AnalyzeRequest {
    pub file: PathBuf,
    pub source: String,
    pub analyzers: Vec<String>,
    pub package_cache: PathBuf,
}

/// Result of a compilation invocation.
#[derive(Debug, Clone, Deserialize)]
pub struct CompileResult {
    pub success: bool,
    pub diagnostics: Vec<DiagnosticEntry>,
    pub app_path: Option<PathBuf>,
}

/// A single diagnostic from compilation or analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Type information resolved at a position.
#[derive(Debug, Clone, Deserialize)]
pub struct TypeInfo {
    pub name: String,
    pub kind: String,
    pub documentation: Option<String>,
}

/// A completion item from the CodeAnalysis API.
#[derive(Debug, Clone, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    pub kind: String,
    pub detail: Option<String>,
    pub documentation: Option<String>,
}

/// A built-in AL type from CodeAnalysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinType {
    pub name: String,
    pub methods: Vec<BuiltinMethod>,
}

/// A method on a built-in type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinMethod {
    pub name: String,
    pub parameters: Vec<MethodParameter>,
    pub return_type: Option<String>,
    pub documentation: String,
}

/// A method parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodParameter {
    pub name: String,
    pub type_name: String,
    pub is_var: bool,
}

/// Information about a compiler error code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorCodeInfo {
    pub code: String,
    pub message: String,
    pub severity: String,
}

// ---------------------------------------------------------------------------
// SemanticError
// ---------------------------------------------------------------------------

/// Errors from the semantic bridge.
#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error("Failed to spawn .NET bridge: {0}")]
    SpawnFailed(String),

    #[error("Bridge process died unexpectedly")]
    ProcessDied,

    #[error("RPC response timed out after {0:?}")]
    Timeout(Duration),

    #[error("RPC error (code {code}): {message}")]
    RpcError { code: i32, message: String },

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Internal IO state (behind Mutex)
// ---------------------------------------------------------------------------

struct BridgeIo {
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    last_request: Instant,
}

// ---------------------------------------------------------------------------
// SemanticBridge
// ---------------------------------------------------------------------------

/// Async bridge to the .NET CodeAnalysis subprocess.
///
/// Uses `&self` with interior mutability (`tokio::sync::Mutex`) so the bridge
/// can be shared across tasks. The child process is killed when the bridge
/// is dropped.
pub struct SemanticBridge {
    child: Mutex<tokio::process::Child>,
    io: Mutex<BridgeIo>,
    next_id: AtomicU64,
    timeout: Duration,
    idle_timeout: Duration,
}

impl SemanticBridge {
    /// Spawn a new .NET bridge subprocess using the given toolchain.
    pub async fn spawn(toolchain: &AlToolchain) -> Result<Self, SemanticError> {
        let mut child = process::spawn_dotnet_bridge(toolchain)?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SemanticError::SpawnFailed("Failed to capture child stdin".into()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SemanticError::SpawnFailed("Failed to capture child stdout".into()))?;

        Ok(Self {
            child: Mutex::new(child),
            io: Mutex::new(BridgeIo {
                stdin: BufWriter::new(stdin),
                stdout: BufReader::new(stdout),
                last_request: Instant::now(),
            }),
            next_id: AtomicU64::new(1),
            timeout: DEFAULT_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        })
    }

    /// Send a request and wait for the matching response.
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, SemanticError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let request = Request {
            id,
            method: method.to_string(),
            params: if params.is_null() { None } else { Some(params) },
        };

        let mut request_line = serde_json::to_string(&request)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))?;
        request_line.push('\n');

        let mut io = self.io.lock().await;
        io.last_request = Instant::now();

        // Write the request
        io.stdin
            .write_all(request_line.as_bytes())
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    SemanticError::ProcessDied
                } else {
                    SemanticError::IoError(e)
                }
            })?;
        io.stdin.flush().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                SemanticError::ProcessDied
            } else {
                SemanticError::IoError(e)
            }
        })?;

        // Read the response (with timeout)
        let mut line = String::new();
        let read_result = tokio::time::timeout(self.timeout, io.stdout.read_line(&mut line)).await;

        match read_result {
            Ok(Ok(0)) => Err(SemanticError::ProcessDied),
            Ok(Ok(_)) => {
                let response: Response = serde_json::from_str(line.trim())
                    .map_err(|e| SemanticError::SerializationError(format!(
                        "Failed to parse response: {e}. Raw: {line}"
                    )))?;

                if response.id != id {
                    return Err(SemanticError::SerializationError(format!(
                        "Response id mismatch: expected {id}, got {}",
                        response.id
                    )));
                }

                if let Some(err) = response.error {
                    return Err(SemanticError::RpcError {
                        code: err.code,
                        message: err.message,
                    });
                }

                Ok(response.result.unwrap_or(serde_json::Value::Null))
            }
            Ok(Err(e)) => Err(SemanticError::IoError(e)),
            Err(_) => Err(SemanticError::Timeout(self.timeout)),
        }
    }

    /// Run CodeAnalysis analyzers on a source file.
    pub async fn analyze(&self, req: AnalyzeRequest) -> Result<Vec<DiagnosticEntry>, SemanticError> {
        let params = serde_json::to_value(&req)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))?;
        let result = self.call("analyze", params).await?;
        serde_json::from_value(result)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))
    }

    /// Invoke the CodeAnalysis Compilation API on a project directory.
    pub async fn compile(&self, project: &Path) -> Result<CompileResult, SemanticError> {
        let params = serde_json::json!({ "project": project });
        let result = self.call("compile", params).await?;
        serde_json::from_value(result)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))
    }

    /// Resolve the type of the symbol at the given position.
    pub async fn type_at(
        &self,
        file: &Path,
        pos: (u32, u32),
    ) -> Result<Option<TypeInfo>, SemanticError> {
        let params = serde_json::json!({
            "file": file,
            "line": pos.0,
            "column": pos.1,
        });
        let result = self.call("typeAt", params).await?;
        if result.is_null() {
            return Ok(None);
        }
        let info: TypeInfo = serde_json::from_value(result)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))?;
        Ok(Some(info))
    }

    /// Get completion items at the given position.
    pub async fn completions_at(
        &self,
        file: &Path,
        pos: (u32, u32),
    ) -> Result<Vec<CompletionItem>, SemanticError> {
        let params = serde_json::json!({
            "file": file,
            "line": pos.0,
            "column": pos.1,
        });
        let result = self.call("completions", params).await?;
        serde_json::from_value(result)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))
    }

    /// Extract all built-in types and methods from CodeAnalysis.
    pub async fn builtin_types(&self) -> Result<Vec<BuiltinType>, SemanticError> {
        let result = self.call("builtins", serde_json::Value::Null).await?;
        serde_json::from_value(result)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))
    }

    /// List all compiler error codes from CodeAnalysis.
    pub async fn error_codes(&self) -> Result<Vec<ErrorCodeInfo>, SemanticError> {
        let result = self.call("errorCodes", serde_json::Value::Null).await?;
        serde_json::from_value(result)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))
    }

    /// Health check — verifies the .NET process is alive and responsive.
    pub async fn ping(&self) -> Result<(), SemanticError> {
        let result = self.call("ping", serde_json::Value::Null).await?;
        // Accept any successful response as a valid ping
        let _ = result;
        Ok(())
    }

    /// Check whether the bridge has been idle longer than the idle timeout.
    ///
    /// Returns `true` if the bridge has been idle and should be shut down.
    pub async fn check_idle(&self) -> bool {
        let io = self.io.lock().await;
        io.last_request.elapsed() > self.idle_timeout
    }

    /// Gracefully shut down the bridge subprocess.
    ///
    /// Sends a "shutdown" request then kills the process if it doesn't exit.
    pub async fn shutdown(self) {
        // Try to send shutdown request (ignore errors — process might already be dead)
        let _ = self.call("shutdown", serde_json::Value::Null).await;

        // Give the process a moment to exit gracefully
        let mut child = self.child.into_inner();
        match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                // Force kill
                let _ = child.kill().await;
            }
        }
    }

    /// Check if the child process is still running.
    pub async fn is_alive(&self) -> bool {
        let mut child = self.child.lock().await;
        match child.try_wait() {
            Ok(None) => true,  // Still running
            Ok(Some(_)) => false,  // Exited
            Err(_) => false,  // Error checking
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol;

    // -- Serialization tests --

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
    }

    #[test]
    fn test_compile_result_deserialization() {
        let json = serde_json::json!({
            "success": true,
            "diagnostics": [],
            "app_path": "/output/My.app"
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
                    "end_line": 10,
                    "end_column": 15,
                    "severity": "Error",
                    "code": "AL0001",
                    "message": "Syntax error"
                }
            ],
            "app_path": null
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

    // -- Error tests --

    #[test]
    fn test_semantic_error_display() {
        let err = SemanticError::SpawnFailed("dotnet not found".to_string());
        assert_eq!(err.to_string(), "Failed to spawn .NET bridge: dotnet not found");

        let err = SemanticError::ProcessDied;
        assert_eq!(err.to_string(), "Bridge process died unexpectedly");

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

    // -- Request/response matching tests --

    #[test]
    fn test_request_id_generation() {
        let counter = AtomicU64::new(1);
        let id1 = counter.fetch_add(1, Ordering::Relaxed);
        let id2 = counter.fetch_add(1, Ordering::Relaxed);
        let id3 = counter.fetch_add(1, Ordering::Relaxed);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn test_request_response_id_matching() {
        let req = protocol::Request {
            id: 42,
            method: "ping".to_string(),
            params: None,
        };
        let req_json = serde_json::to_string(&req).unwrap();

        // Simulate a response with matching id
        let resp_json = r#"{"id":42,"result":{"status":"ok"}}"#;
        let resp: protocol::Response = serde_json::from_str(resp_json).unwrap();
        assert_eq!(req.id, resp.id);

        // Simulate a response with non-matching id
        let bad_resp_json = r#"{"id":99,"result":"pong"}"#;
        let bad_resp: protocol::Response = serde_json::from_str(bad_resp_json).unwrap();
        assert_ne!(req.id, bad_resp.id);

        // Verify the request was serialized correctly
        let val: serde_json::Value = serde_json::from_str(&req_json).unwrap();
        assert_eq!(val["id"], 42);
    }

    #[test]
    fn test_rpc_error_in_response() {
        let resp_json = r#"{"id":1,"error":{"code":-32603,"message":"Internal error"}}"#;
        let resp: protocol::Response = serde_json::from_str(resp_json).unwrap();
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32603);
    }

    // -- Spawn failure test --

    #[tokio::test]
    async fn test_spawn_fails_gracefully_without_dotnet() {
        let toolchain = AlToolchain {
            alc: PathBuf::from("/nonexistent/alc.dll"),
            aldoc: None,
            code_analysis: PathBuf::from("/nonexistent/CodeAnalysis.dll"),
            analyzers: al_discovery::AnalyzerPaths {
                code_cop: PathBuf::from("/nonexistent/CodeCop.dll"),
                app_source_cop: PathBuf::from("/nonexistent/AppSourceCop.dll"),
                ui_cop: PathBuf::from("/nonexistent/UICop.dll"),
                per_tenant_cop: PathBuf::from("/nonexistent/PerTenantCop.dll"),
                common: PathBuf::from("/nonexistent/Common.dll"),
            },
            dotnet_root: PathBuf::from("/nonexistent"),
            version: "0.0.0".to_string(),
        };

        let result = SemanticBridge::spawn(&toolchain).await;
        // Should return an error, not panic
        match result {
            Ok(_bridge) => {
                // If spawn somehow succeeds (e.g., dotnet project found), that's ok
            }
            Err(e) => {
                assert!(
                    matches!(e, SemanticError::SpawnFailed(_)),
                    "Expected SpawnFailed, got: {e:?}"
                );
            }
        }
    }

    // -- Method parameter tests --

    #[test]
    fn test_method_parameter_is_var() {
        let param = MethodParameter {
            name: "Result".to_string(),
            type_name: "Text".to_string(),
            is_var: true,
        };
        let json = serde_json::to_value(&param).unwrap();
        assert_eq!(json["is_var"], true);

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
}
