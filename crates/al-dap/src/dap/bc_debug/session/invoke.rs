//! The SignalR request and response loop, and the events that arrive with it.
//!
//! One `invoke` runs at a time, holding the completion receiver for the reply
//! it is waiting on. Server-push callbacks that arrive meanwhile are buffered
//! and flushed after the reply, so a Break raised during a variable fetch is
//! not lost.

use std::sync::atomic::Ordering;

use tracing::{debug, error, info, warn};

use crate::dap::{DapError, Result};

use super::super::events::{fatal_exception_message, signalr_to_bc_event, BcEvent};
use super::super::wire::{default_invoke_timeout, SignalRMessage};
use super::BcDebugSession;
impl BcDebugSession {
    /// Invoke a SignalR method and wait for completion.
    ///
    /// Acquires `completion_rx` before sending. Concurrent calls therefore
    /// serialize on the wire as required by BC, rather than both sending and
    /// racing to consume one another's replies.
    pub(super) async fn invoke(
        &self,
        target: &str,
        arguments: Vec<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>> {
        self.invoke_with_timeout(target, arguments, default_invoke_timeout(target))
            .await
    }

    pub(super) async fn invoke_with_timeout(
        &self,
        target: &str,
        arguments: Vec<serde_json::Value>,
        timeout: tokio::time::Duration,
    ) -> Result<Option<serde_json::Value>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();

        let msg = serde_json::json!({
            "type": 1,
            "target": target,
            "arguments": arguments,
            "invocationId": id,
        });

        // Log only the invocation target at INFO. The argument payload
        // can include breakpoint paths, attach metadata and other
        // potentially sensitive content; keep it at DEBUG.
        info!("SignalR invoke: {target} (timeout {}s)", timeout.as_secs());
        debug!(
            target = %target,
            args = %serde_json::to_string(&arguments).unwrap_or_default(),
            "SignalR invoke arguments"
        );
        // Compute the deadline BEFORE the lock + send so the budget
        // declared by `default_invoke_timeout` is faithful even when another
        // invoke is holding `event_rx` for its own (potentially 120s `Attach`)
        // window. Previously the deadline was anchored after the lock was
        // granted, so a queued caller's effective timeout silently extended
        // by however long it waited for the mutex.
        let deadline = tokio::time::Instant::now() + timeout;
        let mut rx = tokio::time::timeout_at(deadline, self.completion_rx.lock())
            .await
            .map_err(|_| DapError::Timeout(timeout))?;
        tokio::time::timeout_at(deadline, self.ws_tx.send(msg.to_string()))
            .await
            .map_err(|_| DapError::Timeout(timeout))?
            .map_err(|_| DapError::ConnectionFailed("WebSocket channel closed".to_string()))?;

        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(msg)) => {
                    if msg.invocation_id.as_deref() == Some(&id) {
                        if let Some(ref err) = msg.error {
                            error!("SignalR error for {target}: {err}");
                            return Err(DapError::ServerError(err.clone()));
                        }
                        debug!("SignalR result for {target}: {:?}", msg.result);
                        return Ok(msg.result);
                    }
                    warn!(
                        expected = %id,
                        actual = ?msg.invocation_id,
                        "ignoring stale SignalR completion"
                    );
                }
                Ok(None) => {
                    return Err(DapError::ConnectionFailed(
                        "SignalR channel closed".to_string(),
                    ))
                }
                Err(_) => return Err(DapError::Timeout(timeout)),
            }
        }
    }

    /// BC sends these hub client callbacks:
    /// - `Break(ApplicationObjectIdWrapper, StackFrame[], string)` — execution stopped
    /// - `IsAlive` — ping, must respond with AcknowledgeIsAlive
    /// - `OnAttachedToConnection` — connected to debug session
    /// - `OnDetachedFromConnection(bool terminateSession)` — disconnected
    /// - `OnFatalDebuggerException(string message)` — fatal error
    pub(super) async fn handle_server_callback(&self, msg: &SignalRMessage) {
        if let Some(target) = &msg.target {
            match target.as_str() {
                "Break" => {
                    info!("Debug Break event received");
                    *self.is_stopped.lock().await = true;
                    // args: [ApplicationObjectIdWrapper, StackFrame[], message]
                    if let Some(args) = &msg.arguments {
                        if args.len() >= 3 {
                            let message = args[2].as_str().unwrap_or("");
                            if !message.is_empty() {
                                debug!("Break message: {message}");
                            }
                        }
                    }
                }
                "IsAlive" => {
                    debug!("IsAlive ping from server");
                    let ack = serde_json::json!({
                        "type": 1,
                        "target": "AcknowledgeIsAlive",
                        "arguments": [],
                    });
                    // Ping — respond with try_send to avoid blocking while event_rx is held.
                    // If the send channel is full, the ping is silently dropped; BC will
                    // retry. Using .await here would deadlock when invoke() holds event_rx.
                    let _ = self.ws_tx.try_send(ack.to_string());
                }
                "OnAttachedToConnection" => {
                    info!("Attached to debug connection");
                    *self.is_attached.lock().await = true;
                }
                "OnDetachedFromConnection" => {
                    let terminate = msg
                        .arguments
                        .as_ref()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    info!("Detached from debug connection (terminate={terminate})");
                    *self.is_attached.lock().await = false;
                }
                "OnFatalDebuggerException" => {
                    let message = fatal_exception_message(&msg.arguments);
                    error!("Fatal debugger exception: {message}");
                }
                _ => {
                    debug!("Unhandled server callback: {target}");
                }
            }
        }
    }

    /// Convert one raw callback to a `BcEvent`, consulting (and, for `Break`,
    /// resetting) `expecting_step` so a Break's reason reflects the most
    /// recent client action exactly once.
    pub(super) async fn convert_event(&self, msg: &SignalRMessage) -> Option<BcEvent> {
        let expecting_step = *self.expecting_step.lock().await;
        let event = signalr_to_bc_event(msg, expecting_step);
        if matches!(event, Some(BcEvent::Break { .. })) {
            *self.expecting_step.lock().await = false;
        }
        event
    }

    /// Process server-push events queued while an invocation or other operation
    /// was in progress.
    ///
    /// Returns processed `BcEvent` values so callers can convert them to DAP events.
    pub async fn flush_pending_events(&self) -> Vec<BcEvent> {
        let raw: Vec<SignalRMessage> = {
            let mut rx = self.event_rx.lock().await;
            let mut events = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                events.push(msg);
            }
            events
        };
        let mut out = Vec::new();
        for event in &raw {
            self.handle_server_callback(event).await;
            if let Some(bc_event) = self.convert_event(event).await {
                out.push(bc_event);
            }
        }
        out
    }

    /// Try to read any immediately available server-push events from the SignalR channel
    /// without blocking. Used by the background event-forwarding task to check for BC
    /// push events (e.g. Break) that arrive while no `invoke()` is in progress.
    ///
    /// Returns processed `BcEvent` values. May return an empty Vec if another
    /// drain is in progress or no events are available.
    pub async fn try_drain_push_events(&self) -> Vec<BcEvent> {
        let mut out = Vec::new();
        // Use try_lock so this never blocks another push-event drain.
        let mut rx = match self.event_rx.try_lock() {
            Ok(r) => r,
            Err(_) => return out,
        };
        while let Ok(msg) = rx.try_recv() {
            if msg.type_ == 1 {
                self.handle_server_callback(&msg).await;
                if let Some(bc_event) = self.convert_event(&msg).await {
                    out.push(bc_event);
                }
            }
        }
        out
    }

    /// Block until a `Break`, `Detached`, or `FatalError` event arrives from BC.
    ///
    /// Returns `true` if execution stopped at a breakpoint (`Break` event) and
    /// `false` if the session ended cleanly or fatally. Uses the dedicated
    /// `break_event_rx` channel populated by the WebSocket reader task, so:
    ///
    /// - No polling loop — the caller yields until BC pushes an event.
    /// - No race: the channel is unbounded, so a `Break` that arrives before
    ///   this method is called is buffered and returned on the first `recv`.
    pub async fn wait_for_break_event(&self) -> bool {
        let mut rx = self.break_event_rx.lock().await;
        rx.recv().await.unwrap_or(false)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use super::super::test_support::{completion, invocation, next_frame};
    use super::super::EVENT_CHANNEL_CAPACITY;

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
    pub(super) async fn invoke_ignores_completion_with_mismatched_invocation_id() {
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
    pub(super) async fn invoke_returns_server_error_on_error_completion() {
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
    pub(super) async fn invoke_times_out_when_no_completion_arrives() {
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
    pub(super) async fn invoke_errors_when_event_channel_closed() {
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
    pub(super) async fn invoke_errors_when_ws_channel_closed() {
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
    pub(super) async fn invoke_buffers_server_push_events_for_flush() {
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
    pub(super) async fn handle_server_callback_isalive_sends_acknowledge() {
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
}
