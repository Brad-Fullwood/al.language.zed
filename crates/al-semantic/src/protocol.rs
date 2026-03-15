//! JSON-RPC message types for .NET bridge communication.
//!
//! The bridge communicates via line-delimited JSON over stdin/stdout.
//! Each message is a single JSON object terminated by a newline.

use serde::{Deserialize, Serialize};

/// A JSON-RPC request.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// A JSON-RPC response.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// A JSON-RPC error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RPC error {}: {}", self.code, self.message)
    }
}

/// Standard JSON-RPC error codes plus bridge-specific ones.
pub mod error_codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    pub const CODE_ANALYSIS_ERROR: i32 = -32000;
    pub const FILE_NOT_FOUND: i32 = -32001;
}
