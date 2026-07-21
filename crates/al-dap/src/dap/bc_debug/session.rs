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

const COMPLETION_CHANNEL_CAPACITY: usize = 32;

/// Capacity of the SignalR event channel that the reader task forwards
/// server-push messages into. A misbehaving (or malicious) BC server
/// flooding the daemon used to grow this channel unboundedly, since the
/// previous channel was `mpsc::unbounded_channel`. F-OPEN-017.
///
/// 4096 messages × ~few-KB-each = ~MB-scale bound. Variable-expansion
/// responses can be large (deep AL records); Break / step-complete events
/// are small. If the channel ever fills, the reader task drops the
/// offending push message with a `warn!` log — that's preferable to OOM.
/// Break events have a *separate* dedicated channel (`break_event_*`)
/// that stays unbounded because each entry is a single `bool` and a
/// dropped Break event leaves the debugger silently stuck.
const EVENT_CHANNEL_CAPACITY: usize = 4096;

#[derive(Debug)]
struct AcceptInvalidCertVerifier {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AcceptInvalidCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn websocket_connector(accept_invalid_certs: bool) -> Result<Option<tokio_tungstenite::Connector>> {
    if !accept_invalid_certs {
        return Ok(None);
    }

    let algorithms = rustls::crypto::ring::default_provider().signature_verification_algorithms;
    let provider = rustls::crypto::ring::default_provider();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| DapError::ConnectionFailed(format!("TLS configuration failed: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptInvalidCertVerifier { algorithms }))
        .with_no_client_auth();
    Ok(Some(tokio_tungstenite::Connector::Rustls(Arc::new(config))))
}

fn websocket_host_header(ws_url: &str) -> Result<String> {
    let parsed = url::Url::parse(ws_url)
        .map_err(|e| DapError::ConnectionFailed(format!("Invalid WebSocket URL: {e}")))?;
    let host = match parsed.host() {
        Some(url::Host::Domain(domain)) => domain.to_string(),
        Some(url::Host::Ipv4(address)) => address.to_string(),
        Some(url::Host::Ipv6(address)) => format!("[{address}]"),
        None => {
            return Err(DapError::ConnectionFailed(
                "WebSocket URL has no host".to_string(),
            ))
        }
    };
    Ok(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn validate_handshake_and_take_frames(text: &str) -> Result<Vec<String>> {
    let mut parts = text.split('\x1e');
    let handshake = parts.next().unwrap_or_default();
    validate_signalr_handshake_response(handshake)?;
    Ok(parts
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect())
}

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
        // Completion replies are correctness-critical. Back-pressure the WS
        // reader rather than dropping a reply and timing out its invocation.
        return completion_tx.send(msg).await.is_ok();
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
    is_stopped: Mutex<bool>,
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
            // bc_client.rs:99 (T035). Without this the DAP path silently
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
        // response, validating the version the server actually negotiated
        // (F-OPEN-137). We request `negotiateVersion=1`; a spec-compliant
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
        // adapter that will misbehave on every later invoke (F-OPEN-016).
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
        // hostile or misbehaving server can't OOM the daemon. F-OPEN-017.
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
            is_stopped: Mutex::new(false),
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
                }
                "OnDetachedFromConnection" => {
                    let terminate = msg
                        .arguments
                        .as_ref()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    info!("Detached from debug connection (terminate={terminate})");
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
            if let Some(bc_event) = signalr_to_bc_event(event) {
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
                if let Some(bc_event) = signalr_to_bc_event(&msg) {
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

    pub async fn attach(&self, config: &BcDebugConfig) -> Result<()> {
        let mut args = serde_json::json!({
            "breakOnError": config.break_on_error,
            "breakOnRecordWrite": config.break_on_record_write,
        });
        // Attach session selectors. `breakOnNext` breaks into the next client
        // session of the given kind; `sessionId` attaches to one already-running
        // session. Both are forwarded only when configured, so the default
        // attach payload (and its existing wire contract) is unchanged.
        if let Some(next) = config.break_on_next.as_deref() {
            args["breakOnNext"] = serde_json::json!(next);
        }
        if let Some(session_id) = config.session_id {
            args["sessionId"] = serde_json::json!(session_id);
        }
        self.invoke("Attach", vec![args]).await?;
        info!("Attached to BC debug session");
        Ok(())
    }

    /// BC hub method: `DebugAdapterConfigurationDone(debugOptions)`
    /// Newer BC versions (>1.0) require debug options argument.
    pub async fn configuration_done(&self, config: &BcDebugConfig) -> Result<()> {
        let debug_options = serde_json::json!({
            "BreakOnError": config.break_on_error,
            "BreakOnErrorBehaviour": if config.break_on_error { 1 } else { 0 }, // All=1, None=0
            "BreakOnRecordWrite": config.break_on_record_write,
            "BreakOnRecordWriteBehaviour": if config.break_on_record_write { 1 } else { 0 },
            "SkipSystemTriggers": true,
            "EnableSqlInformationDebugger": true,
            "EnableLongRunningSqlStatements": true,
            "LongRunningSqlStatementsThreshold": 500,
            "NumberOfSqlStatements": 10,
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
        // BC uses Newtonsoft.Json with [JsonProperty] PascalCase names.
        // ObjectTypeWrapper enum — try both integer and string forms since BC hub
        // configuration may vary. The enum names are: Table, Report, CodeUnit, XmlPort,
        // Page, Query, PageExtension, TableExtension, Enum, EnumExtension, ReportExtension
        // Newtonsoft.Json defaults to integer enum serialization
        let object_id = serde_json::json!({
            "ObjectType": object_type,
            "ObjectNumber": object_number,
        });
        let position = serde_json::json!({
            "Line": line,
            "Column": column,
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

    pub async fn stop_debugging(&self) -> Result<()> {
        if let Err(e) = self.invoke("StopDebugging", vec![]).await {
            tracing::warn!("StopDebugging failed (non-fatal): {e}");
        }
        Ok(())
    }

    /// BC hub method: `GetStackTrace()` → `StackFrame[]`
    ///
    /// Each StackFrame has (at minimum):
    ///   - `ApplicationObjectId` — BC object reference
    ///   - `SourcePosition` — `{Line, Column}`
    ///   - `DisplayName` — human-readable frame name
    ///
    /// TODO: Verify exact hub method name and signature from EditorServices.Protocol.dll.
    ///       Current best guess based on EditorServices protocol reverse-engineering.
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

    /// BC hub method: `GetWatchNode(int frameId, string expression)` → `LocalNode`
    pub async fn evaluate(&self, frame_id: i64, expression: &str) -> Result<serde_json::Value> {
        let result = self
            .invoke(
                "GetWatchNode",
                vec![serde_json::json!(frame_id), serde_json::json!(expression)],
            )
            .await?;
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
            is_stopped: Mutex::new(false),
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

#[cfg(test)]
mod tests {
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
    fn invocation(
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

    fn next_frame(rx: &mut mpsc::Receiver<String>) -> serde_json::Value {
        let raw = rx.try_recv().expect("session should have sent a frame");
        serde_json::from_str(&raw).expect("sent frame is valid JSON")
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
        assert_eq!(args[1]["Line"], 42);
        assert_eq!(args[1]["Column"], 8);
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
    async fn attach_serializes_break_flags() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        let cfg = BcDebugConfig {
            break_on_error: true,
            break_on_record_write: false,
            ..BcDebugConfig::default()
        };
        session.attach(&cfg).await.expect("attach ok");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["target"], "Attach");
        assert_eq!(frame["arguments"][0]["breakOnError"], true);
        assert_eq!(frame["arguments"][0]["breakOnRecordWrite"], false);
        // With neither selector set, the payload carries no session keys —
        // the default attach wire contract is unchanged.
        assert!(
            frame["arguments"][0].get("sessionId").is_none(),
            "sessionId must be absent when unset"
        );
        assert!(
            frame["arguments"][0].get("breakOnNext").is_none(),
            "breakOnNext must be absent when unset"
        );
    }

    #[tokio::test]
    async fn attach_forwards_session_id_and_break_on_next() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        let cfg = BcDebugConfig {
            break_on_next: Some("Agent".to_string()),
            session_id: Some(7),
            ..BcDebugConfig::default()
        };
        session.attach(&cfg).await.expect("attach ok");
        let frame = next_frame(&mut ws_rx);
        assert_eq!(frame["target"], "Attach");
        // Selectors are forwarded into the Attach payload when configured.
        assert_eq!(frame["arguments"][0]["breakOnNext"], "Agent");
        assert_eq!(frame["arguments"][0]["sessionId"], 7);
    }

    #[tokio::test]
    async fn configuration_done_sends_debug_options_on_first_attempt() {
        let (session, event_tx, _b, mut ws_rx) = BcDebugSession::test_new("c".into());
        event_tx
            .send(completion("1", Some(serde_json::json!(null)), None))
            .await
            .unwrap();
        let cfg = BcDebugConfig {
            break_on_error: true,
            break_on_record_write: false,
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
        assert_eq!(args[0]["BreakOnError"], true);
        assert_eq!(args[0]["BreakOnErrorBehaviour"], 1);
        assert_eq!(args[0]["BreakOnRecordWriteBehaviour"], 0);
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
            serde_json::json!("hit"),
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
}
