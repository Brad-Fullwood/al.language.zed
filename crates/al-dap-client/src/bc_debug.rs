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

use crate::{DapError, Result};

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
        if let Some(b) = args.get("launchBrowser").and_then(|v| v.as_bool()) {
            cfg.launch_browser = b;
        }
        if let Some(s) = args.get("schemaUpdateMode").and_then(|v| v.as_str()) {
            cfg.schema_update_mode = s.to_string();
        }
        if let Some(s) = args.get("dependencyPublishingOption").and_then(|v| v.as_str()) {
            cfg.dependency_publishing_option = s.to_string();
        }
        if let Some(b) = args.get("validateServerCertificate").and_then(|v| v.as_bool()) {
            cfg.accept_invalid_certs = !b;
        }
        cfg
    }

    /// Get the base URL for the BC dev API.
    pub fn base_url(&self) -> String {
        if self.environment_type.eq_ignore_ascii_case("OnPrem") {
            let server = self.server.as_deref().unwrap_or("http://localhost");
            let instance = self.server_instance.as_deref().unwrap_or("BC");
            format!("{server}/{instance}/dev")
        } else {
            // Cloud
            let env = self.environment_name.as_deref().unwrap_or("sandbox");
            format!("https://api.businesscentral.dynamics.com/v2.0/{env}/dev")
        }
    }

    /// Get the SignalR hub URL for debugging.
    pub fn debug_hub_url(&self) -> String {
        if self.environment_type.eq_ignore_ascii_case("OnPrem") {
            let server = self.server.as_deref().unwrap_or("http://localhost");
            let instance = self.server_instance.as_deref().unwrap_or("BC");
            format!("{server}/{instance}/dev/DebuggerHub")
        } else {
            let env = self.environment_name.as_deref().unwrap_or("sandbox");
            format!("https://api.businesscentral.dynamics.com/v2.0/{env}/dev/DebuggerHub")
        }
    }
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
        config.tenant, config.schema_update_mode, config.dependency_publishing_option
    );

    info!("Publishing package to {url}");

    let app_bytes = tokio::fs::read(app_path).await
        .map_err(|e| DapError::PublishFailed(format!("Failed to read .app file: {e}")))?;

    let file_name = app_path.file_name()
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
        let body = resp.text().await.unwrap_or_default();
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
    let url = format!("{base}/metadata?tenant={}", config.tenant);

    let resp = http
        .get(&url)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .send()
        .await
        .map_err(|e| DapError::ConnectionFailed(format!("Metadata request failed: {e}")))?;

    if resp.status().is_success() {
        resp.json().await.map_err(|e| DapError::ConnectionFailed(format!("Bad metadata response: {e}")))
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
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

/// Maximum number of server-push events buffered between `invoke()` calls.
const PENDING_EVENT_CAPACITY: usize = 64;

/// Native BC debug session over SignalR.
pub struct BcDebugSession {
    /// Send SignalR messages to the hub
    ws_tx: mpsc::Sender<String>,
    /// Receive events/completions from the hub
    event_rx: Mutex<mpsc::Receiver<SignalRMessage>>,
    /// Invocation ID counter
    next_id: AtomicI64,
    /// SignalR connection ID — used in browser URL for debug context
    pub connection_id: String,
    /// Whether we're currently stopped at a breakpoint
    is_stopped: Mutex<bool>,
    /// Server-push type-1 events that arrived while an `invoke()` was waiting
    /// for its own completion. Callers drain this buffer after each invoke.
    pending_events: Mutex<VecDeque<SignalRMessage>>,
}

impl BcDebugSession {
    /// Connect to the BC debug hub via SignalR WebSocket.
    pub async fn connect(
        config: &BcDebugConfig,
        access_token: &str,
    ) -> Result<Self> {
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
        let resp_text = negotiate_resp.text().await
            .map_err(|e| DapError::ConnectionFailed(format!("Failed to read negotiate response: {e}")))?;
        debug!("Negotiate response (HTTP {}): {}", status, &resp_text[..resp_text.len().min(500)]);

        if !status.is_success() {
            return Err(DapError::ConnectionFailed(format!(
                "SignalR negotiate failed (HTTP {status}): {resp_text}"
            )));
        }

        let negotiate: serde_json::Value = serde_json::from_str(&resp_text)
            .map_err(|e| DapError::ConnectionFailed(format!("Bad negotiate JSON: {e}: {}", &resp_text[..resp_text.len().min(200)])))?;

        let connection_token = negotiate.get("connectionToken")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DapError::ConnectionFailed("No connectionToken in negotiate".to_string()))?;
        let connection_id = negotiate.get("connectionId")
            .and_then(|v| v.as_str())
            .unwrap_or(connection_token)
            .to_string();

        // Connect WebSocket
        let ws_url = hub_url.replace("https://", "wss://").replace("http://", "ws://");
        let ws_url = format!("{ws_url}?id={connection_token}");
        info!("SignalR WebSocket: {ws_url}");

        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&ws_url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
            .header("Host", url::Url::parse(&ws_url).map(|u| u.host_str().unwrap_or("").to_string()).unwrap_or_default())
            .body(())
            .map_err(|e| DapError::ConnectionFailed(format!("WS request build error: {e}")))?;

        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await
            .map_err(|e| DapError::ConnectionFailed(format!("WebSocket connect failed: {e}")))?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Send SignalR handshake (JSON protocol)
        let handshake = "{\"protocol\":\"json\",\"version\":1}\x1e";
        ws_sink.send(tokio_tungstenite::tungstenite::Message::Text(handshake.into()))
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
        let (event_tx, event_rx) = mpsc::channel::<SignalRMessage>(64);

        // Writer task: send messages from channel to WebSocket
        tokio::spawn(async move {
            while let Some(msg) = ws_rx.recv().await {
                let framed = format!("{msg}\x1e"); // SignalR record separator
                if ws_sink.send(tokio_tungstenite::tungstenite::Message::Text(framed.into())).await.is_err() {
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
                            debug!("SignalR recv: type={} target={:?} id={:?}",
                                msg.type_, msg.target, msg.invocation_id);
                            if event_tx.send(msg).await.is_err() {
                                break;
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
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();

        let msg = serde_json::json!({
            "type": 1,
            "target": target,
            "arguments": arguments,
            "invocationId": id,
        });

        info!("SignalR invoke: {} args={}", target, serde_json::to_string(&arguments).unwrap_or_default());
        self.ws_tx.send(msg.to_string()).await
            .map_err(|_| DapError::ConnectionFailed("WebSocket channel closed".to_string()))?;

        // Wait for completion with matching invocation ID
        let mut rx = self.event_rx.lock().await;
        let timeout = tokio::time::Duration::from_secs(60);
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
                    // `drain_pending_events()` after the invoke returns.
                    if msg.type_ == 1 {
                        let mut buf = self.pending_events.lock().await;
                        if buf.len() < PENDING_EVENT_CAPACITY {
                            buf.push_back(msg);
                        } else {
                            warn!("pending_events buffer full — dropping server callback");
                        }
                    }
                }
                Ok(None) => return Err(DapError::ConnectionFailed("SignalR channel closed".to_string())),
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
                    let terminate = msg.arguments.as_ref()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    info!("Detached from debug connection (terminate={terminate})");
                }
                "OnFatalDebuggerException" => {
                    let message = msg.arguments.as_ref()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
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
    pub async fn flush_pending_events(&self) {
        let events: Vec<SignalRMessage> = {
            let mut buf = self.pending_events.lock().await;
            buf.drain(..).collect()
        };
        for event in events {
            self.handle_server_callback(&event).await;
        }
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
        match self.invoke("DebugAdapterConfigurationDone", vec![debug_options]).await {
            Ok(_) => Ok(()),
            Err(_) => {
                // Older BC: no args
                let _ = self.invoke("DebugAdapterConfigurationDone", vec![]).await;
                Ok(())
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
        let result = self.invoke("AddBreakpoint", vec![object_id, position, serde_json::json!(condition)]).await?;
        Ok(result.unwrap_or(serde_json::Value::Null))
    }

    /// Remove a breakpoint.
    /// BC hub method: `RemoveBreakpoint(long breakpointId)`
    pub async fn remove_breakpoint(&self, breakpoint_id: i64) -> Result<()> {
        self.invoke("RemoveBreakpoint", vec![serde_json::json!(breakpoint_id)]).await?;
        Ok(())
    }

    /// Update a breakpoint condition.
    /// BC hub method: `UpdateBreakpoint(long id, string condition)`
    pub async fn update_breakpoint(&self, breakpoint_id: i64, condition: &str) -> Result<()> {
        self.invoke("UpdateBreakpoint", vec![serde_json::json!(breakpoint_id), serde_json::json!(condition)]).await?;
        Ok(())
    }

    /// Continue execution after a breakpoint.
    /// BC hub method: `SetBreakpointResponse(breakpointResponse)`
    /// Note: BC uses "SetBreakpointResponse" for continue, not a "continue" method.
    pub async fn continue_execution(&self, breakpoint_response: serde_json::Value) -> Result<()> {
        *self.is_stopped.lock().await = false;
        self.invoke("SetBreakpointResponse", vec![breakpoint_response]).await?;
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
        let _ = self.invoke("StopDebugging", vec![]).await;
        Ok(())
    }

    /// Get variables for a frame.
    /// BC hub method: `GetVariables(int frameId)` → `LocalNode[]`
    pub async fn get_variables(&self, frame_id: i64) -> Result<serde_json::Value> {
        let result = self.invoke("GetVariables", vec![serde_json::json!(frame_id)]).await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// Get globals for a frame.
    /// BC hub method: `ExpandGlobals(int frameId)` → `LocalNode[]`
    pub async fn get_globals(&self, frame_id: i64) -> Result<serde_json::Value> {
        let result = self.invoke("ExpandGlobals", vec![serde_json::json!(frame_id)]).await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// Expand a variable node.
    /// BC hub method: `ExpandNode(int frameId, string path)` → `LocalNode[]`
    pub async fn expand_node(&self, frame_id: i64, path: &str) -> Result<serde_json::Value> {
        let result = self.invoke("ExpandNode", vec![serde_json::json!(frame_id), serde_json::json!(path)]).await?;
        Ok(result.unwrap_or(serde_json::json!([])))
    }

    /// Evaluate an expression (watch).
    /// BC hub method: `GetWatchNode(int frameId, string expression)` → `LocalNode`
    pub async fn evaluate(&self, frame_id: i64, expression: &str) -> Result<serde_json::Value> {
        let result = self.invoke("GetWatchNode", vec![serde_json::json!(frame_id), serde_json::json!(expression)]).await?;
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
        Ok(result.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default())
    }

    /// Terminate the debug session.
    pub async fn terminate(&self) -> Result<()> {
        let _ = self.invoke("TerminateSession", vec![]).await;
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
