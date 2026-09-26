//! Driving a session without a BC server.
//!
//! [`BcDebugSession::test_new`] wires the same channels `connect` builds and
//! skips the handshake, so a test drives the real `invoke` loop against
//! injected frames. [`fake`] wraps those raw channels for sibling modules that
//! cannot name the private message type.

use std::sync::atomic::AtomicI64;

use tokio::sync::{mpsc, Mutex};

use super::super::wire::SignalRMessage;
use super::{BcDebugSession, COMPLETION_CHANNEL_CAPACITY, EVENT_CHANNEL_CAPACITY};

#[derive(Clone)]
pub(super) struct TestSignalRTx {
    event_tx: mpsc::Sender<SignalRMessage>,
    completion_tx: mpsc::Sender<SignalRMessage>,
}

#[cfg(test)]
impl TestSignalRTx {
    pub(super) async fn send(&self, msg: SignalRMessage) -> std::result::Result<(), ()> {
        if msg.type_ == 3 {
            self.completion_tx.send(msg).await.map_err(|_| ())
        } else {
            self.event_tx.send(msg).await.map_err(|_| ())
        }
    }

    pub(super) fn try_send(&self, msg: SignalRMessage) -> std::result::Result<(), ()> {
        if msg.type_ == 3 {
            self.completion_tx.try_send(msg).map_err(|_| ())
        } else {
            self.event_tx.try_send(msg).map_err(|_| ())
        }
    }
}

impl BcDebugSession {
    /// Test-only constructor that bypasses the SignalR negotiate + WebSocket
    /// handshake performed by [`BcDebugSession::connect`], wiring up the exact
    /// same internal channels so unit tests can drive `invoke()` and the public
    /// debug operations against injected `SignalRMessage` responses.
    ///
    /// Returns the session plus three injection handles:
    /// - `event_tx`: push `SignalRMessage`s (type-3 completions and type-1
    ///   server-push callbacks) that the session's `invoke()` / drain paths
    ///   consume — i.e. the channel the real WebSocket *reader* task feeds.
    /// - `break_event_tx`: signal `wait_for_break_event()` (`true` = Break,
    ///   `false` = session end) — the channel the reader task feeds.
    /// - `ws_rx`: receives the JSON frames the session *sends* (the channel the
    ///   real WebSocket *writer* task drains), so tests can assert the on-wire
    ///   request shape.
    ///
    /// Behaviour-preserving: compiled only under `#[cfg(test)]`, spawns no
    /// tasks, and constructs the struct with the identical field initialisers
    /// `connect()` uses. It introduces no new runtime code path.
    ///
    /// Module-private (not `pub`): the in-file `tests` child module can reach it
    /// while the private `SignalRMessage` type stays unexposed (no
    /// `private_interfaces` leak).
    pub(super) fn test_new(
        connection_id: String,
    ) -> (
        Self,
        TestSignalRTx,
        mpsc::UnboundedSender<bool>,
        mpsc::Receiver<String>,
    ) {
        let (ws_tx, ws_rx) = mpsc::channel::<String>(32);
        let (event_tx, event_rx) = mpsc::channel::<SignalRMessage>(EVENT_CHANNEL_CAPACITY);
        let (completion_tx, completion_rx) =
            mpsc::channel::<SignalRMessage>(COMPLETION_CHANNEL_CAPACITY);
        let (break_event_tx, break_event_rx) = mpsc::unbounded_channel::<bool>();
        let session = Self {
            ws_tx,
            event_rx: Mutex::new(event_rx),
            completion_rx: Mutex::new(completion_rx),
            next_id: AtomicI64::new(1),
            connection_id,
            is_attached: Mutex::new(false),
            is_stopped: Mutex::new(false),
            expecting_step: Mutex::new(false),
            break_event_rx: Mutex::new(break_event_rx),
        };
        (
            session,
            TestSignalRTx {
                event_tx,
                completion_tx,
            },
            break_event_tx,
            ws_rx,
        )
    }
}

/// Test-only fake BC debug hub built on [`BcDebugSession::test_new`].
///
/// `test_new` is module-private and exposes the private `SignalRMessage` type,
/// so sibling modules (e.g. `native_debug`'s unit tests) cannot drive a fake
/// session through it directly. This helper wraps those raw channels behind a
/// `serde_json::Value`-only API and spawns a responder task that mirrors the
/// live WebSocket reader/writer: it records every frame the session sends and
/// replies to each `invoke` with a queued canned completion (FIFO per target),
/// exactly as the real SignalR reader would feed `event_rx`. Server-push
/// callbacks (e.g. `Break`) are injected out-of-band into the same channel.
///
/// Behaviour-preserving: `#[cfg(test)]` only, spawns no production code, and
/// touches no live code path — it merely feeds the existing channels.
#[cfg(test)]
pub(crate) mod fake {
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
    fn completion(
        id: &str,
        result: Option<serde_json::Value>,
        error: Option<&str>,
    ) -> SignalRMessage {
        SignalRMessage {
            type_: 3,
            target: None,
            arguments: None,
            invocation_id: Some(id.to_string()),
            result,
            error: error.map(|e| e.to_string()),
        }
    }
}

/// Build a type-1 (server-invoked callback) SignalR message, e.g. a
/// `Break`/`IsAlive` push from the server.
pub(super) fn invocation(
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

/// Build a type-3 (completion) SignalR message — the server's response to
/// one of our invocations.
pub(super) fn completion(
    id: &str,
    result: Option<serde_json::Value>,
    error: Option<&str>,
) -> SignalRMessage {
    SignalRMessage {
        type_: 3,
        target: None,
        arguments: None,
        invocation_id: Some(id.to_string()),
        result,
        error: error.map(|e| e.to_string()),
    }
}

pub(super) fn next_frame(rx: &mut mpsc::Receiver<String>) -> serde_json::Value {
    let raw = rx.try_recv().expect("session should have sent a frame");
    serde_json::from_str(&raw).expect("sent frame is valid JSON")
}
