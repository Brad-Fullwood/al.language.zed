//! JSON-RPC types for daemon protocol communication.
//!
//! This is the canonical definition shared by the daemon and its clients.

use serde::{Deserialize, Serialize};

// Deserialization tolerates older clients that omit the protocol version;
// constructors always emit the JSON-RPC 2.0 field required by the spec.

fn default_jsonrpc() -> String {
    "2.0".to_string()
}

/// A JSON-RPC 2.0 request identifier.
///
/// The spec allows a string, a number, or (discouraged but legal) null. The
/// daemon historically accepted only `u64`, which made every well-formed
/// string-id request fail deserialization and get answered with
/// `-32700 PARSE_ERROR`. Parsing the id as an untagged enum keeps the numeric
/// wire form byte-identical for existing callers while accepting the other two
/// legal shapes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(serde_json::Number),
    String(String),
    Null,
}

impl RequestId {
    /// The numeric value of this id, when it has one.
    ///
    /// Internal dispatch is keyed on `u64`; non-numeric ids are dispatched with
    /// a placeholder and the original id is restored on the wire.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            RequestId::Number(number) => number.as_u64(),
            _ => None,
        }
    }

    /// The JSON value for this id, suitable for echoing into a response frame.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            RequestId::Number(number) => serde_json::Value::Number(number.clone()),
            RequestId::String(text) => serde_json::Value::String(text.clone()),
            RequestId::Null => serde_json::Value::Null,
        }
    }
}

impl From<u64> for RequestId {
    fn from(value: u64) -> Self {
        RequestId::Number(serde_json::Number::from(value))
    }
}

/// Deserialize a *present* `id` member, including an explicit `null`.
///
/// `#[serde(default)]` only fires when the key is absent, so this keeps
/// "no id" (a notification) distinguishable from `"id": null` (a request whose
/// response must echo `null`).
fn deserialize_present_id<'de, D>(deserializer: D) -> Result<Option<RequestId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    RequestId::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    /// `None` means the message is a notification and MUST NOT be answered.
    #[serde(default, deserialize_with = "deserialize_present_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id: Some(RequestId::from(0)),
            method: String::new(),
            params: None,
        }
    }
}

impl Request {
    pub fn new(id: u64, method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id: Some(RequestId::from(id)),
            method: method.into(),
            params,
        }
    }

    /// A JSON-RPC 2.0 notification — no `id`, and therefore no response.
    pub fn notification(method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id: None,
            method: method.into(),
            params,
        }
    }

    /// True when this message is a notification (no `id` member at all).
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// The numeric id used for internal dispatch. String/null ids dispatch
    /// under `0`; the caller restores the original id on the wire.
    pub fn dispatch_id(&self) -> u64 {
        self.id.as_ref().and_then(RequestId::as_u64).unwrap_or(0)
    }
}

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
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id,
            result: Some(result),
            error: None,
        }
    }

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

    /// Null response — explicit JSON-RPC 2.0 success with `result: null`.
    /// Some LSP methods (e.g. definition with no match) legitimately return
    /// null. The spec requires success responses to carry `result` even when
    /// the value is null, so we serialize `Some(Value::Null)`, not `None`.
    pub fn null(id: u64) -> Self {
        Self {
            jsonrpc: default_jsonrpc(),
            id,
            result: Some(serde_json::Value::Null),
            error: None,
        }
    }

    /// Serialize this response, substituting the originating request's id.
    ///
    /// Dispatch is keyed on `u64`, but a request may legally carry a string or
    /// null id. The response frame must echo the id exactly as received, so the
    /// wire form is produced here rather than by serializing `self` directly.
    pub fn to_json_with_id(&self, id: &RequestId) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": error_codes::INTERNAL_ERROR,
                    "message": "response could not be serialized",
                },
            })
        });
        if let Some(object) = value.as_object_mut() {
            object.insert("id".to_string(), id.to_json());
        }
        value
    }
}

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
        assert!(
            json.contains("\"jsonrpc\":\"2.0\""),
            "Request must include jsonrpc=\"2.0\" per JSON-RPC 2.0 spec; got {json}"
        );
    }

    #[test]
    fn response_constructors_emit_jsonrpc_field() {
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

    #[test]
    fn response_deserialization_tolerates_missing_jsonrpc_field() {
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
    fn null_response_serializes_explicit_result_null() {
        let resp = Response::null(7);
        let s = serde_json::to_string(&resp).unwrap();
        assert!(
            s.contains("\"result\":null"),
            "Response::null must include explicit result:null on the wire; got {s}"
        );
        assert!(
            !s.contains("\"error\""),
            "Response::null must not include error field; got {s}"
        );
    }

    #[test]
    fn error_response_omits_result_field() {
        let resp = Response::error(8, error_codes::INTERNAL_ERROR, "boom");
        let s = serde_json::to_string(&resp).unwrap();
        assert!(
            !s.contains("\"result\""),
            "Response::error must omit result field entirely; got {s}"
        );
        assert!(
            s.contains("\"error\""),
            "Response::error must include error; got {s}"
        );
    }

    #[test]
    fn request_accepts_string_number_and_null_ids() {
        let numeric: Request = serde_json::from_str(r#"{"id":7,"method":"ping"}"#).unwrap();
        assert_eq!(numeric.id, Some(RequestId::from(7)));
        assert_eq!(numeric.dispatch_id(), 7);
        assert!(!numeric.is_notification());

        let string: Request = serde_json::from_str(r#"{"id":"abc","method":"ping"}"#).unwrap();
        assert_eq!(string.id, Some(RequestId::String("abc".to_string())));
        assert_eq!(string.dispatch_id(), 0);
        assert!(!string.is_notification());

        let null: Request = serde_json::from_str(r#"{"id":null,"method":"ping"}"#).unwrap();
        assert_eq!(null.id, Some(RequestId::Null));
        assert!(
            !null.is_notification(),
            "an explicit null id is a request, not a notification"
        );
    }

    #[test]
    fn request_without_id_is_a_notification() {
        let notification: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"ping"}"#).unwrap();
        assert!(notification.is_notification());
        let json = serde_json::to_string(&Request::notification("ping", None)).unwrap();
        assert!(
            !json.contains("\"id\""),
            "notifications must not serialize an id; got {json}"
        );
    }

    #[test]
    fn numeric_request_wire_form_is_unchanged() {
        let json = serde_json::to_string(&Request::new(3, "ping", None)).unwrap();
        assert_eq!(json, r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#);
    }

    #[test]
    fn response_echoes_non_numeric_request_ids() {
        let response = Response::ok(0, serde_json::json!("pong"));
        let json = response.to_json_with_id(&RequestId::String("call-1".to_string()));
        assert_eq!(json["id"], serde_json::json!("call-1"));
        assert_eq!(json["result"], serde_json::json!("pong"));

        let json = response.to_json_with_id(&RequestId::Null);
        assert!(json["id"].is_null());
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
