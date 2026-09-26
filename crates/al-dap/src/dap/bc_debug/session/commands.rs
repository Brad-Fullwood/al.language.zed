//! The debug operations a DAP handler calls, each one SignalR invocation.
//!
//! `configuration_done` is the exception: current BC rejects it with debug
//! options on servers that do not know them, so a rejected first attempt is
//! retried without arguments.

use tracing::info;

use crate::dap::Result;

use super::super::session_config::BcDebugConfig;
use super::BcDebugSession;
impl BcDebugSession {
    pub async fn attach(&self, config: &BcDebugConfig) -> Result<()> {
        // This is EditorServices' AttachOptions wire type. Break flags belong
        // to DebugOptions/configurationDone, not AttachOptions. SignalR emits
        // enum values numerically and applies camelCase to the CLR properties.
        // Default matches the documented schema default
        // (`debug_adapter_schemas/al.json`'s `breakOnNext` property) —
        // WebServiceClient — so a launch config that omits `breakOnNext`
        // attaches to the session class the schema promises, not a
        // different one.
        let break_on_next_client = match config
            .break_on_next
            .as_deref()
            .unwrap_or("WebServiceClient")
            .replace([' ', '-', '_'], "")
            .to_ascii_lowercase()
            .as_str()
        {
            "webclient" => 1,
            "background" => 2,
            "clientservice" => 3,
            "agent" => 4,
            _ => 0, // WebServiceClient
        };
        let session_id = config
            .session_id
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(-1);
        let args = serde_json::json!({
            "breakOnNextClient": break_on_next_client,
            "sessionId": session_id,
            "userId": null,
        });
        self.invoke("Attach", vec![args]).await?;
        info!("Attached to BC debug session");
        Ok(())
    }

    /// BC hub method: `DebugAdapterConfigurationDone(debugOptions)`
    /// Newer BC versions (>1.0) require debug options argument.
    pub async fn configuration_done(&self, config: &BcDebugConfig) -> Result<()> {
        // Microsoft's SignalR JSON protocol applies camelCase to the public
        // .NET DebugOptions properties. PascalCase looks plausible from the
        // CLR types but is rejected by current BC online hubs.
        let debug_options = serde_json::json!({
            "breakOnError": config.break_on_error.enabled(),
            // Current EditorServices enum values are Unspecified=0, None=1,
            // All=2, ExcludeTry=3. Sending the old 0/1 assumption causes
            // configurationDone to be rejected (or interpreted incorrectly)
            // by current Business Central online tenants.
            "breakOnErrorBehaviour": config.break_on_error.wire_value(),
            "breakOnRecordWrite": config.break_on_record_write.enabled(),
            "breakOnRecordWriteBehaviour": config.break_on_record_write.wire_value(),
            "skipSystemTriggers": true,
            "enableSqlInformationDebugger": config.enable_sql_information_debugger,
            "enableLongRunningSqlStatements": config.enable_long_running_sql_statements,
            "longRunningSqlStatementsThreshold": config.long_running_sql_statements_threshold,
            "numberOfSqlStatements": config.number_of_sql_statements,
        });
        // Try with debug options first (newer BC >=2.0), fall back to empty args
        match self
            .invoke("DebugAdapterConfigurationDone", vec![debug_options])
            .await
        {
            Ok(_) => Ok(()),
            Err(first_err) => {
                // Older BC: no args. If this also fails, both forms were
                // rejected — surface the second error instead of silently
                // returning Ok, so the caller (which uses `?`) can abort the
                // debug session rather than proceeding with an unconfigured
                // adapter that will misbehave on later operations.
                match self.invoke("DebugAdapterConfigurationDone", vec![]).await {
                    Ok(_) => Ok(()),
                    Err(second_err) => {
                        tracing::warn!(
                            "configurationDone failed both with debug options ({first_err}) \
                             and with no args ({second_err})"
                        );
                        Err(second_err)
                    }
                }
            }
        }
    }

    /// BC hub method: `AddBreakpoint(ApplicationObjectIdWrapper, SourcePosition, string condition)`
    /// - ApplicationObjectIdWrapper: `{objectType: int, objectNumber: int}`
    /// - SourcePosition: `{line: int, column: int}`
    /// - ObjectTypeWrapper enum: use `crate::native_dap::bc_object_type` constants
    pub async fn add_breakpoint(
        &self,
        object_type: i32,
        object_number: i32,
        line: i64,
        column: i64,
        condition: &str,
    ) -> Result<serde_json::Value> {
        // ApplicationObjectIdWrapper comes from TypeWrappers and requires its
        // CLR property names. camelCase silently becomes object 0/type 0 on
        // current BC hubs, producing an accepted but inert breakpoint.
        let object_id = serde_json::json!({
            "ObjectType": object_type,
            "ObjectNumber": object_number,
        });
        let position = serde_json::json!({
            "line": line,
            "column": column,
        });
        let result = self
            .invoke(
                "AddBreakpoint",
                vec![object_id, position, serde_json::json!(condition)],
            )
            .await?;
        Ok(result.unwrap_or(serde_json::Value::Null))
    }

    /// BC hub method: `RemoveBreakpoint(long breakpointId)`
    pub async fn remove_breakpoint(&self, breakpoint_id: i64) -> Result<()> {
        self.invoke("RemoveBreakpoint", vec![serde_json::json!(breakpoint_id)])
            .await?;
        Ok(())
    }

    /// BC hub method: `UpdateBreakpoint(long id, string condition)`
    pub async fn update_breakpoint(&self, breakpoint_id: i64, condition: &str) -> Result<()> {
        self.invoke(
            "UpdateBreakpoint",
            vec![
                serde_json::json!(breakpoint_id),
                serde_json::json!(condition),
            ],
        )
        .await?;
        Ok(())
    }

    /// BC hub method: `SetBreakpointResponse(breakpointResponse)`
    /// Note: BC uses "SetBreakpointResponse" for continue, not a "continue" method.
    pub async fn continue_execution(&self, breakpoint_response: serde_json::Value) -> Result<()> {
        // Exit reason 0 is plain continue; 1/2/3 (over/in/out) are steps.
        // Record which one this was *before* invoking so a Break that arrives
        // while the invoke is in flight already sees the right expectation.
        let is_step = breakpoint_response != serde_json::json!(0);
        *self.expecting_step.lock().await = is_step;
        self.invoke("SetBreakpointResponse", vec![breakpoint_response])
            .await?;
        *self.is_stopped.lock().await = false;
        Ok(())
    }

    /// Step over the current statement (BC BreakpointExitReason = 1).
    pub async fn step_over(&self) -> Result<()> {
        self.continue_execution(serde_json::json!(1)).await
    }

    /// Step into the current call (BC BreakpointExitReason = 2).
    pub async fn step_in(&self) -> Result<()> {
        self.continue_execution(serde_json::json!(2)).await
    }

    /// Step out of the current procedure (BC BreakpointExitReason = 3).
    pub async fn step_out(&self) -> Result<()> {
        self.continue_execution(serde_json::json!(3)).await
    }

    /// Detach the debugger on the server.
    ///
    /// The error is returned rather than logged: a BC session that refuses
    /// StopDebugging keeps an attached debugger, and swallowing that here left
    /// the caller reporting a clean teardown.
    pub async fn stop_debugging(&self) -> Result<()> {
        self.invoke("StopDebugging", vec![]).await?;
        Ok(())
    }

    /// BC hub method: `GetStackTrace()` → `StackFrame[]`
    ///
    /// Each StackFrame has (at minimum):
    ///   - `ApplicationObjectId` — BC object reference
    ///   - `SourcePosition` — `{Line, Column}`
    ///   - `DisplayName` — human-readable frame name
    ///
    pub async fn get_call_stack(&self) -> Result<serde_json::Value> {
        let result = self.invoke("GetStackTrace", vec![]).await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// BC hub method: `GetVariables(int frameId)` → `LocalNode[]`
    pub async fn get_variables(&self, frame_id: i64) -> Result<serde_json::Value> {
        let result = self
            .invoke("GetVariables", vec![serde_json::json!(frame_id)])
            .await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// BC hub method: `ExpandGlobals(int frameId)` → `LocalNode[]`
    pub async fn get_globals(&self, frame_id: i64) -> Result<serde_json::Value> {
        let result = self
            .invoke("ExpandGlobals", vec![serde_json::json!(frame_id)])
            .await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// BC hub method: `ExpandNode(int frameId, string path)` → `LocalNode[]`
    pub async fn expand_node(&self, frame_id: i64, path: &str) -> Result<serde_json::Value> {
        let result = self
            .invoke(
                "ExpandNode",
                vec![serde_json::json!(frame_id), serde_json::json!(path)],
            )
            .await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// BC hub method: `GetWatchNode(int frameId, string expression, WatchOption)` → `LocalNode`
    pub async fn evaluate(&self, frame_id: i64, expression: &str) -> Result<serde_json::Value> {
        let result = match self
            .invoke(
                "GetWatchNode",
                vec![
                    serde_json::json!(frame_id),
                    serde_json::json!(expression),
                    serde_json::json!(0),
                ],
            )
            .await
        {
            Ok(result) => result,
            Err(_) => {
                self.invoke(
                    "GetWatchNode",
                    vec![serde_json::json!(frame_id), serde_json::json!(expression)],
                )
                .await?
            }
        };
        Ok(result.unwrap_or(serde_json::Value::Null))
    }

    /// BC hub method: `GetSource(ApplicationObjectIdWrapper)` → `string`
    pub async fn get_source(&self, object_type: i32, object_number: i32) -> Result<String> {
        let object_id = serde_json::json!({
            "ObjectType": object_type,
            "ObjectNumber": object_number,
        });
        let result = self.invoke("GetSource", vec![object_id]).await?;
        Ok(result
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default())
    }

    pub async fn terminate(&self) -> Result<()> {
        if let Err(e) = self.invoke("TerminateSession", vec![]).await {
            tracing::debug!(error = %e, "TerminateSession RPC errored — session may already be closed");
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    use crate::dap::DapError;

    use super::super::test_support::{completion, invocation, next_frame};

    #[tokio::test]
    async fn add_breakpoint_serializes_object_position_condition_and_parses_result() {
        let (session, event_tx, _break_tx, mut ws_rx) = BcDebugSession::test_new("conn".into());
        // The first invocation is assigned id "1"; queue its completion so the
        // invoke loop matches it immediately (channels are FIFO + buffered).
        event_tx
            .send(completion("1", Some(serde_json::json!({ "id": 77 })), None))
            .await
            .unwrap();

        let res = session
            .add_breakpoint(5, 50100, 42, 8, "Rec.\"No.\" = '10000'")
            .await
            .expect("add_breakpoint succeeds on a completion");
        assert_eq!(
            res,
            serde_json::json!({ "id": 77 }),
            "result returned verbatim"
        );

        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["type"], 1);
        assert_eq!(frame["target"], "AddBreakpoint");
        assert_eq!(frame["invocationId"], "1");
        let args = frame["arguments"].as_array().unwrap();
        assert_eq!(args[0]["ObjectType"], 5);
        assert_eq!(args[0]["ObjectNumber"], 50100);
        assert_eq!(args[1]["line"], 42);
        assert_eq!(args[1]["column"], 8);
        assert_eq!(args[2], "Rec.\"No.\" = '10000'");
    }

    #[tokio::test]
    async fn remove_breakpoint_serializes_id() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        session.remove_breakpoint(4242).await.expect("remove ok");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["target"], "RemoveBreakpoint");
        assert_eq!(frame["arguments"][0], 4242);
    }

    #[tokio::test]
    async fn update_breakpoint_serializes_id_and_condition() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        session
            .update_breakpoint(9, "x > 5")
            .await
            .expect("update ok");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["target"], "UpdateBreakpoint");
        assert_eq!(frame["arguments"][0], 9);
        assert_eq!(frame["arguments"][1], "x > 5");
    }

    #[tokio::test]
    async fn attach_serializes_editorservices_attach_options() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        let cfg = BcDebugConfig::default();
        session.attach(&cfg).await.expect("attach ok");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["target"], "Attach");
        // Default matches the schema-documented default (WebServiceClient,
        // enum value 0) when the launch config omits breakOnNext.
        assert_eq!(frame["arguments"][0]["breakOnNextClient"], 0);
        assert_eq!(frame["arguments"][0]["sessionId"], -1);
        assert!(frame["arguments"][0]["userId"].is_null());
        assert!(frame["arguments"][0].get("breakOnError").is_none());
    }

    #[tokio::test]
    async fn attach_forwards_session_id_and_break_on_next() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        let cfg = BcDebugConfig {
            break_on_next: Some("Background".to_string()),
            session_id: Some(7),
            ..BcDebugConfig::default()
        };
        session.attach(&cfg).await.expect("attach ok");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["target"], "Attach");
        assert_eq!(frame["arguments"][0]["breakOnNextClient"], 2);
        assert_eq!(frame["arguments"][0]["sessionId"], 7);
    }

    #[tokio::test]
    async fn attach_explicit_web_client_maps_to_one_not_the_default() {
        // "WebClient" must still map to its own enum value (1), distinct
        // from the WebServiceClient default (0), now that omitting
        // breakOnNext no longer defaults to WebClient.
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        let cfg = BcDebugConfig {
            break_on_next: Some("WebClient".to_string()),
            ..BcDebugConfig::default()
        };
        session.attach(&cfg).await.expect("attach ok");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["arguments"][0]["breakOnNextClient"], 1);
    }

    #[tokio::test]
    async fn attach_explicit_web_service_client_matches_default() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        let cfg = BcDebugConfig {
            break_on_next: Some("WebServiceClient".to_string()),
            ..BcDebugConfig::default()
        };
        session.attach(&cfg).await.expect("attach ok");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["arguments"][0]["breakOnNextClient"], 0);
    }

    #[tokio::test]
    async fn configuration_done_sends_debug_options_on_first_attempt() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        let cfg = BcDebugConfig {
            break_on_error: crate::dap::bc_debug::BreakOnError::ExcludeTry,
            break_on_record_write: crate::dap::bc_debug::BreakOnRecordWrite::ExcludeTemporary,
            enable_sql_information_debugger: false,
            enable_long_running_sql_statements: false,
            long_running_sql_statements_threshold: 900,
            number_of_sql_statements: 37,
            ..BcDebugConfig::default()
        };
        session
            .configuration_done(&cfg)
            .await
            .expect("config done ok");

        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["target"], "DebugAdapterConfigurationDone");
        let args = frame["arguments"].as_array().unwrap();
        assert_eq!(args.len(), 1, "debug options arg present on first attempt");
        assert_eq!(args[0]["breakOnError"], true);
        assert_eq!(args[0]["breakOnErrorBehaviour"], 3);
        assert_eq!(args[0]["breakOnRecordWrite"], true);
        assert_eq!(args[0]["breakOnRecordWriteBehaviour"], 3);
        assert_eq!(args[0]["enableSqlInformationDebugger"], false);
        assert_eq!(args[0]["enableLongRunningSqlStatements"], false);
        assert_eq!(args[0]["longRunningSqlStatementsThreshold"], 900);
        assert_eq!(args[0]["numberOfSqlStatements"], 37);
        // A successful first attempt must NOT send the no-args fallback frame.
        assert!(
            ws_rx.try_recv().is_err(),
            "no fallback invoke when the first attempt succeeds"
        );
    }

    #[tokio::test]
    async fn configuration_done_falls_back_to_no_args_when_options_rejected() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        // First attempt (id "1") rejected by an older BC; second (id "2") ok.
        event_tx
            .send(completion(
                "1",
                None,
                Some("Method does not accept debugOptions"),
            ))
            .await
            .unwrap();
        event_tx
            .send(completion("2", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        session
            .configuration_done(&BcDebugConfig::default())
            .await
            .expect("fallback path succeeds");

        let first = next_frame(&mut ws_rx);
        let second = next_frame(&mut ws_rx);
        assert_eq!(first["target"], "DebugAdapterConfigurationDone");
        assert_eq!(
            first["arguments"].as_array().unwrap().len(),
            1,
            "first attempt carries debug options"
        );
        assert_eq!(second["target"], "DebugAdapterConfigurationDone");
        assert_eq!(
            second["arguments"].as_array().unwrap().len(),
            0,
            "fallback attempt carries no args"
        );
    }

    #[tokio::test]
    async fn configuration_done_surfaces_second_error_when_both_attempts_fail() {
        let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", None, Some("first fail")))
            .await
            .unwrap();
        event_tx
            .send(completion("2", None, Some("second fail")))
            .await
            .unwrap();
        let err = session
            .configuration_done(&BcDebugConfig::default())
            .await
            .expect_err("both attempts failing must error");
        assert!(
            matches!(&err, DapError::ServerError(m) if m.contains("second fail")),
            "must surface the second error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn step_ops_send_correct_exit_reason_codes() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        for id in ["1", "2", "3"] {
            event_tx
                .send(completion(id, Some(serde_json::json!(null)), None))
                .await
                .unwrap();
        }
        session.step_over().await.expect("step_over");
        session.step_in().await.expect("step_in");
        session.step_out().await.expect("step_out");

        let f1 = next_frame(&mut ws_rx);
        let f2 = next_frame(&mut ws_rx);
        let f3 = next_frame(&mut ws_rx);
        assert_eq!(f1["target"], "SetBreakpointResponse");
        assert_eq!(f1["arguments"][0], 1, "step_over => BreakpointExitReason 1");
        assert_eq!(f2["arguments"][0], 2, "step_in => BreakpointExitReason 2");
        assert_eq!(f3["arguments"][0], 3, "step_out => BreakpointExitReason 3");
    }

    #[tokio::test]
    async fn continue_execution_clears_is_stopped_and_sends_response() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        // Simulate a Break callback to flip is_stopped = true first.
        session
            .handle_server_callback(&invocation(Some("Break"), None))
            .await;
        assert!(session.is_stopped().await, "Break callback sets is_stopped");

        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        session
            .continue_execution(serde_json::json!(0))
            .await
            .expect("continue ok");
        assert!(!session.is_stopped().await, "continue clears is_stopped");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["target"], "SetBreakpointResponse");
        assert_eq!(frame["arguments"][0], 0);
    }

    #[tokio::test]
    async fn variable_inspection_requests_have_correct_targets_and_params() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        // Completions for the five invocations in call order (ids 1..=5).
        event_tx
            .send(completion(
                "1",
                Some(serde_json::json!([{ "name": "Customer" }])),
                None,
            ))
            .await
            .unwrap();
        event_tx
            .send(completion(
                "2",
                Some(serde_json::json!([{ "name": "x" }])),
                None,
            ))
            .await
            .unwrap();
        event_tx
            .send(completion(
                "3",
                Some(serde_json::json!([{ "name": "g" }])),
                None,
            ))
            .await
            .unwrap();
        event_tx
            .send(completion(
                "4",
                Some(serde_json::json!([{ "name": "child" }])),
                None,
            ))
            .await
            .unwrap();
        event_tx
            .send(completion(
                "5",
                Some(serde_json::json!({ "value": "42" })),
                None,
            ))
            .await
            .unwrap();

        assert_eq!(
            session.get_call_stack().await.unwrap()[0]["name"],
            "Customer"
        );
        assert_eq!(session.get_variables(7).await.unwrap()[0]["name"], "x");
        assert_eq!(session.get_globals(7).await.unwrap()[0]["name"], "g");
        assert_eq!(
            session.expand_node(7, "Customer.Address").await.unwrap()[0]["name"],
            "child"
        );
        assert_eq!(
            session.evaluate(7, "Customer.Name").await.unwrap()["value"],
            "42"
        );

        let f = next_frame(&mut ws_rx);
        assert_eq!(f["target"], "GetStackTrace");
        assert_eq!(f["arguments"].as_array().unwrap().len(), 0);
        let f = next_frame(&mut ws_rx);
        assert_eq!(f["target"], "GetVariables");
        assert_eq!(f["arguments"][0], 7);
        let f = next_frame(&mut ws_rx);
        assert_eq!(f["target"], "ExpandGlobals");
        assert_eq!(f["arguments"][0], 7);
        let f = next_frame(&mut ws_rx);
        assert_eq!(f["target"], "ExpandNode");
        assert_eq!(f["arguments"][0], 7);
        assert_eq!(f["arguments"][1], "Customer.Address");
        let f = next_frame(&mut ws_rx);
        assert_eq!(f["target"], "GetWatchNode");
        assert_eq!(f["arguments"][0], 7);
        assert_eq!(f["arguments"][1], "Customer.Name");
        assert_eq!(f["arguments"][2], 0);
    }

    #[tokio::test]
    async fn get_source_serializes_object_id_and_parses_string() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion(
                "1",
                Some(serde_json::json!("codeunit 50100 X { }")),
                None,
            ))
            .await
            .unwrap();
        let src = session.get_source(5, 50100).await.unwrap();
        assert_eq!(src, "codeunit 50100 X { }");
        let f = next_frame(&mut ws_rx);
        assert_eq!(f["target"], "GetSource");
        assert_eq!(f["arguments"][0]["ObjectType"], 5);
        assert_eq!(f["arguments"][0]["ObjectNumber"], 50100);
    }

    #[tokio::test]
    async fn get_source_non_string_result_becomes_empty_string() {
        let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(42)), None))
            .await
            .unwrap();
        assert_eq!(
            session.get_source(5, 1).await.unwrap(),
            "",
            "a non-string GetSource result must collapse to empty"
        );
    }

    #[tokio::test]
    async fn null_results_fall_back_to_empty_collections() {
        let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
        // Completions with neither result nor error → invoke returns Ok(None).
        event_tx.send(completion("1", None, None)).await.unwrap();
        event_tx.send(completion("2", None, None)).await.unwrap();
        event_tx.send(completion("3", None, None)).await.unwrap();
        assert_eq!(
            session.get_call_stack().await.unwrap(),
            serde_json::json!([])
        );
        assert_eq!(
            session.get_variables(0).await.unwrap(),
            serde_json::json!([])
        );
        assert_eq!(
            session.evaluate(0, "x").await.unwrap(),
            serde_json::Value::Null
        );
    }
}
