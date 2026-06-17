//! DAP wire protocol types.
//!
//! These represent the raw JSON messages exchanged over the DAP wire format.
//! They're intentionally loose (using `serde_json::Value` for bodies) since
//! DAP has many optional/variant-specific fields.

use serde::{Deserialize, Serialize};

/// client → adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapRequest {
    pub seq: i64,
    #[serde(rename = "type")]
    pub type_: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
}

/// adapter → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapResponse {
    pub seq: i64,
    #[serde(rename = "type")]
    pub type_: String,
    pub request_seq: i64,
    pub success: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

/// adapter → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapEvent {
    pub seq: i64,
    #[serde(rename = "type")]
    pub type_: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub enum DapMessage {
    Request(DapRequest),
    Response(DapResponse),
    Event(DapEvent),
}

impl DapMessage {
    pub fn parse(data: &[u8]) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_slice(data)?;
        let type_ = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match type_ {
            "request" => Ok(DapMessage::Request(serde_json::from_value(value)?)),
            "response" => Ok(DapMessage::Response(serde_json::from_value(value)?)),
            "event" => Ok(DapMessage::Event(serde_json::from_value(value)?)),
            _ => {
                // Try response first (most common), then event, then request
                if let Ok(r) = serde_json::from_value::<DapResponse>(value.clone()) {
                    Ok(DapMessage::Response(r))
                } else if let Ok(e) = serde_json::from_value::<DapEvent>(value.clone()) {
                    Ok(DapMessage::Event(e))
                } else {
                    Ok(DapMessage::Request(serde_json::from_value(value)?))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = DapRequest {
            seq: 1,
            type_: "request".to_string(),
            command: "initialize".to_string(),
            arguments: Some(serde_json::json!({"clientID": "al-dap-client"})),
        };
        let json = serde_json::to_vec(&req).unwrap();
        let parsed = DapMessage::parse(&json).unwrap();
        assert!(matches!(parsed, DapMessage::Request(_)));
        if let DapMessage::Request(r) = parsed {
            assert_eq!(r.command, "initialize");
            assert_eq!(r.seq, 1);
        }
    }

    #[test]
    fn response_roundtrip() {
        let resp = DapResponse {
            seq: 2,
            type_: "response".to_string(),
            request_seq: 1,
            success: true,
            command: "initialize".to_string(),
            message: None,
            body: Some(serde_json::json!({"supportsConfigurationDoneRequest": true})),
        };
        let json = serde_json::to_vec(&resp).unwrap();
        let parsed = DapMessage::parse(&json).unwrap();
        assert!(matches!(parsed, DapMessage::Response(_)));
        if let DapMessage::Response(r) = parsed {
            assert!(r.success);
            assert_eq!(r.request_seq, 1);
        }
    }

    #[test]
    fn event_roundtrip() {
        let evt = DapEvent {
            seq: 3,
            type_: "event".to_string(),
            event: "stopped".to_string(),
            body: Some(serde_json::json!({"reason": "breakpoint", "threadId": 1})),
        };
        let json = serde_json::to_vec(&evt).unwrap();
        let parsed = DapMessage::parse(&json).unwrap();
        assert!(matches!(parsed, DapMessage::Event(_)));
        if let DapMessage::Event(e) = parsed {
            assert_eq!(e.event, "stopped");
        }
    }

    #[test]
    fn request_without_arguments() {
        let req = DapRequest {
            seq: 5,
            type_: "request".to_string(),
            command: "configurationDone".to_string(),
            arguments: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("arguments"));
    }

    #[test]
    fn error_response() {
        let resp = DapResponse {
            seq: 4,
            type_: "response".to_string(),
            request_seq: 3,
            success: false,
            command: "evaluate".to_string(),
            message: Some("Expression evaluation failed".to_string()),
            body: None,
        };
        let json = serde_json::to_vec(&resp).unwrap();
        let parsed = DapMessage::parse(&json).unwrap();
        if let DapMessage::Response(r) = parsed {
            assert!(!r.success);
            assert_eq!(r.message.unwrap(), "Expression evaluation failed");
        }
    }
}
