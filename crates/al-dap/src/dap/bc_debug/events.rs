//! BC SignalR server-push event types and conversion from the raw wire format.
//! Split out of the former monolithic `bc_debug.rs` (pure move, no behavior change).

use super::wire::SignalRMessage;

/// A BC server-push event, produced by `flush_pending_events` and `try_drain_push_events`.
///
/// These map to SignalR type-1 callback messages from the debug hub.
#[derive(Debug, Clone)]
pub enum BcEvent {
    /// Execution stopped (breakpoint hit, step complete, or exception).
    ///
    /// `thread_id` is always 1 for AL (single-threaded).
    /// `reason` is typically "breakpoint", "step", or "exception".
    /// `location` carries the top stack frame extracted from the BC Break
    /// callback (`[ApplicationObjectIdWrapper, StackFrame[], message]`) so
    /// daemon-side history records the *actual* break site rather than a copy
    /// of the previous entry. `None` when BC sent no stack frame.
    Break {
        reason: String,
        thread_id: i64,
        location: Option<BreakLocation>,
    },
    Detached {
        terminate: bool,
    },
    FatalError {
        message: String,
    },
    /// Other unrecognised server callback — target name preserved for logging.
    Other {
        target: String,
    },
}

/// Top-frame source location extracted from a BC `Break` callback's
/// `StackFrame[]` argument. Path resolution (object id → workspace file) is the
/// caller's responsibility; this carries only what BC reports directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakLocation {
    pub line: u32,
    pub column: u32,
    /// `SourcePosition`-bearing frame's `DisplayName` (procedure/trigger name).
    pub procedure: Option<String>,
    /// BC `ApplicationObjectId.ObjectType` of the break frame, if present.
    pub object_type: Option<i32>,
    /// BC `ApplicationObjectId.ObjectNumber` of the break frame, if present.
    pub object_number: Option<i32>,
}

/// Extract the top frame's source location from a BC `Break` callback's
/// arguments: `[ApplicationObjectIdWrapper, StackFrame[], message]`. The first
/// stack frame (index 0) is the current execution point. Field name casing
/// varies across BC versions, so both PascalCase and camelCase are accepted —
/// matching `native_dap::bc_stack_to_dap`.
fn break_location_from_args(arguments: &Option<Vec<serde_json::Value>>) -> Option<BreakLocation> {
    let frames = arguments.as_ref()?.get(1)?.as_array()?;
    let frame = frames.first()?;

    let source_position = frame
        .get("SourcePosition")
        .or_else(|| frame.get("sourcePosition"));
    let line = source_position
        .and_then(|sp| sp.get("Line").or_else(|| sp.get("line")))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as u32;
    let column = source_position
        .and_then(|sp| sp.get("Column").or_else(|| sp.get("column")))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as u32;

    let procedure = frame
        .get("DisplayName")
        .or_else(|| frame.get("displayName"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let object_id = frame
        .get("ApplicationObjectId")
        .or_else(|| frame.get("applicationObjectId"));
    let object_type = object_id
        .and_then(|oid| oid.get("ObjectType").or_else(|| oid.get("objectType")))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let object_number = object_id
        .and_then(|oid| oid.get("ObjectNumber").or_else(|| oid.get("objectNumber")))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);

    Some(BreakLocation {
        line,
        column,
        procedure,
        object_type,
        object_number,
    })
}

/// Convert a raw `SignalRMessage` (type-1 server callback) to a `BcEvent`.
/// Extract an informative message from an `OnFatalDebuggerException` callback.
///
/// BC sends the fatal message as the first element of the `arguments` array.
/// When that is unavailable, collapsing every failure into the opaque string
/// `"unknown"` makes production troubleshooting of BC fatal errors very hard.
/// Distinguish the three failure shapes so the
/// log line tells the operator *why* no message was extracted:
///
/// - `arguments` field absent entirely,
/// - `arguments` present but an empty array,
/// - first element present but not a JSON string (report its type).
pub(super) fn fatal_exception_message(arguments: &Option<Vec<serde_json::Value>>) -> String {
    match arguments {
        None => "<no message provided: arguments field absent — check BC server logs>".to_string(),
        Some(args) => match args.first() {
            None => {
                "<no message provided: empty arguments array — check BC server logs>".to_string()
            }
            Some(v) => match v.as_str() {
                Some(s) => s.to_string(),
                None => {
                    let kind = match v {
                        serde_json::Value::Null => "null",
                        serde_json::Value::Bool(_) => "bool",
                        serde_json::Value::Number(_) => "number",
                        serde_json::Value::Array(_) => "array",
                        serde_json::Value::Object(_) => "object",
                        serde_json::Value::String(_) => "string",
                    };
                    format!("<non-string fatal message (received JSON {kind}); raw={v}>")
                }
            },
        },
    }
}

/// Returns `None` for messages that don't need to be forwarded to the DAP layer.
pub(super) fn signalr_to_bc_event(msg: &SignalRMessage) -> Option<BcEvent> {
    let target = msg.target.as_deref()?;
    match target {
        "Break" => Some(BcEvent::Break {
            reason: "breakpoint".to_string(),
            thread_id: 1,
            location: break_location_from_args(&msg.arguments),
        }),
        "OnDetachedFromConnection" => {
            let terminate = msg
                .arguments
                .as_ref()
                .and_then(|a| a.first())
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(BcEvent::Detached { terminate })
        }
        "OnFatalDebuggerException" => {
            let message = fatal_exception_message(&msg.arguments);
            Some(BcEvent::FatalError { message })
        }
        "IsAlive" | "OnAttachedToConnection" => None, // handled internally
        other => Some(BcEvent::Other {
            target: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_message_returns_actual_string() {
        let args = Some(vec![serde_json::json!("disk full")]);
        assert_eq!(fatal_exception_message(&args), "disk full");
    }

    #[test]
    fn fatal_message_distinguishes_absent_arguments() {
        let msg = fatal_exception_message(&None);
        assert!(
            msg.contains("arguments field absent"),
            "absent arguments must be distinguished: {msg}"
        );
        assert_ne!(msg, "unknown");
    }

    #[test]
    fn fatal_message_distinguishes_empty_array() {
        let msg = fatal_exception_message(&Some(vec![]));
        assert!(
            msg.contains("empty arguments array"),
            "empty array must be distinguished: {msg}"
        );
        assert_ne!(msg, "unknown");
    }

    #[test]
    fn fatal_message_reports_non_string_type_and_raw_value() {
        let args = Some(vec![serde_json::json!(42)]);
        let msg = fatal_exception_message(&args);
        assert!(msg.contains("number"), "must report JSON type: {msg}");
        assert!(msg.contains("42"), "must include raw value: {msg}");
        assert_ne!(msg, "unknown");
    }

    #[test]
    fn fatal_message_via_signalr_to_bc_event_is_informative() {
        // Absent arguments on the actual conversion path must surface an
        // informative FatalError, not the opaque "unknown" of old.
        let msg = SignalRMessage {
            type_: 1,
            target: Some("OnFatalDebuggerException".to_string()),
            arguments: None,
            invocation_id: None,
            result: None,
            error: None,
        };
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::FatalError { message }) => {
                assert!(message.contains("arguments field absent"), "{message}");
            }
            other => panic!("expected FatalError, got {other:?}"),
        }
    }

    fn invocation(
        target: Option<&str>,
        arguments: Option<Vec<serde_json::Value>>,
    ) -> SignalRMessage {
        SignalRMessage {
            type_: 1,
            target: target.map(|t| t.to_string()),
            arguments,
            invocation_id: None,
            result: None,
            error: None,
        }
    }

    #[test]
    fn signalr_to_bc_event_break_maps_to_breakpoint_on_thread_1() {
        // A "Break" callback always yields a Break event with reason
        // "breakpoint" on AL's single thread (id 1), regardless of arguments.
        let msg = invocation(Some("Break"), None);
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Break {
                reason,
                thread_id,
                location,
            }) => {
                assert_eq!(reason, "breakpoint");
                assert_eq!(thread_id, 1);
                // No StackFrame[] argument → no location.
                assert_eq!(location, None);
            }
            other => panic!("expected Break, got {other:?}"),
        }
    }

    #[test]
    fn break_event_extracts_top_frame_location_from_stack() {
        // BC Break callback args: [ApplicationObjectIdWrapper, StackFrame[], message].
        // The top (index 0) frame's SourcePosition is the actual break site.
        let args = vec![
            serde_json::json!({ "ObjectType": 5, "ObjectNumber": 50100 }),
            serde_json::json!([
                {
                    "DisplayName": "OnRun",
                    "SourcePosition": { "Line": 42, "Column": 8 },
                    "ApplicationObjectId": { "ObjectType": 5, "ObjectNumber": 50100 }
                },
                {
                    "DisplayName": "Caller",
                    "SourcePosition": { "Line": 1, "Column": 0 }
                }
            ]),
            serde_json::json!("stopped"),
        ];
        let msg = invocation(Some("Break"), Some(args));
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Break { location, .. }) => {
                let loc = location.expect("location extracted from top StackFrame");
                assert_eq!(loc.line, 42);
                assert_eq!(loc.column, 8);
                assert_eq!(loc.procedure.as_deref(), Some("OnRun"));
                assert_eq!(loc.object_type, Some(5));
                assert_eq!(loc.object_number, Some(50100));
            }
            other => panic!("expected Break, got {other:?}"),
        }
    }

    #[test]
    fn break_event_accepts_camelcase_source_position() {
        // Newer BC versions may emit camelCase field names.
        let args = vec![
            serde_json::Value::Null,
            serde_json::json!([
                { "sourcePosition": { "line": 7, "column": 3 } }
            ]),
            serde_json::json!(""),
        ];
        let msg = invocation(Some("Break"), Some(args));
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Break { location, .. }) => {
                let loc = location.expect("location extracted");
                assert_eq!(loc.line, 7);
                assert_eq!(loc.column, 3);
            }
            other => panic!("expected Break, got {other:?}"),
        }
    }

    #[test]
    fn signalr_to_bc_event_detached_reads_terminate_true() {
        // First argument `true` means the session should terminate.
        let msg = invocation(
            Some("OnDetachedFromConnection"),
            Some(vec![serde_json::json!(true)]),
        );
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Detached { terminate }) => assert!(terminate),
            other => panic!("expected Detached, got {other:?}"),
        }
    }

    #[test]
    fn signalr_to_bc_event_detached_reads_terminate_false() {
        // First argument `false` means detach without terminating.
        let msg = invocation(
            Some("OnDetachedFromConnection"),
            Some(vec![serde_json::json!(false)]),
        );
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Detached { terminate }) => assert!(!terminate),
            other => panic!("expected Detached, got {other:?}"),
        }
    }

    #[test]
    fn signalr_to_bc_event_detached_defaults_terminate_false_when_missing() {
        // Absent / non-boolean arguments must default to `terminate = false`
        // rather than panicking or terminating the session unexpectedly.
        for args in [
            None,
            Some(vec![]),
            Some(vec![serde_json::json!("not a bool")]),
        ] {
            let msg = invocation(Some("OnDetachedFromConnection"), args.clone());
            match signalr_to_bc_event(&msg) {
                Some(BcEvent::Detached { terminate }) => {
                    assert!(!terminate, "args {args:?} should default terminate=false")
                }
                other => panic!("expected Detached for args {args:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn signalr_to_bc_event_internal_targets_are_dropped() {
        // IsAlive / OnAttachedToConnection are handled internally and must not
        // be forwarded to the DAP layer.
        for target in ["IsAlive", "OnAttachedToConnection"] {
            let msg = invocation(Some(target), None);
            assert!(
                signalr_to_bc_event(&msg).is_none(),
                "{target} must not produce a BcEvent"
            );
        }
    }

    #[test]
    fn signalr_to_bc_event_unknown_target_preserved_as_other() {
        // An unrecognised callback is preserved verbatim as Other so it can be
        // logged without losing the target name.
        let msg = invocation(Some("SomeFutureCallback"), None);
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Other { target }) => assert_eq!(target, "SomeFutureCallback"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn signalr_to_bc_event_missing_target_is_dropped() {
        // Type-3/6 frames carry no target; they must not be forwarded.
        let msg = invocation(None, None);
        assert!(signalr_to_bc_event(&msg).is_none());
    }

    #[test]
    fn break_location_none_when_arguments_absent() {
        assert_eq!(break_location_from_args(&None), None);
    }

    #[test]
    fn break_location_none_when_no_stack_frame_argument() {
        // arguments[1] (the StackFrame[]) is missing → no location.
        let args = Some(vec![serde_json::json!({ "ObjectType": 5 })]);
        assert_eq!(break_location_from_args(&args), None);
    }

    #[test]
    fn break_location_none_when_frames_empty() {
        // arguments[1] is an empty array → no first frame → no location.
        let args = Some(vec![
            serde_json::Value::Null,
            serde_json::json!([]),
            serde_json::json!("msg"),
        ]);
        assert_eq!(break_location_from_args(&args), None);
    }

    #[test]
    fn break_location_defaults_line_column_to_zero_when_missing() {
        // A frame with no SourcePosition still yields a location with line/col 0
        // (the unwrap_or(0) fallback), rather than None.
        let args = Some(vec![
            serde_json::Value::Null,
            serde_json::json!([{ "DisplayName": "OnRun" }]),
            serde_json::json!(""),
        ]);
        let loc = break_location_from_args(&args).expect("frame present → Some location");
        assert_eq!(loc.line, 0);
        assert_eq!(loc.column, 0);
        assert_eq!(loc.procedure.as_deref(), Some("OnRun"));
        assert_eq!(loc.object_type, None);
        assert_eq!(loc.object_number, None);
    }

    #[test]
    fn break_location_empty_display_name_becomes_none_procedure() {
        let args = Some(vec![
            serde_json::Value::Null,
            serde_json::json!([{
                "DisplayName": "",
                "SourcePosition": { "Line": 3, "Column": 1 }
            }]),
            serde_json::json!(""),
        ]);
        let loc = break_location_from_args(&args).expect("Some location");
        assert_eq!(loc.procedure, None, "empty DisplayName must map to None");
        assert_eq!(loc.line, 3);
        assert_eq!(loc.column, 1);
    }

    #[test]
    fn break_location_reads_camelcase_object_id() {
        let args = Some(vec![
            serde_json::Value::Null,
            serde_json::json!([{
                "sourcePosition": { "line": 9, "column": 2 },
                "applicationObjectId": { "objectType": 7, "objectNumber": 50200 }
            }]),
            serde_json::json!(""),
        ]);
        let loc = break_location_from_args(&args).expect("Some location");
        assert_eq!(loc.object_type, Some(7));
        assert_eq!(loc.object_number, Some(50200));
    }
}
