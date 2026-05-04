//! JSON-RPC types for daemon protocol communication.
//!
//! This is the canonical definition shared by al-protocol, al-core, and al-lsp.

use serde::{Deserialize, Serialize};

// T057: JSON-RPC 2.0 spec requires a `jsonrpc: "2.0"` field on every
// Request and Response. The field is wired in as a `#[serde(default)]`
// String so existing in-tree construction sites continue to compile
// via `..Default::default()` spread or via the `Request::new` /
// `Response::ok` / `Response::error` / `Response::null` constructors.
// Deserialization tolerates omission (defaults to "2.0") so the daemon
// remains backward-compatible with any older client that doesn't emit
// the field.

/// Default value for the `jsonrpc` field.
fn default_jsonrpc() -> String {
    "2.0".to_string()
}

/// A JSON-RPC request.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id: 0,
            method: String::new(),
            params: None,
        }
    }
}

impl Request {
    /// Build a Request with the canonical `jsonrpc: "2.0"` field set.
    pub fn new(id: u64, method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC response.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub error: Option<RpcError>,
}

impl Default for Response {
    fn default() -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id: 0,
            result: None,
            error: None,
        }
    }
}

impl Response {
    /// Successful response with a JSON result value.
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    pub fn error(id: u64, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }

    /// Null response — no result and no error. Some LSP methods (e.g.
    /// definition with no match) legitimately return null.
    pub fn null(id: u64) -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id,
            result: None,
            error: None,
        }
    }
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
    fn request_serialization_omits_null_params() {
        let req = Request::new(1, "ping", None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"params\""));
        // T057: the `jsonrpc: "2.0"` field is now serialised on every Request.
        assert!(
            json.contains("\"jsonrpc\":\"2.0\""),
            "Request must include jsonrpc=\"2.0\" per JSON-RPC 2.0 spec; got {json}"
        );
    }

    /// T057: Response::ok / Response::error / Response::null all set
    /// jsonrpc="2.0" on the wire.
    #[test]
    fn t057_response_constructors_emit_jsonrpc_field() {
        let ok = Response::ok(1, serde_json::json!({"v": 42}));
        let err = Response::error(2, error_codes::METHOD_NOT_FOUND, "no");
        let null = Response::null(3);
        for resp in [&ok, &err, &null] {
            let s = serde_json::to_string(resp).unwrap();
            assert!(
                s.contains("\"jsonrpc\":\"2.0\""),
                "Response missing jsonrpc=\"2.0\"; got {s}"
            );
        }
    }

    /// T057 negative: a Response JSON without the jsonrpc field still
    /// deserializes (default = "2.0") so the daemon remains backward-
    /// compatible with older clients.
    #[test]
    fn t057_response_deserialization_tolerates_missing_jsonrpc_field() {
        let json = r#"{"id":1,"result":{"v":42}}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, 1);
    }

    #[test]
    fn response_deserialization_with_error() {
        let json = r#"{"id":2,"error":{"code":-32601,"message":"Unknown method"}}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error.unwrap().code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn response_ok_constructor() {
        let resp = Response::ok(1, serde_json::json!({"status": "ok"}));
        assert_eq!(resp.id, 1);
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn response_error_constructor() {
        let resp = Response::error(2, error_codes::METHOD_NOT_FOUND, "Unknown method: foo");
        assert_eq!(resp.id, 2);
        assert!(resp.result.is_none());
        assert_eq!(resp.error.unwrap().code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn rpc_error_display() {
        let err = RpcError {
            code: -32601,
            message: "Method not found".to_string(),
        };
        assert_eq!(err.to_string(), "RPC error -32601: Method not found");
    }
}
