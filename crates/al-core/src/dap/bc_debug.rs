//! Native Business Central debug client.
//!
//! Communicates directly with BC via REST API (publish) and SignalR (debug session),
//! eliminating the dependency on Microsoft's EditorServices.Host binary.
//!
//! Protocol (reverse-engineered from EditorServices.Host):
//! 1. REST: POST /v2.0/{env}/dev/apps — publish .app package
//! 2. SignalR: Connect to debug hub for breakpoints, stepping, variables
//!
//! SignalR methods (from EditorServices.Protocol.dll):
//!   OpenConnectionAsync, Attach, ConfigurationDoneAsync, ContinueAsync,
//!   AddBreakpointAsync, RemoveBreakpointAsync, UpdateBreakpointAsync,
//!   GetVariablesAsync, ExpandGlobalsAsync, ExpandNodeAsync,
//!   GetWatchNodeAsync, GetSourceAsync, TerminateSession, IsAlive

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use futures_util::{SinkExt, StreamExt};
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use super::{DapError, Result};

/// Maximum number of pending debug events buffered before being consumed.
const PENDING_EVENT_CAPACITY: usize = 64;

/// Capacity of the SignalR event channel that the reader task forwards
/// every server-push message into. A misbehaving (or malicious) BC server
/// flooding the daemon used to grow this channel unboundedly, since the
/// previous channel was `mpsc::unbounded_channel`. F-OPEN-017.
///
/// 4096 messages × ~few-KB-each = ~MB-scale bound. Variable-expansion
/// responses can be large (deep AL records); Break / step-complete events
/// are small. If the channel ever fills, the reader task drops the
/// offending message with a `warn!` log — that's preferable to OOM.
/// Break events have a *separate* dedicated channel (`break_event_*`)
/// that stays unbounded because each entry is a single `bool` and a
/// dropped Break event leaves the debugger silently stuck.
const EVENT_CHANNEL_CAPACITY: usize = 4096;

/// Parse a DAP arg value that may be a `bool` or a `string` ("none"/"false" → false).
/// `default` is returned for non-bool, non-string variants.
fn parse_bool_or_string(v: &serde_json::Value, default: bool) -> bool {
    match v {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => {
            !s.eq_ignore_ascii_case("none") && !s.eq_ignore_ascii_case("false")
        }
        _ => default,
    }
}

// ---------------------------------------------------------------------------
// BC Server Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BcDebugConfig {
    pub server: Option<String>,
    pub server_instance: Option<String>,
    pub port: u16,
    pub tenant: String,
    pub environment_type: String,
    pub environment_name: Option<String>,
    pub authentication: String,
    pub break_on_error: bool,
    pub break_on_record_write: bool,
    pub break_on_next: Option<String>,
    pub startup_object_type: String,
    pub startup_object_id: i64,
    pub launch_browser: bool,
    pub schema_update_mode: String,
    pub dependency_publishing_option: String,
    pub accept_invalid_certs: bool,
}

impl Default for BcDebugConfig {
    fn default() -> Self {
        Self {
            server: None,
            server_instance: None,
            port: 7049,
            tenant: "default".to_string(),
            environment_type: "Sandbox".to_string(),
            environment_name: None,
            authentication: "UserPassword".to_string(),
            break_on_error: true,
            break_on_record_write: false,
            break_on_next: None,
            startup_object_type: "Page".to_string(),
            startup_object_id: 22,
            launch_browser: true,
            schema_update_mode: "Synchronize".to_string(),
            dependency_publishing_option: "Default".to_string(),
            accept_invalid_certs: false,
        }
    }
}

impl BcDebugConfig {
    /// Build from DAP launch/attach arguments.
    pub fn from_dap_args(args: &serde_json::Value) -> Self {
        let mut cfg = Self::default();
        if let Some(s) = args.get("server").and_then(|v| v.as_str()) {
            cfg.server = Some(s.to_string());
        }
        if let Some(s) = args.get("serverInstance").and_then(|v| v.as_str()) {
            cfg.server_instance = Some(s.to_string());
        }
        if let Some(n) = args.get("port").and_then(|v| v.as_u64()) {
            cfg.port = n as u16;
        }
        if let Some(s) = args.get("tenant").and_then(|v| v.as_str()) {
            cfg.tenant = s.to_string();
        }
        if let Some(s) = args.get("environmentType").and_then(|v| v.as_str()) {
            cfg.environment_type = s.to_string();
        }
        if let Some(s) = args.get("environmentName").and_then(|v| v.as_str()) {
            cfg.environment_name = Some(s.to_string());
        }
        if let Some(s) = args.get("authentication").and_then(|v| v.as_str()) {
            cfg.authentication = s.to_string();
        }
        // breakOnError / breakOnRecordWrite can be bool or string ("none"/"false" → false)
        if let Some(v) = args.get("breakOnError") {
            cfg.break_on_error = parse_bool_or_string(v, true);
        }
        if let Some(v) = args.get("breakOnRecordWrite") {
            cfg.break_on_record_write = parse_bool_or_string(v, false);
        }
        if let Some(s) = args.get("breakOnNext").and_then(|v| v.as_str()) {
            cfg.break_on_next = Some(s.to_string());
        }
        if let Some(s) = args.get("startupObjectType").and_then(|v| v.as_str()) {
            cfg.startup_object_type = s.to_string();
        }
        if let Some(n) = args.get("startupObjectId").and_then(|v| v.as_i64()) {
            cfg.startup_object_id = n;
        }
        if let Some(v) = args.get("launchBrowser") {
            cfg.launch_browser = parse_bool_or_string(v, cfg.launch_browser);
        }
        if let Some(s) = args.get("schemaUpdateMode").and_then(|v| v.as_str()) {
            cfg.schema_update_mode = s.to_string();
        }
        if let Some(s) = args
            .get("dependencyPublishingOption")
            .and_then(|v| v.as_str())
        {
            cfg.dependency_publishing_option = s.to_string();
        }
        if let Some(v) = args.get("validateServerCertificate") {
            cfg.accept_invalid_certs = !parse_bool_or_string(v, !cfg.accept_invalid_certs);
        }
        cfg
    }

    /// Build the base URL prefix for on-prem: `{server}:{port}/{instance}`.
    /// Uses the same pattern as `al-core::launch::BcServerConfig::dev_packages_url`.
    pub(crate) fn onprem_base(&self) -> String {
        let server = self.server.as_deref().unwrap_or("http://localhost");
        let instance = self.server_instance.as_deref().unwrap_or("BC");
        let host = format!("{}:{}", server.trim_end_matches('/'), self.port);
        format!("{host}/{instance}")
    }

    /// Get the base URL for the BC dev API.
    pub fn base_url(&self) -> String {
        if self.environment_type.eq_ignore_ascii_case("OnPrem") {
            // Fix #3: include port in on-prem URL
            format!("{}/dev", self.onprem_base())
        } else {
            // Fix #2: cloud URL must include tenant before environment name.
            // URL-encode tenant and environment name so values containing special
            // characters (spaces, dots, slashes) produce valid URLs.
            let tenant = percent_encode_url(&self.tenant);
            let env = percent_encode_url(self.environment_name.as_deref().unwrap_or("sandbox"));
            format!("https://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/dev")
        }
    }

    /// Get the SignalR hub URL for debugging.
    pub fn debug_hub_url(&self) -> String {
        if self.environment_type.eq_ignore_ascii_case("OnPrem") {
            // Fix #3: include port in on-prem URL
            format!("{}/dev/DebuggerHub", self.onprem_base())
        } else {
            // Fix #2: cloud URL must include tenant before environment name.
            // URL-encode tenant and environment name (same reason as base_url).
            let tenant = percent_encode_url(&self.tenant);
            let env = percent_encode_url(self.environment_name.as_deref().unwrap_or("sandbox"));
            format!("https://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/dev/DebuggerHub")
        }
    }
}

// ---------------------------------------------------------------------------
// BC Event types (public)
// ---------------------------------------------------------------------------

/// A BC server-push event, produced by `flush_pending_events` and `try_drain_push_events`.
///
/// These map to SignalR type-1 callback messages from the debug hub.
#[derive(Debug, Clone)]
pub enum BcEvent {
    /// Execution stopped (breakpoint hit, step complete, or exception).
    ///
    /// `thread_id` is always 1 for AL (single-threaded).
    /// `reason` is typically "breakpoint", "step", or "exception".
    Break { reason: String, thread_id: i64 },
    /// Debug session was detached.
    Detached { terminate: bool },
    /// Fatal debugger exception from the server.
    FatalError { message: String },
    /// Other unrecognised server callback — target name preserved for logging.
    Other { target: String },
}

// ---------------------------------------------------------------------------
// BC REST API Client
// ---------------------------------------------------------------------------

/// Publish an .app package to BC.
pub async fn publish_app(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
    app_path: &Path,
) -> Result<()> {
    let base = config.base_url();
    let url = format!(
        "{base}/apps?tenant={}&SchemaUpdateMode={}&DependencyPublishingOption={}",
        percent_encode_url(&config.tenant),
        config.schema_update_mode,
        config.dependency_publishing_option
    );

    info!("Publishing package to {url}");

    let app_bytes = tokio::fs::read(app_path)
        .await
        .map_err(|e| DapError::PublishFailed(format!("Failed to read .app file: {e}")))?;

    let file_name = app_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("app.app")
        .to_string();

    let part = reqwest::multipart::Part::bytes(app_bytes)
        .file_name(file_name)
        .mime_str("application/octet-stream")
        .map_err(|e| DapError::PublishFailed(format!("MIME error: {e}")))?;

    let form = reqwest::multipart::Form::new().part("file", part);

    let resp = http
        .post(&url)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| DapError::PublishFailed(format!("Publish request failed: {e}")))?;

    if resp.status().is_success() {
        info!("Package published successfully");
        Ok(())
    } else {
        let status = resp.status();
        let body = crate::bc_client::sanitize_error_body(
            &resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<body read failed: {e}>")),
        );
        Err(DapError::PublishFailed(format!(
            "Publish failed (HTTP {status}): {body}"
        )))
    }
}

/// Get server metadata.
pub async fn get_metadata(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
) -> Result<serde_json::Value> {
    let base = config.base_url();
    let url = format!(
        "{base}/metadata?tenant={}",
        percent_encode_url(&config.tenant)
    );

    let resp = http
        .get(&url)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .send()
        .await
        .map_err(|e| DapError::ConnectionFailed(format!("Metadata request failed: {e}")))?;

    if resp.status().is_success() {
        resp.json()
            .await
            .map_err(|e| DapError::ConnectionFailed(format!("Bad metadata response: {e}")))
    } else {
        let status = resp.status();
        let body = crate::bc_client::sanitize_error_body(
            &resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<body read failed: {e}>")),
        );
        Err(DapError::ConnectionFailed(format!(
            "Metadata failed (HTTP {status}): {body}"
        )))
    }
}

// ---------------------------------------------------------------------------
// SignalR Debug Hub Client
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SignalRMessage {
    #[serde(rename = "type")]
    type_: i32,
    // Type 1: invocation (server → client callback)
    target: Option<String>,
    arguments: Option<Vec<serde_json::Value>>,
    // Type 3: completion (response to our invocation)
    #[serde(rename = "invocationId")]
    invocation_id: Option<String>,
    result: Option<serde_json::Value>,
    error: Option<String>,
    // Type 6: ping
}

/// Native BC debug session over SignalR.
pub struct BcDebugSession {
    /// Send SignalR messages to the hub
    ws_tx: mpsc::Sender<String>,
    /// Receive events/completions from the hub (unbounded — never drops events)
    event_rx: Mutex<mpsc::Receiver<SignalRMessage>>,
    /// Invocation ID counter
    next_id: AtomicI64,
    /// SignalR connection ID — used in browser URL for debug context
    pub connection_id: String,
    /// Whether we're currently stopped at a breakpoint
    is_stopped: Mutex<bool>,
    /// Server-push type-1 events that arrived while an `invoke()` was waiting
    /// for its own completion. Callers drain this buffer after each invoke.
    /// Unbounded so that Break events are never silently dropped.
    pending_events: Mutex<VecDeque<SignalRMessage>>,
    /// Receives `true` when a Break event arrives and `false` when the session
    /// ends (Detached or FatalError). Populated by the WebSocket reader task,
    /// which sends without holding any lock — no deadlock risk. The channel is
    /// unbounded so events are never lost even if they arrive before the
    /// receiver calls `wait_for_break_event`.
    break_event_rx: Mutex<mpsc::UnboundedReceiver<bool>>,
}

/// Per-operation timeout for SignalR `invoke()` calls. Different debug-hub
/// targets have wildly different latency budgets:
///
/// - quick step/continue control flow → a few seconds is plenty
/// - stack/variable inspection → can be slow on deep AL records
/// - attach / disconnect / publish → cover network setup + server-side work
///
/// A blanket 60 s timeout (the prior default) was too short for slow-network
/// attach flows and too long for the user to notice that "Step Over" was
/// silently stuck. F-OPEN-015.
fn default_invoke_timeout(target: &str) -> tokio::time::Duration {
    use tokio::time::Duration;
    match target {
        // Step / continue / break — should respond within a couple of seconds
        // on a healthy server. Short timeout so a hung server fails fast.
        "Next" | "StepIn" | "StepOut" | "Continue" | "Break" => Duration::from_secs(10),
        // Connection ping. Should be very fast.
        "IsAlive" => Duration::from_secs(5),
        // Variable inspection / stack frames — can be slow on deep records
        // (BC's GetVariables walks the record graph server-side).
        "GetVariables" | "GetStackTrace" | "ExpandGlobals" | "ExpandVariableTree"
        | "ExpandLocalsTree" | "GetSource" => Duration::from_secs(30),
        // Attach / DebugAdapterConfigurationDone — network setup. Allow a
        // longer budget for high-latency BC SaaS connections.
        "Attach" | "DebugAdapterConfigurationDone" => Duration::from_secs(120),
        // Breakpoint operations — usually fast but can serialize behind a
        // BC compilation step.
        "AddBreakpoint" | "RemoveBreakpoint" | "SetBreakpointResponse" => Duration::from_secs(30),
        // Teardown — should be quick; if it isn't, we abandon and tear down
        // the WS connection anyway.
        "StopDebugging" | "TerminateSession" => Duration::from_secs(10),
        // Unknown / future targets — fall back to the previous global value.
        _ => Duration::from_secs(60),
    }
}

impl BcDebugSession {
    /// Connect to the BC debug hub via SignalR WebSocket.
    pub async fn connect(config: &BcDebugConfig, access_token: &str) -> Result<Self> {
        if config.accept_invalid_certs {
            // Match the warn-on-construction parity from BcClient::new at
            // bc_client.rs:99 (T035). Without this the DAP path silently
            // disables TLS certificate validation when launch.json sets
            // accept_invalid_certs=true.
            warn!(
                "BcDebugSession::connect: accept_invalid_certs=true is active — TLS \
                 certificate validation is DISABLED for the SignalR negotiate + \
                 WebSocket. Use only for local-dev sandboxes."
            );
        }
        let hub_url = config.debug_hub_url();

        // SignalR negotiate to get connection token
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

        let connection_token = negotiate
            .get("connectionToken")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                DapError::ConnectionFailed("No connectionToken in negotiate".to_string())
            })?;
        // The SignalR session id is reported as `connectionId`, but BC versions
        // (and the underlying SignalR implementation) have varied the casing, so
        // accept the common variants. Falling back to the connection *token* is
        // semantically wrong — the token is a WebSocket auth credential, not a
        // session id — so log a warning when no recognised id field is present
        // rather than silently substituting it (F-OPEN-137).
        let connection_id = ["connectionId", "ConnectionId", "connection_id"]
            .iter()
            .find_map(|key| negotiate.get(*key).and_then(|v| v.as_str()))
            .unwrap_or_else(|| {
                tracing::warn!(
                    "SignalR negotiate response has no connectionId field (checked \
                     connectionId/ConnectionId/connection_id); falling back to connection token \
                     as session id — debug context may be incorrect"
                );
                connection_token
            })
            .to_string();

        // Connect WebSocket
        let ws_url = hub_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        let ws_url = format!("{ws_url}?id={}", percent_encode_url(connection_token));
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
            .header(
                "Host",
                url::Url::parse(&ws_url)
                    .map(|u| u.host_str().unwrap_or("").to_string())
                    .unwrap_or_default(),
            )
            .body(())
            .map_err(|e| DapError::ConnectionFailed(format!("WS request build error: {e}")))?;

        let (ws_stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| DapError::ConnectionFailed(format!("WebSocket connect failed: {e}")))?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Send SignalR handshake (JSON protocol)
        let handshake = "{\"protocol\":\"json\",\"version\":1}\x1e";
        ws_sink
            .send(tokio_tungstenite::tungstenite::Message::Text(
                handshake.into(),
            ))
            .await
            .map_err(|e| DapError::ConnectionFailed(format!("SignalR handshake failed: {e}")))?;

        // Read handshake response
        if let Some(msg) = ws_source.next().await {
            let msg = msg.map_err(|e| DapError::ConnectionFailed(format!("WS read error: {e}")))?;
            debug!("SignalR handshake response: {:?}", msg);
        }

        info!("SignalR connection established");

        // Set up message channels
        let (ws_tx, mut ws_rx) = mpsc::channel::<String>(32);
        // Deep-but-bounded event channel. Break events take a dedicated
        // unbounded path below to preserve the "Break must never be lost"
        // invariant; everything else drops with a warn on overflow so a
        // hostile or misbehaving server can't OOM the daemon. F-OPEN-017.
        let (event_tx, event_rx) = mpsc::channel::<SignalRMessage>(EVENT_CHANNEL_CAPACITY);
        // Dedicated channel for Break/Detached/FatalError notifications.
        // Using an unbounded channel ensures events buffered before wait_for_break_event
        // is called are never lost. Each entry is one `bool`, so even a
        // pathological 1M-deep backlog is ~1 MB — bounded in practice by
        // the number of break events the BC server emits in one session.
        let (break_event_tx, break_event_rx) = mpsc::unbounded_channel::<bool>();

        // Writer task: send messages from channel to WebSocket
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

        // Reader task: receive messages from WebSocket and dispatch
        tokio::spawn(async move {
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

                // SignalR messages are separated by \x1e
                for part in text.split('\x1e') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<SignalRMessage>(part) {
                        Ok(msg) => {
                            if msg.type_ == 6 {
                                // Ping — ignore
                                continue;
                            }
                            debug!(
                                "SignalR recv: type={} target={:?} id={:?}",
                                msg.type_, msg.target, msg.invocation_id
                            );
                            // Notify wait_for_break_event before forwarding the full
                            // message so it can unblock immediately on Break/end events.
                            // The break_event_tx.send(...) result is intentionally
                            // logged-on-drop rather than collapsed into a match guard:
                            // putting a side-effecting send() in a pattern guard would
                            // be unusual and harder to reason about than the explicit
                            // if-let-err shape here.
                            #[allow(clippy::collapsible_match)]
                            if msg.type_ == 1 {
                                match msg.target.as_deref() {
                                    Some("Break") => {
                                        if break_event_tx.send(true).is_err() {
                                            tracing::debug!(
                                                target = "Break",
                                                "break_event_tx receiver dropped — \
                                                 wait_for_break_event listener has gone"
                                            );
                                        }
                                    }
                                    Some(
                                        "OnDetachedFromConnection" | "OnFatalDebuggerException",
                                    ) => {
                                        if break_event_tx.send(false).is_err() {
                                            tracing::debug!(
                                                target = "Detached/Fatal",
                                                "break_event_tx receiver dropped — \
                                                 session-end notification not delivered"
                                            );
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            // try_send so a full channel drops the message
                            // with a warn instead of awaiting (which would
                            // hold up the WS reader task and back-pressure
                            // the BC server). The dedicated break-event
                            // channel above carries the don't-lose-this
                            // signal separately. F-OPEN-017.
                            if let Err(e) = event_tx.try_send(msg) {
                                match e {
                                    tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                        warn!(
                                            cap = EVENT_CHANNEL_CAPACITY,
                                            "SignalR event channel full — dropping non-Break message; \
                                             debug consumer is not draining fast enough"
                                        );
                                    }
                                    tokio::sync::mpsc::error::TrySendError::Closed(_) => break,
                                }
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
            next_id: AtomicI64::new(1),
            connection_id,
            is_stopped: Mutex::new(false),
            pending_events: Mutex::new(VecDeque::with_capacity(PENDING_EVENT_CAPACITY)),
            break_event_rx: Mutex::new(break_event_rx),
        })
    }

    /// Invoke a SignalR method and wait for completion.
    ///
    /// Acquires `event_rx` for the duration of the call. Concurrent calls will
    /// queue on the mutex — this is intentional: BC requires request/response
    /// serialisation. Do NOT call this from within `handle_server_callback`
    /// (which is invoked while `event_rx` is held) — use `ws_tx.try_send`
    /// directly instead to avoid deadlock.
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
        self.ws_tx
            .send(msg.to_string())
            .await
            .map_err(|_| DapError::ConnectionFailed("WebSocket channel closed".to_string()))?;

        // Wait for completion with matching invocation ID
        let mut rx = self.event_rx.lock().await;
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(msg)) => {
                    // Check if it's a completion for our invocation
                    if msg.type_ == 3 && msg.invocation_id.as_deref() == Some(&id) {
                        if let Some(ref err) = msg.error {
                            error!("SignalR error for {target}: {err}");
                            return Err(DapError::ServerError(err.clone()));
                        }
                        debug!("SignalR result for {target}: {:?}", msg.result);
                        return Ok(msg.result);
                    }
                    // Server-push event received while waiting for our completion.
                    // Buffer it so it isn't dropped; the caller drains via
                    // `flush_pending_events()` after the invoke returns.
                    // No capacity limit — losing a Break event would leave the debugger silent.
                    if msg.type_ == 1 {
                        let mut buf = self.pending_events.lock().await;
                        buf.push_back(msg);
                    }
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

    /// Handle callbacks from the server.
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
                    // Execution stopped (breakpoint hit, step complete, exception)
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
                    // Ping — respond with try_send to avoid blocking while event_rx is held.
                    // If the send channel is full, the ping is silently dropped; BC will
                    // retry. Using .await here would deadlock when invoke() holds event_rx.
                    debug!("IsAlive ping from server");
                    let ack = serde_json::json!({
                        "type": 1,
                        "target": "AcknowledgeIsAlive",
                        "arguments": [],
                    });
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

    // ----- Pending event buffer -----

    /// Process server-push events that arrived during the last `invoke()` call.
    ///
    /// Call this after every `invoke()` to handle buffered callbacks (e.g.
    /// `Break`, `OnDetachedFromConnection`) that arrived while the invoke loop
    /// was consuming the event channel.
    ///
    /// Returns processed `BcEvent` values so callers can convert them to DAP events.
    pub async fn flush_pending_events(&self) -> Vec<BcEvent> {
        let raw: Vec<SignalRMessage> = {
            let mut buf = self.pending_events.lock().await;
            buf.drain(..).collect()
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
    /// Returns processed `BcEvent` values. May return an empty Vec if `invoke()` is
    /// currently holding the `event_rx` lock or if no events are available.
    pub async fn try_drain_push_events(&self) -> Vec<BcEvent> {
        let mut out = Vec::new();
        // Use try_lock so this never blocks waiting for invoke() to release event_rx.
        let mut rx = match self.event_rx.try_lock() {
            Ok(r) => r,
            Err(_) => return out, // invoke() is running — events will be buffered in pending_events
        };
        // Drain all currently available messages (non-blocking)
        while let Ok(msg) = rx.try_recv() {
            if msg.type_ == 1 {
                self.handle_server_callback(&msg).await;
                if let Some(bc_event) = signalr_to_bc_event(&msg) {
                    out.push(bc_event);
                }
            }
            // type 3 completions without a pending invoke are unexpected — ignore
        }
        out
    }

    // ----- Break-event wait -----

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

    // ----- Public debug operations -----

    /// Attach to the BC debug session.
    pub async fn attach(&self, config: &BcDebugConfig) -> Result<()> {
        let args = serde_json::json!({
            "breakOnError": config.break_on_error,
            "breakOnRecordWrite": config.break_on_record_write,
        });
        self.invoke("Attach", vec![args]).await?;
        info!("Attached to BC debug session");
        Ok(())
    }

    /// Signal configuration done.
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

    /// Add a breakpoint.
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

    /// Remove a breakpoint.
    /// BC hub method: `RemoveBreakpoint(long breakpointId)`
    pub async fn remove_breakpoint(&self, breakpoint_id: i64) -> Result<()> {
        self.invoke("RemoveBreakpoint", vec![serde_json::json!(breakpoint_id)])
            .await?;
        Ok(())
    }

    /// Update a breakpoint condition.
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

    /// Continue execution after a breakpoint.
    /// BC hub method: `SetBreakpointResponse(breakpointResponse)`
    /// Note: BC uses "SetBreakpointResponse" for continue, not a "continue" method.
    pub async fn continue_execution(&self, breakpoint_response: serde_json::Value) -> Result<()> {
        *self.is_stopped.lock().await = false;
        self.invoke("SetBreakpointResponse", vec![breakpoint_response])
            .await?;
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

    /// Stop debugging.
    /// BC hub method: `StopDebugging`
    pub async fn stop_debugging(&self) -> Result<()> {
        if let Err(e) = self.invoke("StopDebugging", vec![]).await {
            tracing::warn!("StopDebugging failed (non-fatal): {e}");
        }
        Ok(())
    }

    /// Get the current call stack.
    ///
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

    /// Get variables for a frame.
    /// BC hub method: `GetVariables(int frameId)` → `LocalNode[]`
    pub async fn get_variables(&self, frame_id: i64) -> Result<serde_json::Value> {
        let result = self
            .invoke("GetVariables", vec![serde_json::json!(frame_id)])
            .await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// Get globals for a frame.
    /// BC hub method: `ExpandGlobals(int frameId)` → `LocalNode[]`
    pub async fn get_globals(&self, frame_id: i64) -> Result<serde_json::Value> {
        let result = self
            .invoke("ExpandGlobals", vec![serde_json::json!(frame_id)])
            .await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// Expand a variable node.
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

    /// Evaluate an expression (watch).
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

    /// Get source for an object.
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

    /// Terminate the debug session.
    pub async fn terminate(&self) -> Result<()> {
        if let Err(e) = self.invoke("TerminateSession", vec![]).await {
            tracing::debug!(error = %e, "TerminateSession RPC errored — session may already be closed");
        }
        Ok(())
    }

    /// Check if the debug hub is alive.
    ///
    /// Calls `invoke()`, which acquires `event_rx`. Only safe to call when no
    /// other `invoke()` is in progress (i.e. outside of an active debug loop).
    /// Server-side `IsAlive` pings during a session are handled automatically
    /// via `try_send` in `handle_server_callback`.
    pub async fn is_alive(&self) -> bool {
        self.invoke("IsAlive", vec![]).await.is_ok()
    }

    /// Check if currently stopped at a breakpoint.
    pub async fn is_stopped(&self) -> bool {
        *self.is_stopped.lock().await
    }
}

/// Convert a raw `SignalRMessage` (type-1 server callback) to a `BcEvent`.
/// Extract an informative message from an `OnFatalDebuggerException` callback.
///
/// BC sends the fatal message as the first element of the `arguments` array.
/// When that is unavailable, collapsing every failure into the opaque string
/// `"unknown"` makes production troubleshooting of BC fatal errors very hard
/// (F-OPEN-138). Instead, distinguish the three distinct failure shapes so the
/// log line tells the operator *why* no message was extracted:
///
/// - `arguments` field absent entirely,
/// - `arguments` present but an empty array,
/// - first element present but not a JSON string (report its type).
fn fatal_exception_message(arguments: &Option<Vec<serde_json::Value>>) -> String {
    match arguments {
        None => "<no message provided: arguments field absent — check BC server logs>".to_string(),
        Some(args) => match args.first() {
            None => {
                "<no message provided: empty arguments array — check BC server logs>".to_string()
            }
            Some(v) => match v.as_str() {
                Some(s) => s.to_string(),
                None => {
                    let kind = match v {
                        serde_json::Value::Null => "null",
                        serde_json::Value::Bool(_) => "bool",
                        serde_json::Value::Number(_) => "number",
                        serde_json::Value::Array(_) => "array",
                        serde_json::Value::Object(_) => "object",
                        serde_json::Value::String(_) => "string",
                    };
                    format!("<non-string fatal message (received JSON {kind}); raw={v}>")
                }
            },
        },
    }
}

/// Returns `None` for messages that don't need to be forwarded to the DAP layer.
fn signalr_to_bc_event(msg: &SignalRMessage) -> Option<BcEvent> {
    let target = msg.target.as_deref()?;
    match target {
        "Break" => {
            // BC Break event indicates execution stopped.
            // The message from BC doesn't always specify a reason; we infer from context.
            // For simplicity, we report "breakpoint" as the reason. A more complete
            // implementation could inspect the break flags to distinguish step/exception.
            Some(BcEvent::Break {
                reason: "breakpoint".to_string(),
                thread_id: 1,
            })
        }
        "OnDetachedFromConnection" => {
            let terminate = msg
                .arguments
                .as_ref()
                .and_then(|a| a.first())
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(BcEvent::Detached { terminate })
        }
        "OnFatalDebuggerException" => {
            let message = fatal_exception_message(&msg.arguments);
            Some(BcEvent::FatalError { message })
        }
        "IsAlive" | "OnAttachedToConnection" => None, // handled internally
        other => Some(BcEvent::Other {
            target: other.to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/// Percent-encode a string for safe embedding as a URL query parameter value.
///
/// Unreserved characters (RFC 3986) are passed through unchanged; all other
/// bytes are encoded as `%XX`. This is used to sanitize server-returned values
/// (e.g. SignalR `connectionToken`) before they are embedded in WebSocket URLs.
fn percent_encode_url(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
        }
    }
    out
}

/// Replace the value of `connectionToken` (SignalR session credential) in a
/// JSON response body with a `<redacted>` placeholder before logging or
/// surfacing in errors. Falls back to the original text if the body is not
/// valid JSON or has no such field.
fn redact_connection_token(body: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    if let Some(map) = v.as_object_mut() {
        for key in ["connectionToken", "ConnectionToken", "accessToken"] {
            if map.contains_key(key) {
                map.insert(
                    key.to_string(),
                    serde_json::Value::String("<redacted>".to_string()),
                );
            }
        }
    }
    v.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- default_invoke_timeout (F-OPEN-015) ---------------------------------

    #[test]
    fn invoke_timeout_step_ops_are_short() {
        // Positive: step / continue / break should respond within seconds;
        // they get a short timeout so a hung server fails fast and the user
        // notices instead of waiting a full minute.
        for target in ["Next", "StepIn", "StepOut", "Continue", "Break"] {
            let t = default_invoke_timeout(target);
            assert!(
                t <= tokio::time::Duration::from_secs(15),
                "{target} timeout {t:?} should be ≤ 15s"
            );
        }
    }

    #[test]
    fn invoke_timeout_variable_inspection_is_medium() {
        // Positive: variable / stack-frame inspection can be slow on deep
        // BC records but shouldn't take more than ~30s either.
        for target in ["GetVariables", "GetStackTrace", "ExpandGlobals"] {
            let t = default_invoke_timeout(target);
            assert!(
                t >= tokio::time::Duration::from_secs(15)
                    && t <= tokio::time::Duration::from_secs(60),
                "{target} timeout {t:?} should be in [15s, 60s]"
            );
        }
    }

    #[test]
    fn invoke_timeout_attach_is_generous() {
        // Positive: attach / DebugAdapterConfigurationDone include network
        // setup against potentially-slow BC SaaS endpoints; need budget.
        for target in ["Attach", "DebugAdapterConfigurationDone"] {
            let t = default_invoke_timeout(target);
            assert!(
                t >= tokio::time::Duration::from_secs(60),
                "{target} timeout {t:?} should be ≥ 60s for SaaS latency headroom"
            );
        }
    }

    #[test]
    fn invoke_timeout_unknown_target_falls_back_to_60s() {
        // Negative: an unrecognised target (future BC protocol additions, or
        // a typo in our code) must still produce a finite, reasonable
        // default rather than panic or return zero.
        let t = default_invoke_timeout("SomeFutureUnknownTarget");
        assert_eq!(t, tokio::time::Duration::from_secs(60));
    }

    #[test]
    fn redact_connection_token_replaces_field() {
        let body = r#"{"connectionToken":"secret-abc","url":"/signalr"}"#;
        let redacted = redact_connection_token(body);
        assert!(!redacted.contains("secret-abc"));
        assert!(redacted.contains("<redacted>"));
        assert!(redacted.contains("/signalr"));
    }

    #[test]
    fn redact_connection_token_passthrough_on_invalid_json() {
        let body = "not-json garbage";
        assert_eq!(redact_connection_token(body), "not-json garbage");
    }

    #[test]
    fn redact_connection_token_no_field_unchanged_logically() {
        let body = r#"{"foo":"bar"}"#;
        let out = redact_connection_token(body);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["foo"], "bar");
        assert!(parsed.get("connectionToken").is_none());
    }

    fn cloud_config(tenant: &str, env_name: &str) -> BcDebugConfig {
        BcDebugConfig {
            environment_type: "Sandbox".to_string(),
            tenant: tenant.to_string(),
            environment_name: Some(env_name.to_string()),
            ..BcDebugConfig::default()
        }
    }

    fn onprem_config(server: &str, instance: &str, port: u16) -> BcDebugConfig {
        BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some(server.to_string()),
            server_instance: Some(instance.to_string()),
            port,
            ..BcDebugConfig::default()
        }
    }

    #[test]
    fn cloud_base_url_includes_tenant() {
        // Fix #2: cloud URL must include tenant before environment name
        let cfg = cloud_config("mytenant.onmicrosoft.com", "MySandbox");
        let url = cfg.base_url();
        assert!(
            url.contains("mytenant.onmicrosoft.com"),
            "cloud base_url should contain tenant: {url}"
        );
        assert!(
            url.contains("MySandbox"),
            "cloud base_url should contain env name: {url}"
        );
        assert_eq!(
            url,
            "https://api.businesscentral.dynamics.com/v2.0/mytenant.onmicrosoft.com/MySandbox/dev"
        );
    }

    #[test]
    fn cloud_hub_url_includes_tenant() {
        let cfg = cloud_config("contoso.com", "Production");
        let url = cfg.debug_hub_url();
        assert_eq!(
            url,
            "https://api.businesscentral.dynamics.com/v2.0/contoso.com/Production/dev/DebuggerHub"
        );
    }

    #[test]
    fn onprem_base_url_includes_port() {
        // Fix #3: on-prem URL must include port
        let cfg = onprem_config("http://erp.example.com", "BC240", 7050);
        let url = cfg.base_url();
        assert!(
            url.contains(":7050"),
            "on-prem base_url should include custom port: {url}"
        );
        assert_eq!(url, "http://erp.example.com:7050/BC240/dev");
    }

    #[test]
    fn onprem_hub_url_includes_port() {
        let cfg = onprem_config("https://bc.corp.local", "PROD", 9090);
        let url = cfg.debug_hub_url();
        assert_eq!(url, "https://bc.corp.local:9090/PROD/dev/DebuggerHub");
    }

    #[test]
    fn onprem_default_port_still_included() {
        // Default port (7049) should still appear in the URL — always include port
        let cfg = onprem_config("http://localhost", "BC", 7049);
        let url = cfg.base_url();
        assert!(
            url.contains(":7049"),
            "default port should be in URL: {url}"
        );
    }

    #[test]
    fn percent_encode_url_safe_chars_unchanged() {
        assert_eq!(percent_encode_url("abc-123_XYZ.~"), "abc-123_XYZ.~");
    }

    #[test]
    fn percent_encode_url_encodes_special_chars() {
        let token = "token=value&other=x";
        let encoded = percent_encode_url(token);
        assert!(!encoded.contains('='), "= should be encoded: {encoded}");
        assert!(!encoded.contains('&'), "& should be encoded: {encoded}");
        assert!(encoded.contains("%3D"), "= → %3D: {encoded}");
        assert!(encoded.contains("%26"), "& → %26: {encoded}");
    }

    #[test]
    fn percent_encode_url_empty_string() {
        assert_eq!(percent_encode_url(""), "");
    }

    // --- fatal_exception_message (F-OPEN-138) --------------------------------

    #[test]
    fn fatal_message_returns_actual_string() {
        let args = Some(vec![serde_json::json!("disk full")]);
        assert_eq!(fatal_exception_message(&args), "disk full");
    }

    #[test]
    fn fatal_message_distinguishes_absent_arguments() {
        let msg = fatal_exception_message(&None);
        assert!(
            msg.contains("arguments field absent"),
            "absent arguments must be distinguished: {msg}"
        );
        assert_ne!(msg, "unknown");
    }

    #[test]
    fn fatal_message_distinguishes_empty_array() {
        let msg = fatal_exception_message(&Some(vec![]));
        assert!(
            msg.contains("empty arguments array"),
            "empty array must be distinguished: {msg}"
        );
        assert_ne!(msg, "unknown");
    }

    #[test]
    fn fatal_message_reports_non_string_type_and_raw_value() {
        let args = Some(vec![serde_json::json!(42)]);
        let msg = fatal_exception_message(&args);
        assert!(msg.contains("number"), "must report JSON type: {msg}");
        assert!(msg.contains("42"), "must include raw value: {msg}");
        assert_ne!(msg, "unknown");
    }

    #[test]
    fn fatal_message_via_signalr_to_bc_event_is_informative() {
        // Absent arguments on the actual conversion path must surface an
        // informative FatalError, not the opaque "unknown" of old.
        let msg = SignalRMessage {
            type_: 1,
            target: Some("OnFatalDebuggerException".to_string()),
            arguments: None,
            invocation_id: None,
            result: None,
            error: None,
        };
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::FatalError { message }) => {
                assert!(message.contains("arguments field absent"), "{message}");
            }
            other => panic!("expected FatalError, got {other:?}"),
        }
    }

    // --- signalr_to_bc_event conversion shapes (F-OPEN-136) ------------------

    /// Build a type-1 (invocation) SignalR message with the given target and
    /// arguments, leaving the completion-only fields empty. Mirrors the shape
    /// produced by the WebSocket reader for server → client callbacks.
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

    #[test]
    fn signalr_to_bc_event_break_maps_to_breakpoint_on_thread_1() {
        // A "Break" callback always yields a Break event with reason
        // "breakpoint" on AL's single thread (id 1), regardless of arguments.
        let msg = invocation(Some("Break"), None);
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Break { reason, thread_id }) => {
                assert_eq!(reason, "breakpoint");
                assert_eq!(thread_id, 1);
            }
            other => panic!("expected Break, got {other:?}"),
        }
    }

    #[test]
    fn signalr_to_bc_event_detached_reads_terminate_true() {
        // First argument `true` means the session should terminate.
        let msg = invocation(
            Some("OnDetachedFromConnection"),
            Some(vec![serde_json::json!(true)]),
        );
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Detached { terminate }) => assert!(terminate),
            other => panic!("expected Detached, got {other:?}"),
        }
    }

    #[test]
    fn signalr_to_bc_event_detached_reads_terminate_false() {
        // First argument `false` means detach without terminating.
        let msg = invocation(
            Some("OnDetachedFromConnection"),
            Some(vec![serde_json::json!(false)]),
        );
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Detached { terminate }) => assert!(!terminate),
            other => panic!("expected Detached, got {other:?}"),
        }
    }

    #[test]
    fn signalr_to_bc_event_detached_defaults_terminate_false_when_missing() {
        // Absent / non-boolean arguments must default to `terminate = false`
        // rather than panicking or terminating the session unexpectedly.
        for args in [
            None,
            Some(vec![]),
            Some(vec![serde_json::json!("not a bool")]),
        ] {
            let msg = invocation(Some("OnDetachedFromConnection"), args.clone());
            match signalr_to_bc_event(&msg) {
                Some(BcEvent::Detached { terminate }) => {
                    assert!(!terminate, "args {args:?} should default terminate=false")
                }
                other => panic!("expected Detached for args {args:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn signalr_to_bc_event_internal_targets_are_dropped() {
        // IsAlive / OnAttachedToConnection are handled internally and must not
        // be forwarded to the DAP layer.
        for target in ["IsAlive", "OnAttachedToConnection"] {
            let msg = invocation(Some(target), None);
            assert!(
                signalr_to_bc_event(&msg).is_none(),
                "{target} must not produce a BcEvent"
            );
        }
    }

    #[test]
    fn signalr_to_bc_event_unknown_target_preserved_as_other() {
        // An unrecognised callback is preserved verbatim as Other so it can be
        // logged without losing the target name.
        let msg = invocation(Some("SomeFutureCallback"), None);
        match signalr_to_bc_event(&msg) {
            Some(BcEvent::Other { target }) => assert_eq!(target, "SomeFutureCallback"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn signalr_to_bc_event_missing_target_is_dropped() {
        // Type-3/6 frames carry no target; they must not be forwarded.
        let msg = invocation(None, None);
        assert!(signalr_to_bc_event(&msg).is_none());
    }
}
