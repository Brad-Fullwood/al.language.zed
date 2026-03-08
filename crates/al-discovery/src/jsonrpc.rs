//! Shared JSON-RPC types for .NET bridge subprocess communication.
//!
//! Both `al-semantic` and `al-dap` spawn .NET subprocesses and communicate
//! via line-delimited JSON-RPC over stdin/stdout. This module provides the
//! common request/response types used by both bridges.

use serde::{Deserialize, Serialize};

/// A JSON-RPC request sent to a .NET bridge process.
#[derive(Debug, Serialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A JSON-RPC response from a .NET bridge process.
#[derive(Debug, Deserialize)]
pub struct Response {
    pub id: u64,
    pub result: Option<serde_json::Value>,
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
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i32 = -32700;
    /// The request is not a valid JSON-RPC request.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal error in the .NET bridge.
    pub const INTERNAL_ERROR: i32 = -32603;
    /// CodeAnalysis.dll failed to load a type or method.
    pub const CODE_ANALYSIS_ERROR: i32 = -32000;
    /// The requested file was not found.
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
        // params should be skipped when None
        assert!(!json.contains("params"));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"ping\""));
    }

    #[test]
    fn test_request_serialization_with_params() {
        let req = Request {
            id: 42,
            method: "analyze".to_string(),
            params: Some(serde_json::json!({"file": "/tmp/test.al"})),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"params\""));
        assert!(json.contains("\"file\":\"/tmp/test.al\""));
    }

    #[test]
    fn test_response_deserialization_success() {
        let json = r#"{"id":1,"result":"pong"}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, 1);
        assert_eq!(resp.result.unwrap().as_str().unwrap(), "pong");
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_response_deserialization_error() {
        let json = r#"{"id":2,"error":{"code":-32601,"message":"Unknown method: foo"}}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, 2);
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert_eq!(err.message, "Unknown method: foo");
    }

    #[test]
    fn test_response_deserialization_with_both() {
        // Some implementations include both result and error
        let json = r#"{"id":3,"result":null,"error":{"code":-32603,"message":"internal"}}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, 3);
        assert!(resp.error.is_some());
    }

    #[test]
    fn test_rpc_error_display() {
        let err = RpcError {
            code: -32601,
            message: "Method not found".to_string(),
        };
        assert_eq!(err.to_string(), "RPC error -32601: Method not found");
    }

    #[test]
    fn test_request_roundtrip() {
        let req = Request {
            id: 99,
            method: "builtins".to_string(),
            params: Some(serde_json::json!({})),
        };
        let json = serde_json::to_string(&req).unwrap();
        // Parse it back as a generic value to verify structure
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["id"], 99);
        assert_eq!(val["method"], "builtins");
    }
}
