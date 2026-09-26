use super::*;

// --- SignalR session injection (BcDebugSession::test_new) -----------------
//
// `test_new` wires the same internal channels `connect()` builds but skips
// the negotiate + WebSocket handshake, so these tests drive the REAL
// `invoke()` loop and every public debug operation against injected SignalR
// messages. They assert on both the on-wire request shape (`ws_rx`, the
// channel the writer task drains) and the parsed responses / error branches.

/// Build a type-1 (server-invoked callback) SignalR message, e.g. a
/// `Break`/`IsAlive` push from the server.
fn invocation(target: Option<&str>, arguments: Option<Vec<serde_json::Value>>) -> SignalRMessage {
    SignalRMessage {
        type_: 1,
        target: target.map(|t| t.to_string()),
        arguments,
        invocation_id: None,
        result: None,
        error: None,
    }
}

/// Build a type-3 (completion) SignalR message — the server's response to
/// one of our invocations.
fn completion(id: &str, result: Option<serde_json::Value>, error: Option<&str>) -> SignalRMessage {
    SignalRMessage {
        type_: 3,
        target: None,
        arguments: None,
        invocation_id: Some(id.to_string()),
        result,
        error: error.map(|e| e.to_string()),
    }
}

fn next_frame(rx: &mut mpsc::Receiver<String>) -> serde_json::Value {
    let raw = rx.try_recv().expect("session should have sent a frame");
    serde_json::from_str(&raw).expect("sent frame is valid JSON")
}

/// One task routes every SignalR message, so waiting on the completion
/// channel stops Break too. A hub that answers invocations nobody is
/// waiting for used to fill the 32 slots and the session went silent: no
/// further `stopped` event reached the DAP client, with no error anywhere.
#[tokio::test]
async fn a_full_completion_channel_still_lets_break_through() {
    let (event_tx, _event_rx) = mpsc::channel::<SignalRMessage>(EVENT_CHANNEL_CAPACITY);
    let (completion_tx, _completion_rx) =
        mpsc::channel::<SignalRMessage>(COMPLETION_CHANNEL_CAPACITY);
    let (break_event_tx, mut break_event_rx) = mpsc::unbounded_channel::<bool>();

    // Nothing consumes completions, so the channel fills and stays full.
    for index in 0..COMPLETION_CHANNEL_CAPACITY + 8 {
        let routed = route_signalr_message(
            completion(&format!("stale-{index}"), None, None),
            &event_tx,
            &completion_tx,
            &break_event_tx,
        )
        .await;
        assert!(routed, "the reader must keep running at message {index}");
    }

    let routed = route_signalr_message(
        invocation(Some("Break"), None),
        &event_tx,
        &completion_tx,
        &break_event_tx,
    )
    .await;
    assert!(routed);
    assert_eq!(
        break_event_rx.try_recv().ok(),
        Some(true),
        "Break must still reach its dedicated channel"
    );
}

#[test]
fn websocket_host_header_preserves_port_and_ipv6_brackets() {
    assert_eq!(
        websocket_host_header("wss://bc.example.test:7049/debug?id=x").unwrap(),
        "bc.example.test:7049"
    );
    assert_eq!(
        websocket_host_header("ws://[2001:db8::1]:8080/debug?id=x").unwrap(),
        "[2001:db8::1]:8080"
    );
    assert_eq!(
        websocket_host_header("wss://bc.example.test/debug?id=x").unwrap(),
        "bc.example.test"
    );
}

#[test]
fn handshake_retains_coalesced_signalr_frames() {
    let frames = validate_handshake_and_take_frames(
        "{}\x1e{\"type\":1,\"target\":\"Break\"}\x1e{\"type\":3,\"invocationId\":\"1\"}\x1e",
    )
    .unwrap();
    assert_eq!(frames.len(), 2);
    assert!(frames[0].contains("Break"));
    assert!(frames[1].contains("invocationId"));
}

#[test]
fn invalid_cert_option_builds_only_the_explicit_insecure_connector() {
    assert!(websocket_connector(false).unwrap().is_none());
    assert!(websocket_connector(true).unwrap().is_some());
}

#[tokio::test]
async fn concurrent_invokes_are_serialized_before_send() {
    let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
    let session = Arc::new(session);

    let first_session = Arc::clone(&session);
    let first = tokio::spawn(async move { first_session.invoke("First", vec![]).await });
    let first_frame: serde_json::Value =
        serde_json::from_str(&ws_rx.recv().await.unwrap()).unwrap();
    assert_eq!(first_frame["target"], "First");

    let second_session = Arc::clone(&session);
    let second = tokio::spawn(async move { second_session.invoke("Second", vec![]).await });
    assert!(
        tokio::time::timeout(tokio::time::Duration::from_millis(25), ws_rx.recv())
            .await
            .is_err(),
        "second invoke must not reach the wire before the first completes"
    );

    event_tx
        .send(completion(
            first_frame["invocationId"].as_str().unwrap(),
            Some(serde_json::json!("first")),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        first.await.unwrap().unwrap(),
        Some(serde_json::json!("first"))
    );

    let second_frame: serde_json::Value =
        serde_json::from_str(&ws_rx.recv().await.unwrap()).unwrap();
    assert_eq!(second_frame["target"], "Second");
    event_tx
        .send(completion(
            second_frame["invocationId"].as_str().unwrap(),
            Some(serde_json::json!("second")),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        second.await.unwrap().unwrap(),
        Some(serde_json::json!("second"))
    );
}

#[tokio::test]
async fn push_queue_is_bounded_without_dropping_completions() {
    let (session, event_tx, _b, _ws_rx) = BcDebugSession::test_new("c".into());
    for _ in 0..EVENT_CHANNEL_CAPACITY {
        event_tx
            .try_send(invocation(Some("OnAttachedToConnection"), None))
            .unwrap();
    }
    assert!(
        event_tx
            .try_send(invocation(Some("OnAttachedToConnection"), None))
            .is_err(),
        "push queue must reject overflow"
    );
    event_tx
        .send(completion("1", Some(serde_json::json!("ok")), None))
        .await
        .unwrap();
    assert_eq!(
        session.invoke("StillCompletes", vec![]).await.unwrap(),
        Some(serde_json::json!("ok"))
    );
}

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
async fn break_after_step_reports_reason_step() {
    // A Break that lands after a step_over must surface reason "step",
    // not the default "breakpoint" — BC's own Break callback carries no
    // such distinction, so the session must derive it from the last
    // client action.
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    event_tx
        .send(completion("1", Some(serde_json::json!(null)), None))
        .await
        .unwrap();
    session.step_over().await.expect("step_over");

    event_tx
        .send(invocation(Some("Break"), None))
        .await
        .unwrap();
    let events = session.try_drain_push_events().await;
    assert_eq!(events.len(), 1);
    match &events[0] {
        BcEvent::Break { reason, .. } => assert_eq!(reason, "step"),
        other => panic!("expected Break, got {other:?}"),
    }
}

#[tokio::test]
async fn break_after_continue_reports_reason_breakpoint() {
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    event_tx
        .send(completion("1", Some(serde_json::json!(null)), None))
        .await
        .unwrap();
    session
        .continue_execution(serde_json::json!(0))
        .await
        .expect("continue");

    event_tx
        .send(invocation(Some("Break"), None))
        .await
        .unwrap();
    let events = session.try_drain_push_events().await;
    assert_eq!(events.len(), 1);
    match &events[0] {
        BcEvent::Break { reason, .. } => assert_eq!(reason, "breakpoint"),
        other => panic!("expected Break, got {other:?}"),
    }
}

#[tokio::test]
async fn break_reason_step_is_consumed_by_one_break_only() {
    // After the pending-step Break is drained, a second, unprompted
    // Break must fall back to "breakpoint" rather than staying "step".
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    event_tx
        .send(completion("1", Some(serde_json::json!(null)), None))
        .await
        .unwrap();
    session.step_over().await.expect("step_over");

    event_tx
        .send(invocation(Some("Break"), None))
        .await
        .unwrap();
    event_tx
        .send(invocation(Some("Break"), None))
        .await
        .unwrap();
    let events = session.try_drain_push_events().await;
    assert_eq!(events.len(), 2);
    match &events[0] {
        BcEvent::Break { reason, .. } => assert_eq!(reason, "step"),
        other => panic!("expected Break, got {other:?}"),
    }
    match &events[1] {
        BcEvent::Break { reason, .. } => assert_eq!(reason, "breakpoint"),
        other => panic!("expected Break, got {other:?}"),
    }
}

#[tokio::test]
async fn break_with_message_reports_exception_even_after_step() {
    // An error message takes priority over a pending step: the reason
    // must be "exception" and the text must be surfaced.
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    event_tx
        .send(completion("1", Some(serde_json::json!(null)), None))
        .await
        .unwrap();
    session.step_over().await.expect("step_over");

    let break_args = vec![
        serde_json::Value::Null,
        serde_json::json!([]),
        serde_json::json!("Division by zero"),
    ];
    event_tx
        .send(invocation(Some("Break"), Some(break_args)))
        .await
        .unwrap();
    let events = session.try_drain_push_events().await;
    assert_eq!(events.len(), 1);
    match &events[0] {
        BcEvent::Break { reason, text, .. } => {
            assert_eq!(reason, "exception");
            assert_eq!(text.as_deref(), Some("Division by zero"));
        }
        other => panic!("expected Break, got {other:?}"),
    }
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

#[tokio::test]
async fn invoke_ignores_completion_with_mismatched_invocation_id() {
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    event_tx
        .send(completion("999", Some(serde_json::json!("stale")), None))
        .await
        .unwrap();
    event_tx
        .send(completion("1", Some(serde_json::json!("fresh")), None))
        .await
        .unwrap();
    let r = session.invoke("GetSource", vec![]).await.unwrap();
    assert_eq!(r, Some(serde_json::json!("fresh")));
}

#[tokio::test]
async fn invoke_returns_server_error_on_error_completion() {
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    event_tx
        .send(completion("1", None, Some("AL object is locked")))
        .await
        .unwrap();
    let err = session
        .invoke("RemoveBreakpoint", vec![])
        .await
        .expect_err("error completion must fail the invoke");
    assert!(
        matches!(&err, DapError::ServerError(m) if m.contains("AL object is locked")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn invoke_times_out_when_no_completion_arrives() {
    let (session, _event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    // No completion is ever pushed; _event_tx / _w stay alive so neither
    // channel closes. A short explicit budget exercises the real timeout
    // branch of the invoke loop without waiting a production-length deadline,
    // and asserts the reported duration matches the configured budget.
    let timeout = tokio::time::Duration::from_millis(50);
    let err = session
        .invoke_with_timeout("IsAlive", vec![], timeout)
        .await
        .expect_err("an unanswered invoke must time out");
    match err {
        DapError::Timeout(d) => assert_eq!(d, timeout, "the configured budget is reported"),
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn invoke_errors_when_event_channel_closed() {
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    drop(event_tx); // no senders left → rx.recv() yields None
    let err = session
        .invoke("IsAlive", vec![])
        .await
        .expect_err("a closed event channel must fail the invoke");
    assert!(
        matches!(&err, DapError::ConnectionFailed(m) if m.contains("channel closed")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn invoke_errors_when_ws_channel_closed() {
    let (session, _event_tx, _b, ws_rx) = BcDebugSession::test_new("c".into());
    drop(ws_rx); // the writer side is gone → ws_tx.send() fails immediately
    let err = session
        .invoke("IsAlive", vec![])
        .await
        .expect_err("a closed ws channel must fail the invoke");
    assert!(
        matches!(&err, DapError::ConnectionFailed(m) if m.contains("WebSocket channel closed")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn invoke_buffers_server_push_events_for_flush() {
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    // A Break callback arrives BEFORE our completion while invoke holds
    // event_rx — it must be buffered, not dropped, then drained by flush.
    let break_args = vec![
        serde_json::Value::Null,
        serde_json::json!([{ "DisplayName": "OnRun", "SourcePosition": { "Line": 10, "Column": 2 } }]),
        serde_json::json!(""),
    ];
    event_tx
        .send(invocation(Some("Break"), Some(break_args)))
        .await
        .unwrap();
    event_tx
        .send(completion("1", Some(serde_json::json!(null)), None))
        .await
        .unwrap();

    session.invoke("GetStackTrace", vec![]).await.unwrap();

    let events = session.flush_pending_events().await;
    assert_eq!(events.len(), 1, "exactly the buffered Break is flushed");
    match &events[0] {
        BcEvent::Break {
            reason, location, ..
        } => {
            assert_eq!(reason, "breakpoint");
            let loc = location.as_ref().expect("break location extracted");
            assert_eq!(loc.line, 10);
        }
        other => panic!("expected Break, got {other:?}"),
    }
    // flush_pending_events also runs handle_server_callback("Break").
    assert!(session.is_stopped().await, "Break dispatch set is_stopped");
}

#[tokio::test]
async fn try_drain_push_events_drains_type1_and_ignores_completions() {
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    // A stray type-3 completion (no pending invoke) is ignored, while the
    // type-1 Break callback is converted and returned.
    event_tx
        .send(completion("99", Some(serde_json::json!("ignored")), None))
        .await
        .unwrap();
    event_tx
        .send(invocation(Some("Break"), None))
        .await
        .unwrap();
    let events = session.try_drain_push_events().await;
    assert_eq!(events.len(), 1, "only the type-1 Break yields an event");
    assert!(matches!(&events[0], BcEvent::Break { .. }));
    assert!(
        session.is_stopped().await,
        "Break dispatch flips is_stopped"
    );
}

#[tokio::test]
async fn try_drain_push_events_yields_nothing_while_event_rx_is_locked() {
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    event_tx
        .send(invocation(Some("Break"), None))
        .await
        .unwrap();
    {
        // Simulate an in-flight invoke() holding event_rx: try_drain must
        // use try_lock and bail out empty rather than block.
        let _guard = session.event_rx.lock().await;
        assert!(
            session.try_drain_push_events().await.is_empty(),
            "a contended drain must return empty without consuming"
        );
    }
    let events = session.try_drain_push_events().await;
    assert_eq!(
        events.len(),
        1,
        "event preserved while contended, drained after"
    );
}

#[tokio::test]
async fn wait_for_break_event_returns_true_on_break_false_on_end() {
    let (session, _e, break_tx, _w) = BcDebugSession::test_new("c".into());
    break_tx.send(true).unwrap();
    assert!(session.wait_for_break_event().await, "Break signal => true");
    break_tx.send(false).unwrap();
    assert!(
        !session.wait_for_break_event().await,
        "session-end signal => false"
    );
}

#[tokio::test]
async fn wait_for_break_event_returns_false_when_channel_closed() {
    let (session, _e, break_tx, _w) = BcDebugSession::test_new("c".into());
    drop(break_tx); // reader task gone → recv None → default false
    assert!(!session.wait_for_break_event().await);
}

#[tokio::test]
async fn handle_server_callback_isalive_sends_acknowledge() {
    let (session, _e, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
    session
        .handle_server_callback(&invocation(Some("IsAlive"), None))
        .await;
    let frame = next_frame(&mut ws_rx);
    assert_eq!(frame["target"], "AcknowledgeIsAlive");
    assert_eq!(frame["type"], 1);
}

#[tokio::test]
async fn is_alive_true_on_completion_false_on_error() {
    let (session, event_tx, _b, _w) = BcDebugSession::test_new("c".into());
    event_tx
        .send(completion("1", Some(serde_json::json!(null)), None))
        .await
        .unwrap();
    assert!(session.is_alive().await, "a completion => alive");

    let (session2, event_tx2, _b2, _w2) = BcDebugSession::test_new("c".into());
    event_tx2
        .send(completion("1", None, Some("dead")))
        .await
        .unwrap();
    assert!(!session2.is_alive().await, "a server error => not alive");
}
