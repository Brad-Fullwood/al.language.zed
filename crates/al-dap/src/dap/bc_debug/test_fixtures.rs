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
