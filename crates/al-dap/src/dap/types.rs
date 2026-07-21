//! AL-specific result types for debug sessions.
//!
//! These are the types returned by `DebugSession` methods and serialized
//! as JSON for CLI/MCP consumers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    Compiling,
    Running,
    Paused,
    Stopped,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionStatus::Compiling => write!(f, "compiling"),
            SessionStatus::Running => write!(f, "running"),
            SessionStatus::Paused => write!(f, "paused"),
            SessionStatus::Stopped => write!(f, "stopped"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugState {
    pub status: SessionStatus,
    pub session_id: String,
    pub location: Option<Location>,
    pub stack: Vec<StackFrame>,
    pub variables: Vec<Variable>,
    pub thread_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub procedure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub source: Option<String>,
    pub line: u32,
    pub column: u32,
}

/// A variable with optional Record field expansion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    /// Expanded fields for Record-type variables (1 level deep).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Variable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakpointInfo {
    pub id: i64,
    pub file: String,
    pub line: u32,
    pub condition: Option<String>,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalResult {
    pub result: String,
    pub type_name: String,
}

/// A recorded breakpoint hit stored in debug history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakpointHit {
    pub seq: u32,
    pub breakpoint_id: i64,
    pub timestamp: String,
    pub location: Location,
    pub variables: Vec<Variable>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_display() {
        assert_eq!(SessionStatus::Compiling.to_string(), "compiling");
        assert_eq!(SessionStatus::Running.to_string(), "running");
        assert_eq!(SessionStatus::Paused.to_string(), "paused");
        assert_eq!(SessionStatus::Stopped.to_string(), "stopped");
    }

    #[test]
    fn debug_state_serializes_camel_case() {
        let state = DebugState {
            status: SessionStatus::Paused,
            session_id: "s1".to_string(),
            location: Some(Location {
                file: "test.al".to_string(),
                line: 42,
                column: 1,
                procedure: Some("OnRun".to_string()),
            }),
            stack: vec![StackFrame {
                id: 1,
                name: "OnRun".to_string(),
                source: Some("test.al".to_string()),
                line: 42,
                column: 1,
            }],
            variables: vec![Variable {
                name: "Rec".to_string(),
                value: "Customer".to_string(),
                type_name: "Record Customer".to_string(),
                fields: vec![Variable {
                    name: "No.".to_string(),
                    value: "10000".to_string(),
                    type_name: "Code[20]".to_string(),
                    fields: vec![],
                }],
            }],
            thread_id: Some(1),
        };

        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"sessionId\""));
        assert!(json.contains("\"threadId\""));
        assert!(json.contains("\"typeName\""));

        let parsed: DebugState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, SessionStatus::Paused);
        assert_eq!(parsed.session_id, "s1");
        assert_eq!(parsed.stack.len(), 1);
        assert_eq!(parsed.variables[0].fields.len(), 1);
    }

    #[test]
    fn variable_without_fields_omits_field() {
        let var = Variable {
            name: "x".to_string(),
            value: "42".to_string(),
            type_name: "Integer".to_string(),
            fields: vec![],
        };
        let json = serde_json::to_string(&var).unwrap();
        assert!(!json.contains("fields"));
    }

    #[test]
    fn breakpoint_info_serializes() {
        let bp = BreakpointInfo {
            id: 1,
            file: "test.al".to_string(),
            line: 10,
            condition: Some("Rec.\"No.\" = '10000'".to_string()),
            verified: true,
        };
        let json = serde_json::to_string(&bp).unwrap();
        let parsed: BreakpointInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.condition.unwrap(), "Rec.\"No.\" = '10000'");
    }

    #[test]
    fn eval_result_serializes() {
        let result = EvalResult {
            result: "10000".to_string(),
            type_name: "Code[20]".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"typeName\""));
    }

    #[test]
    fn breakpoint_hit_serializes() {
        let hit = BreakpointHit {
            seq: 1,
            breakpoint_id: 5,
            timestamp: "2026-03-15T10:00:00Z".to_string(),
            location: Location {
                file: "test.al".to_string(),
                line: 42,
                column: 1,
                procedure: Some("OnRun".to_string()),
            },
            variables: vec![],
        };
        let json = serde_json::to_string(&hit).unwrap();
        assert!(json.contains("\"breakpointId\""));
    }
}
