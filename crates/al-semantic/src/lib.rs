//! .NET bridge for CodeAnalysis API — semantic analysis, analyzers, compilation.
//!
//! Hosts the .NET CLR in-process via `netcorehost` and communicates with a thin
//! C# bridge DLL using JSON-in/JSON-out over function pointers. No subprocess.
//!
//! Architecture: Rust → netcorehost → Bridge.dll → CodeAnalysis.dll

pub mod cache;
pub mod host;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::host::DotNetHost;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Request to analyze a file with CodeAnalysis analyzers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeRequest {
    pub file: PathBuf,
    pub source: String,
    pub analyzers: Vec<String>,
    pub package_cache: PathBuf,
}

/// Result of a compilation invocation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileResult {
    pub success: bool,
    pub diagnostics: Vec<DiagnosticEntry>,
    pub app_path: Option<PathBuf>,
}

/// A single diagnostic from compilation or analysis.
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
#[serde(rename_all = "camelCase")]
pub struct BuiltinType {
    pub name: String,
    #[serde(default)]
    pub methods: Vec<BuiltinMethod>,
    #[serde(default)]
    pub enum_values: Vec<String>,
}

/// A method on a built-in type.
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

/// A method parameter.
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
        if self.is_var { write!(f, "var ")?; }
        write!(f, "{}: {}", self.name, self.type_name)
    }
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
}

// ---------------------------------------------------------------------------
// SemanticBridge
// ---------------------------------------------------------------------------

/// Async bridge to the .NET CodeAnalysis API.
///
/// Uses in-process .NET hosting via `netcorehost`. The .NET runtime is loaded
/// once and stays alive for the lifetime of the bridge. All calls go through
/// function pointers — no subprocess, no JSON-RPC, no stdio.
///
/// Thread-safe: can be shared across tokio tasks via `Arc`.
pub struct SemanticBridge {
    host: Arc<DotNetHost>,
    version: String,
    timeout: Duration,
}

/// Default timeout for bridge calls.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

impl SemanticBridge {
    /// Initialize the .NET bridge with explicit paths.
    ///
    /// - `code_analysis`: path to `Microsoft.Dynamics.Nav.CodeAnalysis.dll`
    /// - `version`: toolchain version string (used for cache keying)
    ///
    /// This loads the CLR in-process and initializes the bridge DLL.
    pub fn new(code_analysis: &Path, version: &str) -> Result<Self, SemanticError> {
        let (bridge_dll, runtime_config) = host::find_bridge_dll()?;
        let host = DotNetHost::new(&bridge_dll, &runtime_config, code_analysis)?;

        Ok(Self {
            host: Arc::new(host),
            version: version.to_string(),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// The toolchain version this bridge was initialized with.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Deserialize a bridge response value, converting JSON errors to [`SemanticError`].
    fn parse_response<T: serde::de::DeserializeOwned>(
        value: serde_json::Value,
    ) -> Result<T, SemanticError> {
        serde_json::from_value(value)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))
    }

    /// Internal: call with timeout on a blocking thread.
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, SemanticError> {
        let host = self.host.clone();
        let method = method.to_string();
        let timeout = self.timeout;

        // Run the .NET call on a blocking thread to avoid blocking the tokio runtime
        let result = tokio::time::timeout(timeout, tokio::task::spawn_blocking(move || {
            host.call(&method, params)
        }))
        .await;

        match result {
            Ok(Ok(inner)) => inner,
            Ok(Err(join_err)) => Err(SemanticError::HostInit(format!(
                "Bridge call panicked: {join_err}"
            ))),
            Err(_) => Err(SemanticError::Timeout(timeout)),
        }
    }

    /// Run CodeAnalysis analyzers on a source file.
    pub async fn analyze(
        &self,
        req: AnalyzeRequest,
    ) -> Result<Vec<DiagnosticEntry>, SemanticError> {
        let params = serde_json::to_value(&req)
            .map_err(|e| SemanticError::SerializationError(e.to_string()))?;
        let result = self.call("analyze", params).await?;
        Self::parse_response(result)
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
        let info: TypeInfo = Self::parse_response(result)?;
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
        Self::parse_response(result)
    }

    /// Extract all built-in types and methods from CodeAnalysis.
    ///
    /// Checks disk cache first. On cache miss, calls the bridge and caches the result.
    pub async fn builtin_types(&self) -> Result<Vec<BuiltinType>, SemanticError> {
        // Check cache
        if let Some(cached) = cache::read_builtins(&self.version) {
            return Ok(cached);
        }

        let result = self.call("builtins", serde_json::Value::Null).await?;
        let types: Vec<BuiltinType> = Self::parse_response(result)?;

        // Cache for next time
        cache::write_builtins(&self.version, &types);

        Ok(types)
    }

    /// List all compiler error codes from CodeAnalysis.
    ///
    /// Default: cached. Set `AL_ERROR_CODES_LIVE=1` for fresh extraction.
    pub async fn error_codes(&self) -> Result<Vec<ErrorCodeInfo>, SemanticError> {
        let live = std::env::var("AL_ERROR_CODES_LIVE").is_ok();

        if !live {
            if let Some(cached) = cache::read_error_codes(&self.version) {
                return Ok(cached);
            }
        }

        let result = self.call("errorCodes", serde_json::Value::Null).await?;
        let codes: Vec<ErrorCodeInfo> = Self::parse_response(result)?;

        cache::write_error_codes(&self.version, &codes);

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

    /// Health check — verifies the .NET bridge is responsive.
    pub async fn ping(&self) -> Result<(), SemanticError> {
        let _ = self.call("ping", serde_json::Value::Null).await?;
        Ok(())
    }

    /// Graceful shutdown. The CLR is cleaned up when DotNetHost is dropped.
    pub async fn shutdown(self) {
        // Nothing to do — CLR shuts down with the DotNetHost drop.
        // This method exists for API compatibility.
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        // Verify camelCase serialization
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

    // -- Error tests --

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
}
