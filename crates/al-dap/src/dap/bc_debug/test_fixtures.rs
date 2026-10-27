//! Wire payloads for tests, derived field for field from the decompiled
//! Microsoft types (PascalCase, enums as integers, per AspNetCoreSignalRFactory.cs:24-27).
//! A CDX capture can replace these later.

use serde_json::{json, Value};

/// `ApplicationObjectIdWrapper` for codeunit 50100.
// fixture source: TypeWrappers/Microsoft.Dynamics.Nav.DebuggerService/ApplicationObjectIdWrapper.cs:20-24
// (ObjectNumber int, ObjectType ObjectTypeWrapper.CodeUnit = 5, ObjectTypeWrapper.cs)
pub(crate) fn object_id() -> Value {
    json!({ "ObjectNumber": 50100, "ObjectType": 5 })
}

/// One `StackFrame`. `from` and `to` are `(Line, Column)` of `StatementSpan`.
// fixture source: TypeWrappers/Microsoft.Dynamics.Nav.DebuggerService/StackFrame.cs:34-58
// (MethodName, ObjectId, ObjectName, StatementSpan) and SourceSpan.cs:14-18, SourcePosition.cs:15-19
pub(crate) fn stack_frame(method: &str, from: (u16, u16), to: (u16, u16)) -> Value {
    json!({
        "MethodName": method,
        "ObjectId": object_id(),
        "ObjectName": "BJF Probe",
        "StatementSpan": {
            "From": { "Line": from.0, "Column": from.1 },
            "To": { "Line": to.0, "Column": to.1 }
        }
    })
}

/// Arguments of the `Break` callback: `[ApplicationObjectIdWrapper, StackFrame[], string]`.
// fixture source: Microsoft.Dynamics.Nav.EditorServices.Protocol.DebugAdapter/HubBasedDebuggerService.cs:393
// (`Break(objectId, stack, errorMessage)`)
pub(crate) fn break_args(frames: Vec<Value>, message: &str) -> Vec<Value> {
    vec![object_id(), Value::Array(frames), json!(message)]
}

/// The `AddBreakpoint` reply for codeunit 50100 at 0-based line 41.
// fixture source: TypeWrappers/Microsoft.Dynamics.Nav.DebuggerService/BreakpointDefinition.cs:9-27
// (BreakpointId, Condition, InternalMethodName, MethodName, ObjectId, SourceSpan, RelativeSourceSpan)
pub(crate) fn breakpoint_definition(id: i64) -> Value {
    json!({
        "BreakpointId": id,
        "Condition": "",
        "InternalMethodName": "OnRun",
        "MethodName": "OnRun",
        "ObjectId": object_id(),
        "SourceSpan": {
            "From": { "Line": 41, "Column": 0 },
            "To": { "Line": 41, "Column": 20 }
        },
        "RelativeSourceSpan": {
            "From": { "Line": 3, "Column": 0 },
            "To": { "Line": 3, "Column": 20 }
        }
    })
}

/// `SourcePosition` as sent to `AddBreakpoint`.
// fixture source: TypeWrappers/Microsoft.Dynamics.Nav.DebuggerService/SourcePosition.cs:12-15 (Line, Column ushort)
pub(crate) fn source_position(line: u16, column: u16) -> Value {
    json!({ "Line": line, "Column": column })
}

/// `AttachOptions` as sent to `Attach`.
// fixture source: Microsoft.Dynamics.Nav.EditorServices.Protocol/Microsoft.Dynamics.Nav.EditorServices.Protocol.DebugAdapter/AttachOptions.cs:21-27
// (BreakOnNextClient enum as int, SessionId int, UserId string or null)
pub(crate) fn attach_options(break_on_next: i32, session_id: i32, user_id: Option<&str>) -> Value {
    json!({ "BreakOnNextClient": break_on_next, "SessionId": session_id, "UserId": user_id })
}

/// `DebugOptions` as sent to `DebugAdapterConfigurationDone` for the config's break settings.
// fixture source: Microsoft.Dynamics.Nav.EditorServices.Protocol/Microsoft.Dynamics.Nav.EditorServices.Protocol.DebugAdapter/DebugOptions.cs:19-44
// (nine JsonProperty members, enums as int)
pub(crate) fn debug_options(config: &super::BcDebugConfig) -> Value {
    json!({
        "BreakOnError": config.break_on_error.enabled(),
        "BreakOnErrorBehaviour": config.break_on_error.wire_value(),
        "BreakOnRecordWrite": config.break_on_record_write.enabled(),
        "BreakOnRecordWriteBehaviour": config.break_on_record_write.wire_value(),
        "SkipSystemTriggers": true,
        "EnableSqlInformationDebugger": config.enable_sql_information_debugger,
        "EnableLongRunningSqlStatements": config.enable_long_running_sql_statements,
        "LongRunningSqlStatementsThreshold": config.long_running_sql_statements_threshold,
        "NumberOfSqlStatements": config.number_of_sql_statements,
    })
}
