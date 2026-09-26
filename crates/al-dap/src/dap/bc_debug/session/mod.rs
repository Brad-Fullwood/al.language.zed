//! Native BC debug session over SignalR: connection setup, the SignalR
//! `invoke()` request/response loop, and every public debug operation.
//! Split out of the former monolithic `bc_debug.rs` (pure move, no behavior change).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use reqwest::header::AUTHORIZATION;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use crate::dap::{DapError, Result};

use super::events::{fatal_exception_message, signalr_to_bc_event, BcEvent};
use super::session_config::BcDebugConfig;
use super::wire::{
    default_invoke_timeout, percent_encode_url, redact_connection_token,
    resolve_negotiate_connection, validate_signalr_handshake_response, SignalRMessage,
};

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

mod transport;

use transport::*;

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
    pub async fn connect(config: &BcDebugConfig, access_token: &str) -> Result<Self> {
        if config.accept_invalid_certs {
            // Match the warn-on-construction parity from BcClient::new at
            // bc_client.rs:99. Without this the DAP path silently
            // disables TLS certificate validation when launch.json sets
            // accept_invalid_certs=true.
            al_bc::http_auth::warn_insecure_tls("DAP SignalR debug");
        }
        let hub_url = config.debug_hub_url();

        let negotiate_url = format!("{hub_url}/negotiate?negotiateVersion=1");
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .build()
            .map_err(|e| DapError::ConnectionFailed(format!("HTTP client error: {e}")))?;

        info!("SignalR negotiate: {negotiate_url}");
        let negotiate_resp = http
            .post(&negotiate_url)
            .header(AUTHORIZATION, format!("Bearer {access_token}"))
            .header("Content-Length", "0")
            .body("")
            .send()
            .await
            .map_err(|e| DapError::ConnectionFailed(format!("SignalR negotiate failed: {e}")))?;

        let status = negotiate_resp.status();
        let resp_text = negotiate_resp.text().await.map_err(|e| {
            DapError::ConnectionFailed(format!("Failed to read negotiate response: {e}"))
        })?;
        let redacted = redact_connection_token(&resp_text);
        debug!(
            "Negotiate response (HTTP {}): {}",
            status,
            &redacted[..redacted.len().min(500)]
        );

        if !status.is_success() {
            return Err(DapError::ConnectionFailed(format!(
                "SignalR negotiate failed (HTTP {status}): {}",
                redact_connection_token(&resp_text)
            )));
        }

        let negotiate: serde_json::Value = serde_json::from_str(&resp_text).map_err(|e| {
            let redacted = redact_connection_token(&resp_text);
            DapError::ConnectionFailed(format!(
                "Bad negotiate JSON: {e}: {}",
                &redacted[..redacted.len().min(200)]
            ))
        })?;

        // Resolve the WebSocket connection identifier from the negotiate
        // response, validating the version the server actually negotiated.
        // We request `negotiateVersion=1`; a spec-compliant
        // server echoes the version it agreed to and, for v1, returns a
        // `connectionToken` distinct from `connectionId`. A server that
        // negotiates down to v0 returns no `connectionToken` and the
        // `connectionId` doubles as the `?id=` value.
        let resolved = resolve_negotiate_connection(&negotiate)?;
        let connection_id = resolved.connection_id;

        let ws_url = hub_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        let ws_url = format!("{ws_url}?id={}", percent_encode_url(&resolved.ws_id));
        // Redact the connection_token from the log line — it grants access to
        // the active debug session and must not appear in plaintext logs.
        let log_url = ws_url.split('?').next().unwrap_or(&ws_url);
        info!("SignalR WebSocket: {log_url}?id=<redacted>");

        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&ws_url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("Host", websocket_host_header(&ws_url)?)
            .body(())
            .map_err(|e| DapError::ConnectionFailed(format!("WS request build error: {e}")))?;

        let connector = websocket_connector(config.accept_invalid_certs)?;
        let (ws_stream, _) =
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector)
                .await
                .map_err(|e| {
                    DapError::ConnectionFailed(format!("WebSocket connect failed: {e}"))
                })?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        let handshake = "{\"protocol\":\"json\",\"version\":1}\x1e";
        ws_sink
            .send(tokio_tungstenite::tungstenite::Message::Text(
                handshake.into(),
            ))
            .await
            .map_err(|e| DapError::ConnectionFailed(format!("SignalR handshake failed: {e}")))?;

        // Read handshake response. A SignalR server signals a
        // protocol/version mismatch here via `{"error":...}`; validate it so a
        // rejected handshake fails loudly instead of limping on against an
        // adapter that will misbehave on every later invoke.
        let initial_frames = if let Some(msg) = ws_source.next().await {
            let msg = msg.map_err(|e| DapError::ConnectionFailed(format!("WS read error: {e}")))?;
            debug!("SignalR handshake response: {:?}", msg);
            if let tokio_tungstenite::tungstenite::Message::Text(text) = &msg {
                validate_handshake_and_take_frames(text)?
            } else {
                Vec::new()
            }
        } else {
            return Err(DapError::ConnectionFailed(
                "WebSocket closed before SignalR handshake response".to_string(),
            ));
        };

        info!("SignalR connection established");

        let (ws_tx, mut ws_rx) = mpsc::channel::<String>(32);
        // Deep-but-bounded event channel. Break events take a dedicated
        // unbounded path below to preserve the "Break must never be lost"
        // invariant; everything else drops with a warn on overflow so a
        // hostile or misbehaving server can't OOM the daemon.
        let (event_tx, event_rx) = mpsc::channel::<SignalRMessage>(EVENT_CHANNEL_CAPACITY);
        let (completion_tx, completion_rx) =
            mpsc::channel::<SignalRMessage>(COMPLETION_CHANNEL_CAPACITY);
        // Dedicated channel for Break/Detached/FatalError notifications.
        // Using an unbounded channel ensures events buffered before wait_for_break_event
        // is called are never lost. Each entry is one `bool`, so even a
        // pathological 1M-deep backlog is ~1 MB — bounded in practice by
        // the number of break events the BC server emits in one session.
        let (break_event_tx, break_event_rx) = mpsc::unbounded_channel::<bool>();

        tokio::spawn(async move {
            while let Some(msg) = ws_rx.recv().await {
                let framed = format!("{msg}\x1e"); // SignalR record separator
                if let Err(e) = ws_sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(framed.into()))
                    .await
                {
                    error!("BC SignalR send failed: {e} — debug stream dead");
                    break;
                }
            }
        });

        tokio::spawn(async move {
            for part in initial_frames {
                match serde_json::from_str::<SignalRMessage>(&part) {
                    Ok(msg) => {
                        if !route_signalr_message(msg, &event_tx, &completion_tx, &break_event_tx)
                            .await
                        {
                            return;
                        }
                    }
                    Err(e) => warn!("Failed to parse coalesced SignalR message: {e}: {part}"),
                }
            }

            while let Some(msg) = ws_source.next().await {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        error!("WS read error: {e}");
                        break;
                    }
                };

                let text = match msg {
                    tokio_tungstenite::tungstenite::Message::Text(t) => t,
                    tokio_tungstenite::tungstenite::Message::Ping(_) => continue,
                    tokio_tungstenite::tungstenite::Message::Close(_) => break,
                    _ => continue,
                };

                for part in text.split('\x1e') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<SignalRMessage>(part) {
                        Ok(msg) => {
                            if !route_signalr_message(
                                msg,
                                &event_tx,
                                &completion_tx,
                                &break_event_tx,
                            )
                            .await
                            {
                                return;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to parse SignalR message: {e}: {part}");
                        }
                    }
                }
            }
        });

        Ok(Self {
            ws_tx,
            event_rx: Mutex::new(event_rx),
            completion_rx: Mutex::new(completion_rx),
            next_id: AtomicI64::new(1),
            connection_id,
            is_attached: Mutex::new(false),
            is_stopped: Mutex::new(false),
            expecting_step: Mutex::new(false),
            break_event_rx: Mutex::new(break_event_rx),
        })
    }

    /// Invoke a SignalR method and wait for completion.
    ///
    /// Acquires `completion_rx` before sending. Concurrent calls therefore
    /// serialize on the wire as required by BC, rather than both sending and
    /// racing to consume one another's replies.
    async fn invoke(
        &self,
        target: &str,
        arguments: Vec<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>> {
        self.invoke_with_timeout(target, arguments, default_invoke_timeout(target))
            .await
    }

    async fn invoke_with_timeout(
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
    async fn handle_server_callback(&self, msg: &SignalRMessage) {
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
    async fn convert_event(&self, msg: &SignalRMessage) -> Option<BcEvent> {
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

    /// Whether BC has bound this debugger connection to a concrete NST client
    /// session. For break-on-next attaches this becomes true asynchronously
    /// through `OnAttachedToConnection` after the debug browser is opened.
    pub async fn is_attached(&self) -> bool {
        *self.is_attached.lock().await
    }

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
#[derive(Clone)]
struct TestSignalRTx {
    event_tx: mpsc::Sender<SignalRMessage>,
    completion_tx: mpsc::Sender<SignalRMessage>,
}

#[cfg(test)]
impl TestSignalRTx {
    async fn send(&self, msg: SignalRMessage) -> std::result::Result<(), ()> {
        if msg.type_ == 3 {
            self.completion_tx.send(msg).await.map_err(|_| ())
        } else {
            self.event_tx.send(msg).await.map_err(|_| ())
        }
    }

    fn try_send(&self, msg: SignalRMessage) -> std::result::Result<(), ()> {
        if msg.type_ == 3 {
            self.completion_tx.try_send(msg).map_err(|_| ())
        } else {
            self.event_tx.try_send(msg).map_err(|_| ())
        }
    }
}

#[cfg(test)]
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
    fn test_new(
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

#[cfg(test)]
pub(crate) mod fake;

#[cfg(test)]
mod tests;
