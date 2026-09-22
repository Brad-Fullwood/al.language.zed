//! Forwarding BC push events to the client.
//!
//! BC sends Break and output events over SignalR at any time, not only in
//! reply to an invocation, so a background task polls for them and writes the
//! DAP events into the same channel the stdio loop drains.

use std::path::{Path, PathBuf};

use super::session::try_configuration_done;
use crate::dap::bc_debug::BcEvent;

use super::{make_event, NativeDapState, ResolvedObject};

impl<F, Fut, R, P, C, CompileFut, A> NativeDapState<F, R, P, C, A>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    R: Fn(&str) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    A: Fn(&Path) -> std::result::Result<Option<PathBuf>, String> + Send + Sync + 'static,
{
    /// Spawn background task to forward BC push events to Zed.
    ///
    /// BC sends Break events via SignalR push at any time (not just in
    /// response to our invocations). This task polls `try_drain_push_events()`
    /// which uses try_lock() on event_rx — if an invoke() is running it skips,
    /// knowing the event will be captured in pending_events and forwarded after
    /// the invoke returns via flush_pending_events().
    ///
    /// Cancellation: send a new value on cancel_tx before spawning a new task
    /// so the old task exits cleanly on reconnect (prevents task leaks).
    pub(super) fn spawn_event_forwarder(&self) {
        let generation = *self.cancel_tx.borrow() + 1;
        let _ = self.cancel_tx.send(generation);
        let mut cancel_rx_clone = self.cancel_rx.clone();
        let session_clone = self.session.clone();
        let event_tx_clone = self.dap_event_tx.clone();
        let seq_clone = self.seq.clone();
        let debug_config_clone = self.debug_config.clone();
        let configured_clone = self.configured.clone();
        tokio::spawn(async move {
            // Snapshot the generation we were spawned in.
            // If cancel_rx_clone sees a newer value, the task exits.
            let my_generation = generation;
            loop {
                if *cancel_rx_clone.borrow() != my_generation {
                    return;
                }

                // Clone the Arc<BcDebugSession> while holding the mutex,
                // then immediately drop the guard so async methods on the
                // session are not called while the mutex is held (deadlock).
                let session_arc = session_clone.lock().await.clone();
                let bc_session = match session_arc {
                    Some(s) => s,
                    None => return, // session ended
                };

                // Retry configurationDone here, exactly like the MCP path's
                // `NativeDebugSession::drain_events` (native_debug.rs:222):
                // current BC online rejects `DebugAdapterConfigurationDone`
                // until `OnAttachedToConnection` fires, which for
                // break-on-next web-client sessions happens only after the
                // browser attaches — i.e. after the client already sent
                // configurationDone once and got rejected. Poll here until
                // it succeeds so accepted breakpoints don't stay inert.
                try_configuration_done(&bc_session, &debug_config_clone, &configured_clone).await;

                // All async calls happen without holding the session mutex.
                let mut bc_events = bc_session.try_drain_push_events().await;
                // Also flush pending events buffered during invoke() calls.
                bc_events.extend(bc_session.flush_pending_events().await);

                for bc_event in bc_events {
                    let dap_evt = match &bc_event {
                        BcEvent::Break {
                            reason,
                            thread_id,
                            text,
                            ..
                        } => {
                            let mut body = serde_json::json!({
                                "reason": reason,
                                "threadId": thread_id,
                                "allThreadsStopped": true,
                            });
                            // DAP's `stopped` event carries exception/error
                            // detail in `text`; surface the Break message BC
                            // sent instead of discarding it.
                            if let Some(text) = text {
                                body["text"] = serde_json::json!(text);
                            }
                            make_event(&seq_clone, "stopped", Some(body))
                        }
                        BcEvent::Detached { terminate } => {
                            if *terminate {
                                make_event(&seq_clone, "terminated", None)
                            } else {
                                continue;
                            }
                        }
                        BcEvent::FatalError { message } => make_event(
                            &seq_clone,
                            "output",
                            Some(serde_json::json!({
                                "category": "stderr",
                                "output": format!("Fatal debugger error: {message}\r\n"),
                            })),
                        ),
                        BcEvent::Other { .. } => continue,
                    };
                    let Ok(body) = serde_json::to_vec(&dap_evt) else {
                        continue;
                    };
                    // try_send + warn-log preserves the producer side's
                    // back-pressure semantics: if Zed is wedged and the
                    // 1024-slot channel fills, drop the event with a log
                    // rather than block this task forever.
                    match event_tx_clone.try_send(body) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            tracing::warn!(
                                "DAP event channel saturated (1024) — \
                                                     dropping event; client appears to be stuck"
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            return; // receiver dropped — DAP server shut down
                        }
                    }
                }

                tokio::select! {
                    _ = cancel_rx_clone.changed() => {
                        if *cancel_rx_clone.borrow() != my_generation {
                            return;
                        }
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(50)) => {}
                }
            }
        });
    }
}
