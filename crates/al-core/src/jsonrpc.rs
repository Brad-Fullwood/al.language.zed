//! Shared JSON-RPC types for .NET bridge subprocess communication.
//!
//! Both `al-semantic` and `al-dap` spawn .NET subprocesses and communicate
//! via line-delimited JSON-RPC over stdin/stdout.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = Request {
            id: 1,
            method: "ping".to_string(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("params"));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"ping\""));
    }

    #[test]
    fn test_response_deserialization_error() {
        let json = r#"{"id":2,"error":{"code":-32601,"message":"Unknown method: foo"}}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, 2);
        let err = resp.error.unwrap();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn test_rpc_error_display() {
        let err = RpcError {
            code: -32601,
            message: "Method not found".to_string(),
        };
        assert_eq!(err.to_string(), "RPC error -32601: Method not found");
    }
}
