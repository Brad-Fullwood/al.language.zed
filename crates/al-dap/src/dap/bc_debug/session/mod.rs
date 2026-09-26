//! Native BC debug session over SignalR.
//!
//! [`connect`] does the negotiate and WebSocket handshake, [`invoke`] carries
//! the request and response loop and the server-push events that arrive
//! alongside it, and [`commands`] holds the debug operations a DAP handler
//! calls. This file holds the session state, the channel capacities and the
//! reader-side routing that decides which channel a frame goes to.

pub(super) mod commands;
pub(super) mod connect;
pub(super) mod invoke;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
pub(crate) use test_support::fake;

use std::sync::atomic::AtomicI64;

use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};

use super::wire::SignalRMessage;

/// Capacity of the channel carrying SignalR completion replies to `invoke`.
///
/// Exactly one `invoke` consumes from it at a time and it discards any
/// invocation id it did not ask for, so replies queued beyond the one in
/// flight are stale by construction. The reader never waits on this channel:
/// see [`route_signalr_message`].
const COMPLETION_CHANNEL_CAPACITY: usize = 32;

/// Capacity of the SignalR event channel that the reader task forwards
/// server-push messages into. A misbehaving (or malicious) BC server
/// flooding the daemon used to grow this channel unboundedly, since the
/// previous channel was `mpsc::unbounded_channel`.
///
/// 4096 messages × ~few-KB-each = ~MB-scale bound. Variable-expansion
/// responses can be large (deep AL records); Break / step-complete events
/// are small. If the channel ever fills, the reader task drops the
/// offending push message with a `warn!` log — that's preferable to OOM.
/// Break events have a *separate* dedicated channel (`break_event_*`)
/// that stays unbounded because each entry is a single `bool` and a
/// dropped Break event leaves the debugger silently stuck.
const EVENT_CHANNEL_CAPACITY: usize = 4096;

async fn route_signalr_message(
    msg: SignalRMessage,
    event_tx: &mpsc::Sender<SignalRMessage>,
    completion_tx: &mpsc::Sender<SignalRMessage>,
    break_event_tx: &mpsc::UnboundedSender<bool>,
) -> bool {
    if msg.type_ == 6 {
        return true;
    }
    debug!(
        "SignalR recv: type={} target={:?} id={:?}",
        msg.type_, msg.target, msg.invocation_id
    );

    if msg.type_ == 1 {
        let break_state = match msg.target.as_deref() {
            Some("Break") => Some(true),
            Some("OnDetachedFromConnection" | "OnFatalDebuggerException") => Some(false),
            _ => None,
        };
        if let Some(state) = break_state {
            if break_event_tx.send(state).is_err() {
                debug!("break-event listener has gone away");
            }
        }
    }

    if msg.type_ == 3 {
        // One reader task routes everything, so waiting here stops Break,
        // OnDetachedFromConnection and IsAlive as well. A hub that answers an
        // invocation twice, or answers one the client already timed out on,
        // fills the channel with replies nobody is waiting for, and the
        // session used to go silent with no error: the DAP client simply never
        // saw another `stopped` event. Dropping the surplus costs at worst one
        // invoke timeout.
        return match completion_tx.try_send(msg) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(msg)) => {
                warn!(
                    invocation_id = ?msg.invocation_id,
                    cap = COMPLETION_CHANNEL_CAPACITY,
                    "SignalR completion channel full — dropping a reply nothing is waiting on"
                );
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        };
    }

    match event_tx.try_send(msg) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!(
                cap = EVENT_CHANNEL_CAPACITY,
                "SignalR push-event channel full — dropping callback"
            );
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

/// Native BC debug session over SignalR.
pub struct BcDebugSession {
    ws_tx: mpsc::Sender<String>,
    /// Bounded server-push queue. Overflow may discard ordinary callbacks, but
    /// completion replies and Break notifications use dedicated channels.
    event_rx: Mutex<mpsc::Receiver<SignalRMessage>>,
    /// Completion replies are back-pressured, never dropped, and consumed by
    /// exactly one serialized `invoke()` at a time.
    completion_rx: Mutex<mpsc::Receiver<SignalRMessage>>,
    next_id: AtomicI64,
    /// SignalR connection ID — used in browser URL for debug context
    pub connection_id: String,
    /// True after BC confirms that a concrete client session has attached.
    /// `Attach` itself only registers the debugger; configurationDone must be
    /// deferred until this callback for break-on-next web-client sessions.
    is_attached: Mutex<bool>,
    is_stopped: Mutex<bool>,
    /// True when the most recent `SetBreakpointResponse` sent to BC carried a
    /// step exit reason (over/in/out) rather than plain continue (0). Read by
    /// `signalr_to_bc_event` to derive the next `Break`'s `stopped` reason
    /// ("step" vs "breakpoint") — BC's own `Break` callback carries no such
    /// distinction. Reset on every `continue_execution` call.
    expecting_step: Mutex<bool>,
    /// Receives `true` when a Break event arrives and `false` when the session
    /// ends (Detached or FatalError). Populated by the WebSocket reader task,
    /// which sends without holding any lock — no deadlock risk. The channel is
    /// unbounded so events are never lost even if they arrive before the
    /// receiver calls `wait_for_break_event`.
    break_event_rx: Mutex<mpsc::UnboundedReceiver<bool>>,
}

impl BcDebugSession {
    /// Whether BC has bound this debugger connection to a concrete NST client
    /// session. For break-on-next attaches this becomes true asynchronously
    /// through `OnAttachedToConnection` after the debug browser is opened.
    pub async fn is_attached(&self) -> bool {
        *self.is_attached.lock().await
    }

    /// Calls `invoke()`, which acquires `completion_rx`. Only safe to call when no
    /// other `invoke()` is in progress (i.e. outside of an active debug loop).
    /// Server-side `IsAlive` pings during a session are handled automatically
    /// via `try_send` in `handle_server_callback`.
    pub async fn is_alive(&self) -> bool {
        self.invoke("IsAlive", vec![]).await.is_ok()
    }

    pub async fn is_stopped(&self) -> bool {
        *self.is_stopped.lock().await
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    use super::test_support::{completion, invocation};

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
}
