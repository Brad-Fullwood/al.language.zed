//! Test-only fake BC debug hub built on [`BcDebugSession::test_new`].
//!
//! `test_new` is module-private and exposes the private `SignalRMessage` type,
//! so sibling modules (e.g. `native_debug`'s unit tests) cannot drive a fake
//! session through it directly. This helper wraps those raw channels behind a
//! `serde_json::Value`-only API and spawns a responder task that mirrors the
//! live WebSocket reader/writer: it records every frame the session sends and
//! replies to each `invoke` with a queued canned completion (FIFO per target),
//! exactly as the real SignalR reader would feed `event_rx`. Server-push
//! callbacks (e.g. `Break`) are injected out-of-band into the same channel.
//!
//! Behaviour-preserving: `#[cfg(test)]` only, spawns no production code, and
//! touches no live code path — it merely feeds the existing channels.

use super::{BcDebugSession, SignalRMessage, TestSignalRTx};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// A canned reply the fake hub returns for the next `invoke` of a target.
enum Reply {
    Ok(serde_json::Value),
    Err(String),
}

/// Handle to a fake BC debug hub. See the module doc.
pub(crate) struct FakeBc {
    event_tx: TestSignalRTx,
    replies: Arc<Mutex<HashMap<String, VecDeque<Reply>>>>,
    sent: Arc<Mutex<Vec<serde_json::Value>>>,
    _responder: tokio::task::JoinHandle<()>,
}

impl FakeBc {
    /// Build a fake session plus its hub handle. The hub's responder task
    /// auto-answers every `invoke` from the queued replies for that target.
    pub(crate) fn start(connection_id: &str) -> (BcDebugSession, FakeBc) {
        let (session, event_tx, _break_event_tx, mut ws_rx) =
            BcDebugSession::test_new(connection_id.to_string());
        let replies: Arc<Mutex<HashMap<String, VecDeque<Reply>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let sent: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

        let replies_task = Arc::clone(&replies);
        let sent_task = Arc::clone(&sent);
        let event_tx_task = event_tx.clone();
        let responder = tokio::spawn(async move {
            // Mirror the live writer-drain + reader-feed loop: read each
            // frame the session emits, record it, and push a completion.
            while let Some(frame) = ws_rx.recv().await {
                let v: serde_json::Value = match serde_json::from_str(&frame) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = v
                    .get("invocationId")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let target = v
                    .get("target")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                sent_task.lock().unwrap().push(v);
                // Side-channel sends (e.g. AcknowledgeIsAlive) carry no
                // invocationId and expect no completion.
                if id.is_empty() {
                    continue;
                }
                let reply = {
                    let mut map = replies_task.lock().unwrap();
                    map.get_mut(&target).and_then(|q| q.pop_front())
                };
                let msg = match reply {
                    Some(Reply::Ok(r)) => completion(&id, Some(r), None),
                    Some(Reply::Err(e)) => completion(&id, None, Some(&e)),
                    // No queued reply → an empty completion, i.e. the same
                    // `Ok(None)` the real hub sends for a fire-and-forget op.
                    None => completion(&id, None, None),
                };
                if event_tx_task.send(msg).await.is_err() {
                    break;
                }
            }
        });

        (
            session,
            FakeBc {
                event_tx,
                replies,
                sent,
                _responder: responder,
            },
        )
    }

    /// Queue a successful completion for the next `invoke` of `target` (FIFO).
    pub(crate) fn reply_ok(&self, target: &str, result: serde_json::Value) {
        self.replies
            .lock()
            .unwrap()
            .entry(target.to_string())
            .or_default()
            .push_back(Reply::Ok(result));
    }

    /// Queue an error completion for the next `invoke` of `target` (FIFO).
    pub(crate) fn reply_err(&self, target: &str, error: &str) {
        self.replies
            .lock()
            .unwrap()
            .entry(target.to_string())
            .or_default()
            .push_back(Reply::Err(error.to_string()));
    }

    /// Inject a server-push type-1 callback (e.g. `"Break"`) into the event
    /// channel, as the live WebSocket reader would on a hub callback.
    /// `arguments` is the SignalR `arguments` array; a non-array value is
    /// wrapped into a single-element array, and `null` becomes no arguments.
    pub(crate) fn push_callback(&self, target: &str, arguments: serde_json::Value) {
        let args = match arguments {
            serde_json::Value::Array(a) => Some(a),
            serde_json::Value::Null => None,
            other => Some(vec![other]),
        };
        let msg = SignalRMessage {
            type_: 1,
            target: Some(target.to_string()),
            arguments: args,
            invocation_id: None,
            result: None,
            error: None,
        };
        self.event_tx
            .try_send(msg)
            .expect("fake event channel has capacity");
    }

    /// All JSON frames the session has sent so far, in order — for
    /// request-shape assertions.
    pub(crate) fn sent_frames(&self) -> Vec<serde_json::Value> {
        self.sent.lock().unwrap().clone()
    }
}

/// Build a type-3 (completion) SignalR message — the hub's reply to an invoke.
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
