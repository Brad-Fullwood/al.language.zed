//! DAP message types.
//!
//! Implements the Debug Adapter Protocol message format used between
//! Zed (the DAP client) and the al-dap server over stdin/stdout.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Base message types
// ---------------------------------------------------------------------------

/// A DAP request from the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapRequest {
    pub seq: u64,
    #[serde(rename = "type")]
    pub type_: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
}

/// A DAP response sent back to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapResponse {
    pub seq: u64,
    #[serde(rename = "type")]
    pub type_: String,
    pub request_seq: u64,
    pub success: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

/// A DAP event sent to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapEvent {
    pub seq: u64,
    #[serde(rename = "type")]
    pub type_: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

/// A raw DAP message — used for initial parsing before we know the type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapMessage {
    pub seq: u64,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(flatten)]
    pub rest: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// Server capabilities returned in the `initialize` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub supports_configuration_done_request: bool,
    #[serde(default)]
    pub supports_evaluate_for_hovers: bool,
    #[serde(default)]
    pub supports_step_back: bool,
    #[serde(default)]
    pub supports_set_variable: bool,
    #[serde(default)]
    pub supports_restart_request: bool,
    #[serde(default)]
    pub supports_conditional_breakpoints: bool,
    #[serde(default)]
    pub supports_hit_conditional_breakpoints: bool,
    #[serde(default)]
    pub supports_log_points: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            supports_configuration_done_request: true,
            supports_evaluate_for_hovers: true,
            supports_step_back: false,
            supports_set_variable: true,
            supports_restart_request: false,
            supports_conditional_breakpoints: true,
            supports_hit_conditional_breakpoints: true,
            supports_log_points: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Launch configuration
// ---------------------------------------------------------------------------

/// Launch configuration from the client (equivalent to launch.json settings).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlLaunchConfig {
    /// BC server URL (e.g., "http://localhost"). Empty for cloud.
    #[serde(default)]
    pub server: String,
    /// BC server instance name (e.g., "BC"). Empty for cloud.
    #[serde(default)]
    pub server_instance: String,
    /// Tenant name/ID.
    #[serde(default = "default_tenant")]
    pub tenant: String,
    /// Authentication method: "UserPassword" or "AAD".
    #[serde(default)]
    pub authentication: String,
    /// Environment type: "OnPrem", "Sandbox", or "Production".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_type: Option<String>,
    /// Environment name for cloud (e.g., "sandbox", "production").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_name: Option<String>,
    /// Break on error: "None", "All", or "Unhandled".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakpoint_on_error: Option<String>,
    /// Break on record write: "None", "All".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_on_record_write: Option<String>,
    /// Whether to launch a browser session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_browser: Option<bool>,
    /// Startup object ID (e.g., page 22 for Customer List).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_object_id: Option<u64>,
    /// Startup object type (e.g., "Page").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_object_type: Option<String>,
    /// Enable SQL information in debugger.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_sql_information_debugger: Option<bool>,
    /// Enable long running SQL statement tracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_long_running_sql_statements: Option<bool>,
    /// Threshold in ms for long running SQL statements.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_running_sql_statements_threshold: Option<u64>,
    /// Number of SQL statements to collect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_of_sql_statements: Option<u64>,
}

impl AlLaunchConfig {
    /// Whether this is a cloud (SaaS) configuration.
    pub fn is_cloud(&self) -> bool {
        matches!(
            self.environment_type.as_deref(),
            Some("Sandbox" | "Production")
        ) || self.environment_name.is_some()
    }

    /// Effective authentication method (defaults to AAD for cloud, UserPassword for on-prem).
    pub fn effective_authentication(&self) -> &str {
        if !self.authentication.is_empty() {
            &self.authentication
        } else if self.is_cloud() {
            "AAD"
        } else {
            "UserPassword"
        }
    }
}

fn default_tenant() -> String {
    "default".to_string()
}

// ---------------------------------------------------------------------------
// Breakpoint types
// ---------------------------------------------------------------------------

/// A breakpoint set in source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBreakpoint {
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
}

/// A breakpoint as returned by the server (with verified status).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakpoint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
}

// ---------------------------------------------------------------------------
// Thread / Stack / Variable types
// ---------------------------------------------------------------------------

/// A thread in the debugged process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: u64,
    pub name: String,
}

/// A stack frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    pub line: u32,
    pub column: u32,
}

/// A scope within a stack frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub name: String,
    pub variables_reference: u64,
    pub expensive: bool,
}

/// A variable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    pub variables_reference: u64,
}

/// A source file reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

// ---------------------------------------------------------------------------
// SetBreakpointsArguments
// ---------------------------------------------------------------------------

/// Arguments for the `setBreakpoints` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBreakpointsArguments {
    pub source: Source,
    #[serde(default)]
    pub breakpoints: Vec<SourceBreakpoint>,
}

// ---------------------------------------------------------------------------
// Framing: Content-Length based reading/writing
// ---------------------------------------------------------------------------

use std::io;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Read a single DAP message from a Content-Length framed stream.
///
/// DAP uses the same framing as LSP:
/// ```text
/// Content-Length: <length>\r\n
/// \r\n
/// <JSON payload>
/// ```
pub async fn read_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<serde_json::Value, io::Error> {
    // Read headers
    let mut content_length: Option<usize> = None;

    loop {
        let mut header_line = String::new();
        let bytes_read = reader.read_line(&mut header_line).await?;
        if bytes_read == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF while reading DAP header"));
        }

        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            // Empty line marks end of headers
            break;
        }

        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            );
        }
        // Ignore unknown headers (e.g., Content-Type)
    }

    let length = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing Content-Length header"))?;

    // Read exactly `length` bytes of JSON payload
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;

    serde_json::from_slice(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Invalid JSON: {e}")))
}

/// Write a single DAP message with Content-Length framing.
pub async fn write_message<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    value: &serde_json::Value,
) -> Result<(), io::Error> {
    let body = serde_json::to_string(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Encode a DAP message into a Content-Length framed byte buffer (sync, for testing).
pub fn encode_message(value: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_string(value).expect("failed to serialize DAP message");
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut buf = Vec::with_capacity(header.len() + body.len());
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(body.as_bytes());
    buf
}

// ---------------------------------------------------------------------------
// JSON-RPC types (for communication with the .NET bridge process)
// ---------------------------------------------------------------------------

pub use al_discovery::jsonrpc::{Request as BridgeRequest, Response as BridgeResponse, RpcError};
