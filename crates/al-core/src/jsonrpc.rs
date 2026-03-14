//! Shared JSON-RPC types for .NET bridge subprocess communication.
//!
//! Re-exports from `al_protocol::jsonrpc`. Both `al-semantic` and `al-dap`
//! use these types for line-delimited JSON-RPC over stdin/stdout.

pub use al_protocol::jsonrpc::{error_codes, Request, Response, RpcError};

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
