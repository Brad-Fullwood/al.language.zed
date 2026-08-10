//! Native DAP server for AL debugging.
//!
//! Speaks DAP over stdio to Zed, and translates to BC REST + SignalR internally.
//! Replaces the EditorServices.Host proxy entirely.
//!
//! Flow:
//! 1. Zed sends DAP initialize → we respond with capabilities
//! 2. Zed sends launch/attach → we compile, publish, connect SignalR
//! 3. Zed sends setBreakpoints → we call AddBreakpointAsync on SignalR
//! 4. BC sends events (stopped, etc.) → we translate to DAP events
//! 5. Zed sends continue/step → we call ContinueAsync on SignalR
//! 6. Zed sends variables/evaluate → we call GetVariablesAsync/GetWatchNodeAsync

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{self, BufReader};
use tokio::sync::{watch, Mutex};
use tracing::{debug, error, info, warn};

use super::bc_debug::{
    get_web_endpoint, percent_encode_url, publish_app, BcDebugConfig, BcDebugSession, BcEvent,
};
use super::framing::{read_dap_body, write_dap_frame};
use super::{DapError, Result};

/// BC `ObjectTypeWrapper` enum values (integer encoding used by SignalR hub).
///
/// Source: EditorServices.Protocol.dll reverse-engineering.
/// Newtonsoft.Json defaults to integer enum serialization, so these are
/// transmitted as integers in SignalR JSON messages.
pub mod bc_object_type {
    pub const TABLE: i32 = 1;
    pub const REPORT: i32 = 3;
    pub const CODEUNIT: i32 = 5;
    pub const XMLPORT: i32 = 6;
    pub const PAGE: i32 = 8;
    pub const QUERY: i32 = 9;
    pub const PAGE_EXTENSION: i32 = 14;
    pub const TABLE_EXTENSION: i32 = 15;
    pub const ENUM: i32 = 16;
    pub const ENUM_EXTENSION: i32 = 17;
    pub const REPORT_EXTENSION: i32 = 22;
    pub const UNKNOWN: i32 = -1;
}

/// Map an AL object kind string (from the file index) to a BC `ObjectTypeWrapper` integer.
///
/// The kind strings come from the workspace file index (lowercased AL object type names).
/// Returns `bc_object_type::UNKNOWN` for unrecognised kinds.
pub fn kind_to_object_type(kind: &str) -> i32 {
    match kind.to_lowercase().as_str() {
        "table" => bc_object_type::TABLE,
        "report" => bc_object_type::REPORT,
        "codeunit" => bc_object_type::CODEUNIT,
        "xmlport" => bc_object_type::XMLPORT,
        "page" => bc_object_type::PAGE,
        "query" => bc_object_type::QUERY,
        "pageextension" => bc_object_type::PAGE_EXTENSION,
        "tableextension" => bc_object_type::TABLE_EXTENSION,
        "enum" => bc_object_type::ENUM,
        "enumextension" => bc_object_type::ENUM_EXTENSION,
        "reportextension" => bc_object_type::REPORT_EXTENSION,
        _ => bc_object_type::UNKNOWN,
    }
}

/// Object info resolved from the workspace symbol index.
/// Used to map file paths to BC object types and IDs for breakpoints.
#[derive(Debug, Clone)]
pub struct ResolvedObject {
    /// BC ObjectTypeWrapper value — use `bc_object_type` constants.
    pub object_type: i32,
    pub object_id: i32,
}

const VARIABLE_HANDLE_BASE: i64 = 1 << 18;
const MAX_VARIABLE_HANDLES: usize = 65_536;
const SCOPE_GLOBALS: i64 = 1;
const SCOPE_LOCALS: i64 = 2;

#[derive(Debug, Clone)]
struct VariableHandle {
    frame_id: i64,
    parent_path: String,
    /// BC sometimes supplies children inline and sometimes requires an
    /// `ExpandNode` request. `None` retains that distinction for lazy loading.
    nodes: Option<Vec<serde_json::Value>>,
}

#[derive(Debug)]
struct VariableHandleStore {
    next: i64,
    handles: HashMap<i64, VariableHandle>,
}

impl Default for VariableHandleStore {
    fn default() -> Self {
        Self {
            next: VARIABLE_HANDLE_BASE,
            handles: HashMap::new(),
        }
    }
}

impl VariableHandleStore {
    fn create(&mut self, handle: VariableHandle) -> Option<i64> {
        if self.handles.len() >= MAX_VARIABLE_HANDLES {
            return None;
        }
        let reference = self.next;
        self.next = self.next.checked_add(1)?;
        self.handles.insert(reference, handle);
        Some(reference)
    }

    fn get(&self, reference: i64) -> Option<VariableHandle> {
        self.handles.get(&reference).cloned()
    }

    fn reset(&mut self) {
        self.next = VARIABLE_HANDLE_BASE;
        self.handles.clear();
    }
}

fn scope_reference(group: i64, frame_id: i64) -> Option<i64> {
    let frame_id = u16::try_from(frame_id).ok()?;
    Some((group << 16) | i64::from(frame_id))
}

fn decode_scope_reference(reference: i64) -> Option<(i64, i64)> {
    if !(0..VARIABLE_HANDLE_BASE).contains(&reference) {
        return None;
    }
    let group = reference >> 16;
    if !matches!(group, SCOPE_GLOBALS | SCOPE_LOCALS) {
        return None;
    }
    Some((group, reference & 0xffff))
}

/// Run the native DAP server on stdio.
///
/// `acquire_token` is a callback to get an OAuth access token for the given tenant.
/// `resolve_object` maps a file path to its AL object type + ID using the workspace index.
/// `resolve_path` is the reverse: given a BC (ObjectType, ObjectNumber) returns the source file.
/// A parsed `setBreakpoints` request entry: `(line, condition)`. `condition`
/// is `""` when the client sent none.
type BpRequest = (i64, String);

/// `setBreakpoints` requests queued per source path while no debug session
/// exists yet.
type PendingBreakpoints = HashMap<String, Vec<BpRequest>>;

/// Both are provided by the caller (al-lsp binary) since they depend on `crate::symbols`.
/// Shared state and host callbacks for the native DAP server.
pub(crate) struct NativeDapState<F, R, P, C, A> {
    /// Single monotonic sequence counter shared between handlers and the
    /// background event-forwarding task. DAP requires non-decreasing seq
    /// values across all messages sent to the client.
    seq: Arc<AtomicU64>,
    session: Arc<Mutex<Option<Arc<BcDebugSession>>>>,
    debug_config: Arc<Mutex<Option<BcDebugConfig>>>,
    breakpoints: Arc<Mutex<HashMap<String, Vec<i64>>>>,
    /// `setBreakpoints` requests received before a debug session exists
    /// (i.e. during DAP configuration, right after `initialized`). DAP
    /// clients send these before `launch`/`attach` completes, so they can't
    /// be resolved against BC yet. Keyed by source path, replaced wholesale
    /// on each `setBreakpoints` call for that path (mirrors BC's own
    /// full-replace semantics). Drained and applied once a session starts
    /// (`apply_pending_breakpoints`), which also emits `breakpoint` events
    /// updating verification for the client's initially-unverified rows.
    pending_breakpoints: Arc<Mutex<PendingBreakpoints>>,
    /// True once `DebugAdapterConfigurationDone` has been accepted by BC for
    /// the current session. Current BC online rejects it until
    /// `OnAttachedToConnection` fires, so a rejected first attempt must be
    /// retried by the background event-forwarding task rather than left
    /// permanently unconfigured.
    configured: Arc<Mutex<bool>>,
    variable_handles: Arc<Mutex<VariableHandleStore>>,
    /// Cancellation channel for the background event-forwarding task.
    /// When a new debug session starts we send a new value so the old task exits.
    cancel_tx: watch::Sender<u64>,
    cancel_rx: watch::Receiver<u64>,
    /// Channel for the BC-event forwarding task to send pre-serialized DAP
    /// event bytes to the main loop (bounded to 1024 messages).
    dap_event_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    project_root: String,
    acquire_token: F,
    resolve_object: R,
    resolve_path: P,
    /// Compile the project; `Ok(build log)` on success, `Err(log)` on failure.
    /// Injected by the caller (al-lsp) so this crate never names the build /
    /// emit pipeline.
    compile: C,
    /// Locate the deploy `.app` for a project root. Injected for the same
    /// reason as `compile`.
    find_app: A,
}

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
    /// Dispatch one DAP request to its handler. Returns `Ok(true)` when the
    /// server loop should exit (disconnect/terminate).
    pub(crate) async fn handle_request<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        command: &str,
        request_seq: i64,
        arguments: &serde_json::Value,
    ) -> Result<bool> {
        match command {
            "initialize" => self.handle_initialize(out, request_seq, command).await?,
            "configurationDone" => {
                self.handle_configuration_done(out, request_seq, command)
                    .await?
            }
            "launch" | "attach" => {
                self.handle_launch_attach(out, request_seq, command, arguments)
                    .await?
            }
            "setBreakpoints" => {
                self.handle_set_breakpoints(out, request_seq, command, arguments)
                    .await?
            }
            "next" | "stepIn" | "stepOut" => self.handle_step(out, request_seq, command).await?,
            "pause" => self.handle_pause(out, request_seq, command).await?,
            "setFunctionBreakpoints" | "setVariable" | "completions" | "restart" | "stepBack" => {
                self.handle_unsupported_capability(out, request_seq, command)
                    .await?
            }
            "continue" => self.handle_continue(out, request_seq, command).await?,
            "threads" => self.handle_threads(out, request_seq, command).await?,
            "stackTrace" => {
                self.handle_stack_trace(out, request_seq, command, arguments)
                    .await?
            }
            "scopes" => {
                self.handle_scopes(out, request_seq, command, arguments)
                    .await?
            }
            "variables" => {
                self.handle_variables(out, request_seq, command, arguments)
                    .await?
            }
            "evaluate" => {
                self.handle_evaluate(out, request_seq, command, arguments)
                    .await?
            }
            "disconnect" | "terminate" => {
                self.handle_disconnect(out, request_seq, command).await?;
                return Ok(true);
            }
            _ => self.handle_unknown(out, request_seq, command).await?,
        }
        Ok(false)
    }

    async fn handle_initialize<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        let resp = make_response(
            &self.seq,
            request_seq,
            command,
            true,
            Some(serde_json::json!({
                "supportsConfigurationDoneRequest": true,
                "supportsFunctionBreakpoints": false,
                "supportsConditionalBreakpoints": true,
                "supportsEvaluateForHovers": true,
                "supportsStepBack": false,
                "supportsSetVariable": false,
                "supportsRestartFrame": false,
                "supportsCompletionsRequest": false,
                "supportsTerminateRequest": true,
                "supportsDelayedStackTraceLoading": true,
                "supportsRestartRequest": false,
            })),
            None,
        );
        write_dap(out, &resp).await?;

        let evt = make_event(&self.seq, "initialized", None);
        write_dap(out, &evt).await?;
        Ok(())
    }

    async fn handle_configuration_done<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        // Clone the Arc before dropping the lock so we don't hold the mutex
        // guard across the async invoke() call.
        let session_arc = self.session.lock().await.clone();
        if let Some(s) = session_arc {
            // Current BC online rejects DebugAdapterConfigurationDone until
            // OnAttachedToConnection fires — which for break-on-next
            // web-client launches happens only after the browser attaches,
            // i.e. after the client already sent this very request. Attempt
            // immediately when already attached (fast path); otherwise the
            // background event-forwarding task retries on every poll once
            // `is_attached()` flips true (same retry the MCP path performs
            // in `NativeDebugSession::drain_events`).
            try_configuration_done(&s, &self.debug_config, &self.configured).await;
        }
        write_dap(
            out,
            &make_response(&self.seq, request_seq, command, true, None, None),
        )
        .await?;
        Ok(())
    }

    async fn handle_launch_attach<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let config = BcDebugConfig::from_dap_args(arguments);
        if let Err(error) = config.validate_native() {
            write_dap(
                out,
                &make_response(&self.seq, request_seq, command, false, None, Some(error)),
            )
            .await?;
            return Ok(());
        }
        self.variable_handles.lock().await.reset();
        *self.configured.lock().await = false;
        // Store config for use in the configurationDone handler.
        *self.debug_config.lock().await = Some(config.clone());

        let mut onprem_web_base = None;
        if command == "launch" {
            write_dap(
                out,
                &make_event(
                    &self.seq,
                    "output",
                    Some(serde_json::json!({
                        "category": "console",
                        "output": "Compiling AL project...\r\n"
                    })),
                ),
            )
            .await?;
            // Native-first: build the deploy `.app` with the shared build
            // service. The native emitter needs neither `alc` nor a configured
            // Microsoft toolchain, so launch must never skip compilation merely
            // because those optional components are absent.
            let compile_outcome = (self.compile)(PathBuf::from(&self.project_root)).await;
            match compile_outcome {
                Ok(output) => {
                    if !output.is_empty() {
                        write_dap(
                            out,
                            &make_event(
                                &self.seq,
                                "output",
                                Some(serde_json::json!({
                                    "category": "console",
                                    "output": format!("{output}\r\n")
                                })),
                            ),
                        )
                        .await?;
                    }
                    write_dap(
                        out,
                        &make_event(
                            &self.seq,
                            "output",
                            Some(serde_json::json!({
                                "category": "console",
                                "output": "Compilation succeeded.\r\n"
                            })),
                        ),
                    )
                    .await?;
                }
                Err(e) => {
                    write_dap(
                        out,
                        &make_event(
                            &self.seq,
                            "output",
                            Some(serde_json::json!({
                                "category": "stderr",
                                "output": format!("Compilation failed: {e}\r\n")
                            })),
                        ),
                    )
                    .await?;
                    write_dap(
                        out,
                        &make_response(
                            &self.seq,
                            request_seq,
                            command,
                            false,
                            None,
                            Some(format!("Compilation failed: {e}")),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }

        write_dap(
            out,
            &make_event(
                &self.seq,
                "output",
                Some(serde_json::json!({
                    "category": "console",
                    "output": format!("Authenticating to tenant {}...\r\n", config.tenant)
                })),
            ),
        )
        .await?;

        let token = match (self.acquire_token)(config.tenant.clone()).await {
            Ok(t) => t,
            Err(e) => {
                write_dap(
                    out,
                    &make_response(
                        &self.seq,
                        request_seq,
                        command,
                        false,
                        None,
                        Some(format!("Authentication failed: {e}")),
                    ),
                )
                .await?;
                return Ok(());
            }
        };

        if command == "launch" {
            write_dap(
                out,
                &make_event(
                    &self.seq,
                    "output",
                    Some(serde_json::json!({
                        "category": "console",
                        "output": "Publishing package...\r\n"
                    })),
                ),
            )
            .await?;

            if config.accept_invalid_certs {
                al_bc::http_auth::warn_insecure_tls("DAP launch");
                write_dap(
                    out,
                    &make_event(
                        &self.seq,
                        "output",
                        Some(serde_json::json!({
                            "category": "important",
                            "output": format!(
                                "{}\r\n",
                                al_bc::http_auth::insecure_tls_message("DAP launch")
                            ),
                        })),
                    ),
                )
                .await?;
            }

            let http = reqwest::Client::builder()
                .danger_accept_invalid_certs(config.accept_invalid_certs)
                .build()
                .map_err(|e| DapError::PublishFailed(e.to_string()))?;

            if config.launch_browser && config.environment_type.eq_ignore_ascii_case("OnPrem") {
                match get_web_endpoint(&http, &config, &token).await {
                    Ok(endpoint) => onprem_web_base = Some(endpoint),
                    Err(error) => {
                        write_dap(
                            out,
                            &make_response(
                                &self.seq,
                                request_seq,
                                command,
                                false,
                                None,
                                Some(format!(
                                    "Cannot resolve the on-premises Web client URL: {error}"
                                )),
                            ),
                        )
                        .await?;
                        return Ok(());
                    }
                }
            }

            let app_path = match (self.find_app)(Path::new(&self.project_root)) {
                Ok(app_path) => app_path,
                Err(error) => {
                    write_dap(
                        out,
                        &make_response(
                            &self.seq,
                            request_seq,
                            command,
                            false,
                            None,
                            Some(format!("Cannot locate compiled package: {error}")),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            };
            if let Some(app_path) = app_path {
                match publish_app(&http, &config, &token, &app_path).await {
                    Ok(()) => {
                        write_dap(
                            out,
                            &make_event(
                                &self.seq,
                                "output",
                                Some(serde_json::json!({
                                    "category": "console",
                                    "output": "Package published successfully.\r\n"
                                })),
                            ),
                        )
                        .await?;
                    }
                    Err(e) => {
                        write_dap(
                            out,
                            &make_response(
                                &self.seq,
                                request_seq,
                                command,
                                false,
                                None,
                                Some(format!("Publish failed: {e}")),
                            ),
                        )
                        .await?;
                        return Ok(());
                    }
                }
            } else {
                // a missing .app means compile failed (or
                // hasn't run). Continuing into publish/attach would
                // either silently use a stale .app from a previous
                // build (worse — debugging the wrong source) or
                // produce a confusing "Connect failed" trail. Fail
                // the launch with a clear error so the user sees
                // the compile failure as the root cause.
                write_dap(
                            out,
                            &make_response(
                                &self.seq,
                                request_seq,
                                command,
                                false,
                                None,
                                Some(
                                    "No compiled .app found in project root — compile must succeed before launch (run `al-explorer compile`)."
                                        .to_string(),
                                ),
                            ),
                        )
                        .await?;
                return Ok(());
            }
        }

        write_dap(
            out,
            &make_event(
                &self.seq,
                "output",
                Some(serde_json::json!({
                    "category": "console",
                    "output": "Connecting to debug hub...\r\n"
                })),
            ),
        )
        .await?;

        match BcDebugSession::connect(&config, &token).await {
            Ok(debug_session) => {
                if let Err(e) = debug_session.attach(&config).await {
                    write_dap(
                        out,
                        &make_response(
                            &self.seq,
                            request_seq,
                            command,
                            false,
                            None,
                            Some(format!("Attach failed: {e}")),
                        ),
                    )
                    .await?;
                    return Ok(());
                }

                // Capture connection ID before moving session into Arc+mutex
                let conn_id = debug_session.connection_id.clone();
                let web_url = if command == "launch" && config.launch_browser {
                    match build_debug_browser_url(&config, &conn_id, onprem_web_base.as_deref()) {
                        Ok(url) => Some(url),
                        Err(error) => {
                            write_dap(
                                out,
                                &make_response(
                                    &self.seq,
                                    request_seq,
                                    command,
                                    false,
                                    None,
                                    Some(format!("Cannot build the Web client URL: {error}")),
                                ),
                            )
                            .await?;
                            return Ok(());
                        }
                    }
                } else {
                    None
                };
                let session_arc = Arc::new(debug_session);
                *self.session.lock().await = Some(session_arc.clone());

                self.spawn_event_forwarder();

                write_dap(
                    out,
                    &make_event(
                        &self.seq,
                        "output",
                        Some(serde_json::json!({
                            "category": "console",
                            "output": "Debug session started.\r\n"
                        })),
                    ),
                )
                .await?;

                write_dap(
                    out,
                    &make_response(&self.seq, request_seq, command, true, None, None),
                )
                .await?;

                // Re-apply any breakpoints the client set during DAP
                // configuration (before this session existed) — they were
                // answered `verified: false` at the time and queued rather
                // than lost. Now that a session exists, resolve/add them on
                // BC and tell the client their real verification state via
                // `breakpoint` events.
                self.apply_pending_breakpoints(&session_arc, out).await?;

                // Open browser with debug context params (must match SignalR ConnectionId)
                if let Some(web_url) = web_url {
                    write_dap(
                        out,
                        &make_event(
                            &self.seq,
                            "al/openUri",
                            Some(serde_json::json!({
                                "uri": web_url
                            })),
                        ),
                    )
                    .await?;

                    if !open_browser(&web_url) {
                        tracing::warn!(url = %web_url,
                                    "DAP launch: could not auto-open browser for AAD \
                                     device-code; user must navigate manually");
                    }
                }
            }
            Err(e) => {
                write_dap(
                    out,
                    &make_response(
                        &self.seq,
                        request_seq,
                        command,
                        false,
                        None,
                        Some(format!("Debug hub connection failed: {e}")),
                    ),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn handle_set_breakpoints<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let source_path = arguments
            .get("source")
            .and_then(|s| s.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let bp_requests = arguments
            .get("breakpoints")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let parsed = parse_bp_requests(&bp_requests);

        // Clone the Arc<BcDebugSession> while holding the session mutex,
        // then drop the guard immediately so no mutex is held across
        // the async breakpoint operations below (prevents deadlock).
        let session_arc = self.session.lock().await.clone();

        let result_bps = if let Some(s) = session_arc {
            // Resolve object type and ID from workspace symbol index.
            match (self.resolve_object)(&source_path) {
                Some(obj) => {
                    self.apply_breakpoints_to_session(
                        &s,
                        &source_path,
                        &parsed,
                        obj.object_type,
                        obj.object_id,
                    )
                    .await
                }
                None => unresolved_object_breakpoints(&source_path, &parsed),
            }
        } else {
            // DAP clients (Zed included) send `setBreakpoints` during the
            // configuration phase — right after `initialized`, well before
            // `launch`/`attach` creates a session. Losing these would mean
            // every breakpoint set before launch silently never fires.
            // Queue them (replacing whatever was queued for this source
            // before) and answer unverified-pending rather than permanently
            // failed; `apply_pending_breakpoints` re-applies and re-verifies
            // them once a session exists.
            self.pending_breakpoints
                .lock()
                .await
                .insert(source_path.clone(), parsed.clone());
            parsed
                .iter()
                .map(|(line, _)| {
                    serde_json::json!({
                        "verified": false,
                        "line": line,
                        "message": "Debug session not started yet; breakpoint will be verified once the session starts",
                    })
                })
                .collect()
        };

        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({"breakpoints": result_bps})),
                None,
            ),
        )
        .await?;
        Ok(())
    }

    /// Add/replace the full breakpoint set for one source against a *live*
    /// BC session: remove any previously-tracked ids for that source, add
    /// the requested ones, and return each request's DAP `Breakpoint`
    /// result. Shared by `setBreakpoints` (session already exists) and
    /// `apply_pending_breakpoints` (session just started, applying requests
    /// queued while there was none).
    async fn apply_breakpoints_to_session(
        &self,
        session: &BcDebugSession,
        source_path: &str,
        bp_requests: &[BpRequest],
        object_type: i32,
        object_id: i32,
    ) -> Vec<serde_json::Value> {
        let mut result_bps = Vec::new();

        // Hold the breakpoints lock for the ENTIRE remove → add → store
        // cycle so two concurrent setBreakpoints calls on the same
        // source_path serialise correctly. Without this hold-across-
        // await (tokio::sync::Mutex makes that safe), both callers
        // would read the same `old_ids`, both remove the same set on
        // BC, both add fresh breakpoints, and one caller's `new_ids`
        // would overwrite the other in the map — leaving the BC
        // server's bp set as the union of both adds but the local map
        // tracking only one half and orphaning the rest.
        let mut bps = self.breakpoints.lock().await;
        let old_ids: Vec<i64> = bps.remove(source_path).unwrap_or_default();
        for id in old_ids {
            if let Err(e) = session.remove_breakpoint(id).await {
                tracing::warn!(
                    breakpoint_id = id,
                    error = %e,
                    "DAP setBreakpoints: removing prior breakpoint failed; \
                     local state will be overwritten regardless"
                );
            }
        }

        let mut new_ids = Vec::new();
        for (line, condition) in bp_requests {
            let server_line = line.saturating_sub(1);

            match session
                .add_breakpoint(object_type, object_id, server_line, 0, condition)
                .await
            {
                Ok(result) => {
                    // BC's add_breakpoint can return Ok(Value::Null) or a
                    // payload without an Id field (bc_debug.rs:945). A
                    // breakpoint id of 0 is not a usable handle: we could
                    // neither remove it on a later setBreakpoints nor honour
                    // a "verified: true" claim. Treat a missing/zero id as a
                    // failure rather than recording an orphaned breakpoint.
                    match extract_breakpoint_id(&result) {
                        Some(bp_id) => {
                            new_ids.push(bp_id);
                            result_bps.push(serde_json::json!({
                                "id": bp_id,
                                "verified": true,
                                "line": line,
                            }));
                        }
                        None => {
                            tracing::warn!(
                                line = line,
                                ?result,
                                "DAP setBreakpoints: BC accepted the breakpoint \
                                         but returned no usable id; not tracking it"
                            );
                            result_bps.push(serde_json::json!({
                                        "verified": false,
                                        "line": line,
                                        "message": "Breakpoint created but ID could not be extracted from BC response",
                                    }));
                        }
                    }
                }
                Err(e) => {
                    result_bps.push(serde_json::json!({
                        "verified": false,
                        "line": line,
                        "message": e.to_string(),
                    }));
                }
            }
        }
        bps.insert(source_path.to_string(), new_ids);
        drop(bps);
        result_bps
    }

    /// Re-apply `setBreakpoints` requests that were queued while no debug
    /// session existed yet (see `handle_set_breakpoints`). Called once after
    /// `launch`/`attach` establishes a session: resolves each queued
    /// source's AL object, adds the breakpoints on BC via the same path
    /// live `setBreakpoints` uses, and emits a `breakpoint` event per
    /// breakpoint so the client updates its initially-unverified rows
    /// (matched by `source.path` + `line` since no `id` was known yet at
    /// the time of the original response).
    async fn apply_pending_breakpoints<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        session: &BcDebugSession,
        out: &mut W,
    ) -> Result<()> {
        let pending: PendingBreakpoints = {
            let mut guard = self.pending_breakpoints.lock().await;
            std::mem::take(&mut *guard)
        };

        for (source_path, bp_requests) in pending {
            let result_bps = match (self.resolve_object)(&source_path) {
                Some(obj) => {
                    self.apply_breakpoints_to_session(
                        session,
                        &source_path,
                        &bp_requests,
                        obj.object_type,
                        obj.object_id,
                    )
                    .await
                }
                None => unresolved_object_breakpoints(&source_path, &bp_requests),
            };

            for mut bp in result_bps {
                bp["source"] = serde_json::json!({ "path": &source_path });
                write_dap(
                    out,
                    &make_event(
                        &self.seq,
                        "breakpoint",
                        Some(serde_json::json!({
                            "reason": "changed",
                            "breakpoint": bp,
                        })),
                    ),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Step over / into / out — BC BreakpointExitReason 1 / 2 / 3 via
    /// SetBreakpointResponse. Merged: the three original arms differed only
    /// in which session method they invoked.
    async fn handle_step<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        let session_arc = self.session.lock().await.clone();
        if let Some(s) = session_arc {
            let step = match command {
                "stepIn" => s.step_in().await,
                "stepOut" => s.step_out().await,
                _ => s.step_over().await,
            };
            if let Err(e) = step {
                write_dap(
                    out,
                    &make_response(
                        &self.seq,
                        request_seq,
                        command,
                        false,
                        None,
                        Some(e.to_string()),
                    ),
                )
                .await?;
                return Ok(());
            }
            self.variable_handles.lock().await.reset();
        }
        write_dap(
            out,
            &make_response(&self.seq, request_seq, command, true, None, None),
        )
        .await?;
        Ok(())
    }

    async fn handle_pause<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        // BC's SignalR debug hub does not expose a "pause while running" method.
        // The BC debugger only pauses at breakpoints or on error; there is no
        // equivalent of a SIGSTOP that the client can trigger mid-execution.
        // Respond with failure so Zed shows the user a clear error instead of
        // silently doing nothing.
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                false,
                None,
                Some(
                    "pause is not supported by the BC debug hub; set a breakpoint instead"
                        .to_string(),
                ),
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_unsupported_capability<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        // Keep these command-specific failures aligned with the false
        // initialize capabilities. Protocol DTOs are not hub operations: the
        // live HubBasedDebuggerService has no method for any command below.
        let message = match command {
            "setFunctionBreakpoints" => {
                "function breakpoints are not supported by the BC debug hub; use source breakpoints instead"
            }
            "setVariable" => {
                "setVariable is not supported by the BC debug hub; variables can be inspected and evaluated but not mutated"
            }
            "completions" => {
                "completions are not supported by the native adapter; BC exposes no completion method and Microsoft's adapter delegates this request to its editor workspace"
            }
            "restart" => {
                "restart is not supported by the BC debug hub; disconnect and launch or attach a new debug session"
            }
            "stepBack" => {
                "stepBack is not supported by the BC debug hub; only continue, step-over, step-in, and step-out are available"
            }
            _ => unreachable!("only capability-gated commands are dispatched here"),
        };
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                false,
                None,
                Some(message.to_string()),
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_continue<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        let session_arc = self.session.lock().await.clone();
        if let Some(s) = session_arc {
            // BC expects BreakpointExitReason integer 0 (continue)
            if let Err(e) = s.continue_execution(serde_json::json!(0)).await {
                write_dap(
                    out,
                    &make_response(
                        &self.seq,
                        request_seq,
                        command,
                        false,
                        None,
                        Some(e.to_string()),
                    ),
                )
                .await?;
                return Ok(());
            }
            self.variable_handles.lock().await.reset();
        }
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({"allThreadsContinued": true})),
                None,
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_threads<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({"threads": [{"id": 1, "name": "AL Thread"}]})),
                None,
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_stack_trace<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let session_arc = self.session.lock().await.clone();
        let stack_frames = if let Some(s) = session_arc {
            match s.get_call_stack().await {
                Ok(frames) => bc_stack_to_dap(frames, &self.resolve_path),
                Err(e) => {
                    debug!("get_call_stack failed: {e}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let (page, total) = page_stack_frames(stack_frames, arguments);
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({
                    "stackFrames": page,
                    "totalFrames": total,
                })),
                None,
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_scopes<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let frame_id = arguments
            .get("frameId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let mut scopes = Vec::new();
        if self.session.lock().await.is_some() {
            match (
                scope_reference(SCOPE_GLOBALS, frame_id),
                scope_reference(SCOPE_LOCALS, frame_id),
            ) {
                (Some(globals_ref), Some(locals_ref)) => {
                    scopes.push(serde_json::json!({
                        "name": "Globals",
                        "variablesReference": globals_ref,
                        "expensive": true,
                    }));
                    scopes.push(serde_json::json!({
                        "name": "Locals",
                        "variablesReference": locals_ref,
                        "expensive": false,
                    }));
                }
                _ => {
                    write_dap(
                        out,
                        &make_response(
                            &self.seq,
                            request_seq,
                            command,
                            false,
                            None,
                            Some(format!(
                                "stack frame {frame_id} is outside the supported DAP range"
                            )),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({ "scopes": scopes })),
                None,
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_variables<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let vars_ref = arguments
            .get("variablesReference")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let session_arc = self.session.lock().await.clone();
        let Some(session) = session_arc else {
            write_dap(
                out,
                &make_response(
                    &self.seq,
                    request_seq,
                    command,
                    true,
                    Some(serde_json::json!({"variables": []})),
                    None,
                ),
            )
            .await?;
            return Ok(());
        };

        let resolved = if vars_ref >= VARIABLE_HANDLE_BASE {
            let handle = self.variable_handles.lock().await.get(vars_ref);
            if let Some(handle) = handle {
                let nodes = match handle.nodes {
                    Some(nodes) => Ok(nodes),
                    None => session
                        .expand_node(handle.frame_id, &handle.parent_path)
                        .await
                        .map(json_array),
                };
                nodes.map(|nodes| (handle.frame_id, handle.parent_path, nodes))
            } else {
                Ok((0, String::new(), Vec::new()))
            }
        } else if let Some((group, frame_id)) = decode_scope_reference(vars_ref) {
            match session.get_variables(frame_id).await {
                Ok(root_nodes) if group == SCOPE_LOCALS => Ok((
                    frame_id,
                    String::new(),
                    local_nodes_from_frame_variables(&root_nodes),
                )),
                Ok(root_nodes) => match inline_global_nodes(&root_nodes) {
                    Some(nodes) => Ok((frame_id, String::new(), nodes)),
                    None => session.get_globals(frame_id).await.map(|expanded| {
                        let (parent_path, nodes) = expanded_global_nodes(&expanded);
                        (frame_id, parent_path, nodes)
                    }),
                },
                Err(error) => Err(error),
            }
        } else {
            Ok((0, String::new(), Vec::new()))
        };

        let (frame_id, parent_path, nodes) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                write_dap(
                    out,
                    &make_response(
                        &self.seq,
                        request_seq,
                        command,
                        false,
                        None,
                        Some(format!("Business Central variable request failed: {error}")),
                    ),
                )
                .await?;
                return Ok(());
            }
        };
        let variables = {
            let mut handles = self.variable_handles.lock().await;
            bc_vars_to_dap(&nodes, frame_id, &parent_path, &mut handles)
        };
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({"variables": variables})),
                None,
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_evaluate<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let expression = arguments
            .get("expression")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let frame_id = arguments
            .get("frameId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        // Clone Arc and drop guard before async work.
        let session_arc = self.session.lock().await.clone();
        let result = match session_arc {
            Some(session) => match session.evaluate(frame_id, expression).await {
                Ok(result) => result,
                Err(error) => {
                    write_dap(
                        out,
                        &make_response(
                            &self.seq,
                            request_seq,
                            command,
                            false,
                            None,
                            Some(format!("Business Central evaluation failed: {error}")),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            },
            None => serde_json::Value::Null,
        };
        let display = bc_node_display_value(&result);
        let type_name = result
            .get("TypeName")
            .or_else(|| result.get("typeName"))
            .or_else(|| result.get("Type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let children = bc_node_children(&result);
        let has_children = bc_node_has_children(&result)
            || children.as_ref().is_some_and(|value| !value.is_empty());
        let (variables_reference, named_variables) = if has_children && !expression.is_empty() {
            let named_variables = children.as_ref().map(Vec::len);
            let reference = self
                .variable_handles
                .lock()
                .await
                .create(VariableHandle {
                    frame_id,
                    parent_path: expression.to_string(),
                    nodes: children,
                })
                .unwrap_or(0);
            (reference, named_variables)
        } else {
            (0, None)
        };
        let mut body = serde_json::json!({
            "result": display,
            "type": type_name,
            "variablesReference": variables_reference,
        });
        if let Some(count) = named_variables {
            body["namedVariables"] = serde_json::json!(count);
        }
        write_dap(
            out,
            &make_response(&self.seq, request_seq, command, true, Some(body), None),
        )
        .await?;
        Ok(())
    }

    async fn handle_disconnect<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        // Clone Arc, drop guard, then stop (don't hold mutex across await).
        let session_arc = self.session.lock().await.clone();
        if let Some(s) = session_arc {
            let _ = s.stop_debugging().await;
        }
        *self.session.lock().await = None;
        self.variable_handles.lock().await.reset();
        write_dap(
            out,
            &make_response(&self.seq, request_seq, command, true, None, None),
        )
        .await?;

        write_dap(out, &make_event(&self.seq, "terminated", None)).await?;
        Ok(())
    }

    async fn handle_unknown<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                false,
                None,
                Some(format!("Unsupported command: {command}")),
            ),
        )
        .await?;
        Ok(())
    }

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
    fn spawn_event_forwarder(&self) {
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

pub async fn run_native_dap<F, Fut, R, P, C, CompileFut, A>(
    project_root: &str,
    acquire_token: F,
    resolve_object: R,
    resolve_path: P,
    compile: C,
    find_app: A,
) -> Result<()>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    R: Fn(&str) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    A: Fn(&Path) -> std::result::Result<Option<PathBuf>, String> + Send + Sync + 'static,
{
    let (cancel_tx, cancel_rx) = watch::channel(0u64);
    let (dap_event_tx, mut dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);

    let state = NativeDapState {
        seq: Arc::new(AtomicU64::new(1)),
        session: Arc::new(Mutex::new(None)),
        debug_config: Arc::new(Mutex::new(None)),
        breakpoints: Arc::new(Mutex::new(HashMap::new())),
        pending_breakpoints: Arc::new(Mutex::new(HashMap::new())),
        configured: Arc::new(Mutex::new(false)),
        variable_handles: Arc::new(Mutex::new(VariableHandleStore::default())),
        cancel_tx,
        cancel_rx,
        dap_event_tx,
        project_root: project_root.to_string(),
        acquire_token,
        resolve_object,
        resolve_path,
        compile,
        find_app,
    };

    let mut stdout = io::stdout();

    // Read stdin in a dedicated task that forwards each parsed DAP request
    // body to a channel. The main loop then `select!`s over client requests and
    // BC push events — both channel `recv()`s, which are cancel-safe — so a
    // `stopped`/`output` event queued while the adapter is parked (e.g. after a
    // `continue`, when the client sends nothing and waits for `stopped`) is
    // written to stdout immediately instead of stalling until the next client
    // request. Selecting on `read_dap_body` directly would risk cancelling a
    // partial read and desyncing the framing, so the blocking read lives in its
    // own never-cancelled task.
    let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
    tokio::spawn(async move {
        let mut stdin = BufReader::new(io::stdin());
        loop {
            match read_dap_body(&mut stdin).await {
                Ok(body) => {
                    if req_tx.send(body).await.is_err() {
                        break; // main loop gone
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    error!("Read error: {e}");
                    break;
                }
            }
        }
    });

    loop {
        let body = tokio::select! {
            // A BC push event queued by the forwarder — write it right away.
            Some(frame) = dap_event_rx.recv() => {
                if let Err(e) = write_dap_frame(&mut stdout, &frame).await {
                    warn!("Failed to write BC event to Zed: {e}");
                }
                continue;
            }
            // A client request from the stdin-reader task.
            maybe_body = req_rx.recv() => match maybe_body {
                Some(b) => b,
                None => break, // stdin closed (EOF) or read error
            },
        };

        let msg: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                warn!("Bad JSON from client: {e}");
                continue;
            }
        };

        let command = msg
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let request_seq = msg.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
        let arguments = msg
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        debug!("DAP request: {command} (seq={request_seq})");

        if state
            .handle_request(&mut stdout, &command, request_seq, &arguments)
            .await?
        {
            break;
        }
    }

    info!("Native DAP server shutting down");
    Ok(())
}

fn make_response(
    seq: &AtomicU64,
    request_seq: i64,
    command: &str,
    success: bool,
    body: Option<serde_json::Value>,
    message: Option<String>,
) -> serde_json::Value {
    let s = seq.fetch_add(1, Ordering::Relaxed);
    let mut resp = serde_json::json!({
        "seq": s,
        "type": "response",
        "request_seq": request_seq,
        "requestSeq": request_seq,
        "success": success,
        "command": command,
    });
    if let Some(b) = body {
        resp["body"] = b;
    }
    if let Some(m) = message {
        resp["message"] = serde_json::json!(m);
    }
    resp
}

fn make_event(seq: &AtomicU64, event: &str, body: Option<serde_json::Value>) -> serde_json::Value {
    let s = seq.fetch_add(1, Ordering::Relaxed);
    let mut evt = serde_json::json!({
        "seq": s,
        "type": "event",
        "event": event,
    });
    if let Some(b) = body {
        evt["body"] = b;
    }
    evt
}

/// Build the browser URL that opens the BC debug context for a launch session.
///
/// For cloud sessions the tenant and environment name are percent-encoded so
/// values containing special characters (spaces, ampersands, slashes) produce
/// valid URLs — matching the encoding already applied in
/// [`bc_debug::BcDebugConfig::base_url`] and `debug_hub_url`.
pub fn build_debug_browser_url(
    config: &BcDebugConfig,
    conn_id: &str,
    onprem_web_base: Option<&str>,
) -> Result<String> {
    let object_parameter = match config.startup_object_type.to_ascii_lowercase().as_str() {
        "table" => "table",
        "report" => "report",
        "query" => "query",
        _ => "page",
    };
    let base = if config.environment_type.eq_ignore_ascii_case("OnPrem") {
        onprem_web_base
            .ok_or_else(|| {
                DapError::ConnectionFailed(
                    "on-premises browser launch requires the server-provided PublicWebBaseUrl"
                        .to_string(),
                )
            })?
            .to_string()
    } else {
        let tenant = percent_encode_url(&config.tenant);
        let env = percent_encode_url(config.environment_name.as_deref().unwrap_or("sandbox"));
        format!("https://businesscentral.dynamics.com/{tenant}/{env}/")
    };
    let mut url = url::Url::parse(&base).map_err(|error| {
        DapError::ConnectionFailed(format!("invalid Web client base URL `{base}`: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(DapError::ConnectionFailed(format!(
            "unsupported Web client base URL `{base}`"
        )));
    }
    {
        let mut query = url.query_pairs_mut();
        query.append_pair(object_parameter, &config.startup_object_id.to_string());
        if let Some(company) = config
            .startup_company
            .as_deref()
            .filter(|company| !company.is_empty())
        {
            query.append_pair("company", company);
        }
        if !config.environment_type.eq_ignore_ascii_case("OnPrem") {
            query.append_pair("noSignUpCheck", "1");
        }
        query.append_pair("connectioncontext", conn_id);
        query.append_pair("debuggingcontext", conn_id);
        query.append_pair("sk", conn_id);
    }
    Ok(url.to_string())
}

async fn write_dap<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &serde_json::Value,
) -> std::result::Result<(), std::io::Error> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_dap_frame(writer, &body).await
}

fn open_browser(url: &str) -> bool {
    let ok = {
        #[cfg(target_os = "linux")]
        {
            try_spawn("xdg-open", &[url])
        }
        #[cfg(target_os = "macos")]
        {
            try_spawn("open", &[url])
        }
        #[cfg(target_os = "windows")]
        {
            try_spawn("cmd", &["/c", "start", url])
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            false
        }
    };

    if !ok {
        warn!("open_browser: platform opener failed for URL: {url}");
    }

    ok
}

fn try_spawn(cmd: &str, args: &[&str]) -> bool {
    std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Pull a usable breakpoint id out of BC's `AddBreakpoint` response.
///
/// Current BC returns `BreakpointId`; older variants used `Id`/`id`. BC may
/// also answer with `Value::Null` or a payload lacking any usable ID
/// (see `bc_debug::add_breakpoint`). An id of `0` is not a valid handle — we
/// could neither later remove it nor truthfully report `verified: true` — so a
/// missing or zero id maps to `None`.
fn extract_breakpoint_id(result: &serde_json::Value) -> Option<i64> {
    result
        .get("BreakpointId")
        .or_else(|| result.get("breakpointId"))
        .or_else(|| result.get("Id"))
        .or_else(|| result.get("id"))
        .and_then(|v| v.as_i64())
        .filter(|&id| id != 0)
}

fn json_array(value: serde_json::Value) -> Vec<serde_json::Value> {
    value.as_array().cloned().unwrap_or_default()
}

fn bc_node_name(node: &serde_json::Value) -> Option<&str> {
    node.get("Name")
        .or_else(|| node.get("name"))
        .and_then(serde_json::Value::as_str)
}

fn bc_node_is(node: &serde_json::Value, expected: &str) -> bool {
    bc_node_name(node).is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

fn bc_node_children(node: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    node.get("Children")
        .or_else(|| node.get("children"))
        .and_then(serde_json::Value::as_array)
        .cloned()
}

fn bc_node_has_children(node: &serde_json::Value) -> bool {
    node.get("HasChildren")
        .or_else(|| node.get("hasChildren"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || bc_node_children(node).is_some_and(|children| !children.is_empty())
}

fn local_nodes_from_frame_variables(variables: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(nodes) = variables.as_array() else {
        return Vec::new();
    };
    let mut start = usize::from(
        nodes
            .first()
            .is_some_and(|node| bc_node_is(node, "<Globals>")),
    );
    if nodes
        .get(start)
        .is_some_and(|node| bc_node_is(node, "<Database Statistics>"))
    {
        start += 1;
    }
    nodes[start..].to_vec()
}

/// Return globals already embedded in the `GetVariables` root node. `None`
/// means BC advertised children but requires the separate `ExpandGlobals` RPC.
fn inline_global_nodes(variables: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    let globals = variables.as_array()?.first()?;
    if !bc_node_is(globals, "<Globals>") {
        return None;
    }
    if let Some(children) = bc_node_children(globals) {
        return Some(children);
    }
    (!bc_node_has_children(globals)).then(Vec::new)
}

fn expanded_global_nodes(variables: &serde_json::Value) -> (String, Vec<serde_json::Value>) {
    let nodes = variables.as_array().cloned().unwrap_or_default();
    let Some(first) = nodes.first() else {
        return (String::new(), Vec::new());
    };
    if bc_node_is(first, "<Globals>") {
        let mut flattened = bc_node_children(first).unwrap_or_default();
        flattened.extend(nodes.into_iter().skip(1));
        return (String::new(), flattened);
    }

    // This is the legacy server shape handled by Microsoft's adapter: the
    // first expanded node names the parent whose children follow.
    (
        bc_node_name(first)
            .map(quote_al_identifier)
            .unwrap_or_default(),
        nodes,
    )
}

fn quote_al_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn bc_node_display_value(node: &serde_json::Value) -> String {
    let value = node
        .get("Summary")
        .or_else(|| node.get("summary"))
        .or_else(|| node.get("Value"))
        .or_else(|| node.get("value"));
    match value {
        Some(serde_json::Value::String(value)) => value
            .strip_prefix("\r\n")
            .or_else(|| value.strip_prefix('\n'))
            .unwrap_or(value)
            .to_string(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Convert BC `LocalNode` values into DAP variables while retaining structured
/// child nodes. BC casing varies by server version, so every wire field accepts
/// both its Newtonsoft PascalCase form and camelCase variants.
fn bc_vars_to_dap(
    nodes: &[serde_json::Value],
    frame_id: i64,
    parent_path: &str,
    handles: &mut VariableHandleStore,
) -> Vec<serde_json::Value> {
    nodes
        .iter()
        .filter_map(|node| {
            let name = bc_node_name(node)?;
            let value = bc_node_display_value(node);
            let type_name = node
                .get("TypeName")
                .or_else(|| node.get("typeName"))
                .or_else(|| node.get("Type"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let quoted_name = quote_al_identifier(name);
            let path = if parent_path.is_empty() {
                quoted_name
            } else {
                format!("{parent_path}.{quoted_name}")
            };
            let children = bc_node_children(node);
            let has_children = bc_node_has_children(node)
                || children.as_ref().is_some_and(|value| !value.is_empty());
            let (variables_reference, named_variables) = if has_children {
                let named_variables = children.as_ref().map(Vec::len);
                let reference = handles
                    .create(VariableHandle {
                        frame_id,
                        parent_path: path.clone(),
                        nodes: children,
                    })
                    .unwrap_or(0);
                (reference, named_variables)
            } else {
                (0, None)
            };
            let mut variable = serde_json::json!({
                "name": name,
                "value": value,
                "type": type_name,
                "evaluateName": path,
                "variablesReference": variables_reference,
            });
            if let Some(count) = named_variables {
                variable["namedVariables"] = serde_json::json!(count);
            }
            Some(variable)
        })
        .collect()
}

/// Attempt `DebugAdapterConfigurationDone` if it hasn't already succeeded
/// and BC reports the client has attached. Shared by `handle_configuration_done`
/// (the fast path, attempted the moment the client's request arrives) and
/// the background event-forwarding task (the retry path, polled every cycle
/// until it succeeds) — mirrors the retry the MCP path performs in
/// `NativeDebugSession::drain_events` (native_debug.rs:222): current BC
/// online rejects the RPC until `OnAttachedToConnection` fires, which for
/// break-on-next web-client launches happens only after the browser
/// attaches, i.e. often after the DAP client already sent `configurationDone`
/// once and got rejected.
///
/// Returns `true` if an attempt (successful or not) was made, `false` if
/// skipped (already configured, no config stored yet, or not yet attached) —
/// mainly useful for tests.
async fn try_configuration_done(
    session: &BcDebugSession,
    debug_config: &Mutex<Option<BcDebugConfig>>,
    configured: &Mutex<bool>,
) -> bool {
    if *configured.lock().await {
        return false;
    }
    if !session.is_attached().await {
        return false;
    }
    let Some(cfg) = debug_config.lock().await.clone() else {
        return false;
    };
    match session.configuration_done(&cfg).await {
        Ok(()) => {
            *configured.lock().await = true;
            info!("DAP configurationDone accepted after client attach");
        }
        Err(error) => {
            warn!(%error, "configurationDone rejected; will retry");
        }
    }
    true
}

/// Parse a `setBreakpoints` request's raw `breakpoints` array into
/// `(line, condition)` pairs — the minimal data needed to (re)apply them
/// against a BC session, whether immediately or after queuing.
fn parse_bp_requests(bp_requests: &[serde_json::Value]) -> Vec<BpRequest> {
    bp_requests
        .iter()
        .map(|bp| {
            let line = bp.get("line").and_then(|v| v.as_i64()).unwrap_or(1);
            let condition = bp
                .get("condition")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (line, condition)
        })
        .collect()
}

/// DAP `Breakpoint` results for a source path the workspace index couldn't
/// resolve to an AL object — shared by the live and queued-and-deferred
/// `setBreakpoints` paths.
fn unresolved_object_breakpoints(
    source_path: &str,
    bp_requests: &[BpRequest],
) -> Vec<serde_json::Value> {
    bp_requests
        .iter()
        .map(|(line, _)| {
            serde_json::json!({
                "verified": false, "line": line,
                "message": format!("Could not resolve AL object from workspace index for: {source_path}"),
            })
        })
        .collect()
}

/// Slice a full DAP stack-frame list per the `stackTrace` request's
/// `startFrame`/`levels` arguments and return `(page, totalFrames)`.
///
/// Per the DAP spec: `startFrame` defaults to 0, and `levels` of 0 or absent
/// means "all remaining frames from `startFrame`". `totalFrames` is always
/// the *full* stack length regardless of paging, so a delayed-stack-trace
/// client knows how many more frames it can page in.
fn page_stack_frames(
    frames: Vec<serde_json::Value>,
    arguments: &serde_json::Value,
) -> (Vec<serde_json::Value>, usize) {
    let total = frames.len();
    let start_frame = arguments
        .get("startFrame")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0) as usize;
    let levels = arguments
        .get("levels")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if start_frame >= total {
        return (Vec::new(), total);
    }
    let end = if levels <= 0 {
        total
    } else {
        start_frame.saturating_add(levels as usize).min(total)
    };
    (frames[start_frame..end].to_vec(), total)
}

fn bc_stack_to_dap<P>(frames: serde_json::Value, resolve_path: &P) -> Vec<serde_json::Value>
where
    P: Fn(i32, i32) -> Option<PathBuf>,
{
    let arr = match frames.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .enumerate()
        .map(|(i, frame)| {
            let display_name = frame
                .get("DisplayName")
                .or_else(|| frame.get("displayName"))
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)")
                .to_string();

            // Current BC servers send `StatementSpan.From` instead of
            // `SourcePosition`; prefer it when present (see the fixture at
            // native_debug.rs:905). Both carry 0-based line/column — DAP
            // `stackFrame.line`/`column` are 1-based, so add 1 the same way
            // `parse_bc_stack` (native_debug.rs:527) does. Without the +1
            // Zed highlights one line above the actual stop; without the
            // StatementSpan fallback, servers that only send it report
            // line/column 0.
            let position = frame
                .get("StatementSpan")
                .or_else(|| frame.get("statementSpan"))
                .and_then(|span| span.get("From").or_else(|| span.get("from")))
                .or_else(|| {
                    frame
                        .get("SourcePosition")
                        .or_else(|| frame.get("sourcePosition"))
                });

            let line = position
                .and_then(|sp| sp.get("Line").or_else(|| sp.get("line")))
                .and_then(|v| v.as_i64())
                .map(|v| v.saturating_add(1))
                .unwrap_or(0);

            let col = position
                .and_then(|sp| sp.get("Column").or_else(|| sp.get("column")))
                .and_then(|v| v.as_i64())
                .map(|v| v.saturating_add(1))
                .unwrap_or(0);

            let object_type = frame
                .get("ApplicationObjectId")
                .and_then(|oid| oid.get("ObjectType").or_else(|| oid.get("objectType")))
                .and_then(|v| v.as_i64())
                .map(|v| v as i32);

            let object_number = frame
                .get("ApplicationObjectId")
                .and_then(|oid| oid.get("ObjectNumber").or_else(|| oid.get("objectNumber")))
                .and_then(|v| v.as_i64())
                .map(|v| v as i32);

            let source = match (object_type, object_number) {
                (Some(ot), Some(on)) => {
                    resolve_path(ot, on).map(|p| serde_json::json!({ "path": p.to_string_lossy() }))
                }
                _ => None,
            };

            let mut val = serde_json::json!({
                "id": i as i64,
                "name": display_name,
                "line": line,
                "column": col,
            });
            if let Some(src) = source {
                val["source"] = src;
            }
            val
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_response_includes_both_request_seq_keys() {
        let seq = AtomicU64::new(0);
        let resp = make_response(&seq, 42, "initialize", true, None, None);
        assert_eq!(
            resp.get("request_seq").and_then(|v| v.as_i64()),
            Some(42),
            "response missing snake_case `request_seq`"
        );
        assert_eq!(
            resp.get("requestSeq").and_then(|v| v.as_i64()),
            Some(42),
            "response missing camelCase `requestSeq`"
        );
    }

    #[test]
    fn extract_breakpoint_id_reads_pascal_and_camel_case() {
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "BreakpointId": 99 })),
            Some(99)
        );
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "breakpointId": 88 })),
            Some(88)
        );
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "Id": 42 })),
            Some(42)
        );
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "id": 7 })),
            Some(7)
        );
    }

    #[test]
    fn extract_breakpoint_id_rejects_null_missing_and_zero() {
        // A null response, a payload without an id, and an explicit zero are
        // all unusable handles — recording them would orphan breakpoints on BC
        // and let us falsely report verified: true.
        assert_eq!(extract_breakpoint_id(&serde_json::Value::Null), None);
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "Other": 1 })),
            None
        );
        assert_eq!(extract_breakpoint_id(&serde_json::json!({ "Id": 0 })), None);
        assert_eq!(extract_breakpoint_id(&serde_json::json!({ "id": 0 })), None);
    }

    #[test]
    fn bc_stack_to_dap_includes_source_when_path_resolves() {
        let frames = serde_json::json!([{
            "DisplayName": "MyCodeunit.OnRun",
            "SourcePosition": { "Line": 10, "Column": 4 },
            "ApplicationObjectId": { "ObjectType": 5, "ObjectNumber": 50100 }
        }]);
        let resolve = |ot: i32, on: i32| -> Option<PathBuf> {
            if ot == bc_object_type::CODEUNIT && on == 50100 {
                Some(PathBuf::from("/workspace/src/MyCodeunit.al"))
            } else {
                None
            }
        };
        let result = bc_stack_to_dap(frames, &resolve);
        assert_eq!(result.len(), 1);
        let frame = &result[0];
        assert_eq!(frame["name"], "MyCodeunit.OnRun");
        // BC's SourcePosition is 0-based; DAP stackFrame line/column are 1-based.
        assert_eq!(frame["line"], 11);
        assert_eq!(frame["column"], 5);
        assert_eq!(
            frame["source"]["path"].as_str().unwrap_or(""),
            "/workspace/src/MyCodeunit.al"
        );
    }

    #[test]
    fn bc_stack_to_dap_prefers_statement_span_from_over_source_position() {
        // Current BC servers send StatementSpan.From instead of
        // SourcePosition. When both are present StatementSpan.From wins;
        // when only StatementSpan.From is present it must still be honored
        // (not fall back to the line/column-0 default).
        let frames = serde_json::json!([{
            "DisplayName": "MyCodeunit.OnRun",
            "StatementSpan": { "From": { "Line": 42, "Column": 8 } },
            "SourcePosition": { "Line": 0, "Column": 0 }
        }]);
        let result = bc_stack_to_dap(frames, &|_ot: i32, _on: i32| None::<PathBuf>);
        assert_eq!(result[0]["line"], 43);
        assert_eq!(result[0]["column"], 9);
    }

    #[test]
    fn page_stack_frames_honors_start_frame_and_levels() {
        let frames: Vec<serde_json::Value> =
            (0..10).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) =
            page_stack_frames(frames, &serde_json::json!({ "startFrame": 2, "levels": 3 }));
        assert_eq!(total, 10, "totalFrames must report the full stack size");
        assert_eq!(page.len(), 3);
        assert_eq!(page[0]["id"], 2);
        assert_eq!(page[2]["id"], 4);
    }

    #[test]
    fn page_stack_frames_defaults_to_full_stack_when_arguments_absent() {
        let frames: Vec<serde_json::Value> =
            (0..5).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) = page_stack_frames(frames, &serde_json::json!({}));
        assert_eq!(total, 5);
        assert_eq!(page.len(), 5, "no startFrame/levels means the whole stack");
    }

    #[test]
    fn page_stack_frames_zero_levels_means_all_remaining() {
        let frames: Vec<serde_json::Value> =
            (0..5).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) =
            page_stack_frames(frames, &serde_json::json!({ "startFrame": 3, "levels": 0 }));
        assert_eq!(total, 5);
        assert_eq!(page.len(), 2, "levels=0 means all frames from startFrame");
        assert_eq!(page[0]["id"], 3);
    }

    #[test]
    fn page_stack_frames_start_frame_past_end_yields_empty_page() {
        let frames: Vec<serde_json::Value> =
            (0..3).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) = page_stack_frames(
            frames,
            &serde_json::json!({ "startFrame": 10, "levels": 5 }),
        );
        assert_eq!(total, 3);
        assert!(page.is_empty());
    }

    #[test]
    fn page_stack_frames_levels_beyond_end_clamps_to_total() {
        let frames: Vec<serde_json::Value> =
            (0..3).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) = page_stack_frames(
            frames,
            &serde_json::json!({ "startFrame": 1, "levels": 100 }),
        );
        assert_eq!(total, 3);
        assert_eq!(page.len(), 2);
    }

    #[test]
    fn bc_stack_to_dap_statement_span_only_still_converts_to_one_based() {
        let frames = serde_json::json!([{
            "MethodName": "RunProbe - OnAction",
            "StatementSpan": { "From": { "Line": 42, "Column": 8 } }
        }]);
        let result = bc_stack_to_dap(frames, &|_ot: i32, _on: i32| None::<PathBuf>);
        assert_eq!(result[0]["line"], 43);
        assert_eq!(result[0]["column"], 9);
    }

    #[test]
    fn bc_stack_to_dap_omits_source_when_path_unknown() {
        let frames = serde_json::json!([{
            "DisplayName": "UnknownObject.Trigger",
            "SourcePosition": { "Line": 1, "Column": 0 },
            "ApplicationObjectId": { "ObjectType": 5, "ObjectNumber": 99999 }
        }]);
        let result = bc_stack_to_dap(frames, &|_ot: i32, _on: i32| None::<PathBuf>);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].get("source").is_none(),
            "source should be absent when resolve_path returns None"
        );
    }

    #[test]
    fn bc_stack_to_dap_handles_missing_application_object_id() {
        let frames = serde_json::json!([{
            "DisplayName": "SomeProc",
            "SourcePosition": { "Line": 5, "Column": 0 }
        }]);
        let result = bc_stack_to_dap(frames, &|_ot: i32, _on: i32| None::<PathBuf>);
        assert_eq!(result.len(), 1);
        assert!(result[0].get("source").is_none());
        assert_eq!(result[0]["name"], "SomeProc");
    }

    #[test]
    fn bc_stack_to_dap_returns_empty_for_non_array_input() {
        let result = bc_stack_to_dap(serde_json::json!(null), &|_: i32, _: i32| None::<PathBuf>);
        assert!(result.is_empty(), "non-array input must yield empty vec");
    }

    #[test]
    fn cloud_browser_url_percent_encodes_tenant_and_env() {
        // A tenant/environment containing special characters must be encoded,
        // otherwise the resulting URL is malformed (mirrors the encoding already
        // applied in bc_debug::base_url / debug_hub_url).
        let config = BcDebugConfig {
            environment_type: "Cloud".to_string(),
            tenant: "acme & co".to_string(),
            environment_name: Some("prod/east".to_string()),
            startup_object_id: 22,
            ..Default::default()
        };
        let url = build_debug_browser_url(&config, "abc123", None).expect("valid cloud URL");
        assert!(
            url.contains("acme%20%26%20co"),
            "tenant special chars must be encoded: {url}"
        );
        assert!(
            url.contains("prod%2Feast"),
            "environment special chars must be encoded: {url}"
        );
        assert!(
            !url.contains("acme & co"),
            "raw unencoded tenant must not leak into URL: {url}"
        );
        assert!(
            url.contains("page=22"),
            "startup object id must be present: {url}"
        );
    }

    #[test]
    fn cloud_browser_url_defaults_env_to_sandbox() {
        let config = BcDebugConfig {
            environment_type: "Cloud".to_string(),
            tenant: "tenant1".to_string(),
            environment_name: None,
            ..Default::default()
        };
        let url = build_debug_browser_url(&config, "conn", None).expect("valid cloud URL");
        assert!(
            url.contains("/tenant1/sandbox/?"),
            "missing env should default to sandbox: {url}"
        );
    }

    #[test]
    fn browser_url_honors_every_advertised_startup_object_type_and_company() {
        for (object_type, parameter) in [
            ("Page", "page"),
            ("Table", "table"),
            ("Report", "report"),
            ("Query", "query"),
        ] {
            let config = BcDebugConfig {
                environment_type: "Cloud".to_string(),
                tenant: "tenant".to_string(),
                environment_name: Some("sandbox".to_string()),
                startup_object_type: object_type.to_string(),
                startup_object_id: 42,
                startup_company: Some("CRONUS UK Ltd.".to_string()),
                ..Default::default()
            };
            let url = build_debug_browser_url(&config, "conn", None).expect("valid cloud URL");
            assert!(
                url.contains(&format!("{parameter}=42")),
                "{object_type} must use its own URL parameter: {url}"
            );
            let parsed = url::Url::parse(&url).expect("browser URL parses");
            assert_eq!(
                parsed
                    .query_pairs()
                    .find(|(key, _)| key == "company")
                    .map(|(_, value)| value.into_owned()),
                Some("CRONUS UK Ltd.".to_string()),
                "startup company must survive URL encoding: {url}"
            );
        }
    }

    #[test]
    fn kind_to_object_type_keys_are_real_language_data_keywords() {
        // The bc_object_type::* INTEGERS are Microsoft BC DAP wire constants
        // (legitimately hardcoded). The KEYWORDS that select them are AL language
        // facts — assert each still resolves in language_data, so a future keyword
        // rename that orphans a map entry (live objects then silently → UNKNOWN)
        // fails here instead of in production.
        for kw in [
            "table",
            "report",
            "codeunit",
            "xmlport",
            "page",
            "query",
            "pageextension",
            "tableextension",
            "enum",
            "enumextension",
            "reportextension",
        ] {
            assert_ne!(
                kind_to_object_type(kw),
                bc_object_type::UNKNOWN,
                "{kw} should map to a known BC DAP object type"
            );
            assert!(
                al_syntax::language_data::object_type_by_keyword(kw).is_some(),
                "{kw} must be a real AL object keyword in language_data"
            );
        }
    }

    #[test]
    fn kind_to_object_type_maps_all_known_kinds() {
        // Each AL object kind string from the file index must map to its BC
        // ObjectTypeWrapper integer. A regression here silently sends BC the
        // wrong object type for breakpoints (breakpoints land in the wrong
        // object or are rejected).
        assert_eq!(kind_to_object_type("table"), bc_object_type::TABLE);
        assert_eq!(kind_to_object_type("report"), bc_object_type::REPORT);
        assert_eq!(kind_to_object_type("codeunit"), bc_object_type::CODEUNIT);
        assert_eq!(kind_to_object_type("xmlport"), bc_object_type::XMLPORT);
        assert_eq!(kind_to_object_type("page"), bc_object_type::PAGE);
        assert_eq!(kind_to_object_type("query"), bc_object_type::QUERY);
        assert_eq!(
            kind_to_object_type("pageextension"),
            bc_object_type::PAGE_EXTENSION
        );
        assert_eq!(
            kind_to_object_type("tableextension"),
            bc_object_type::TABLE_EXTENSION
        );
        assert_eq!(kind_to_object_type("enum"), bc_object_type::ENUM);
        assert_eq!(
            kind_to_object_type("enumextension"),
            bc_object_type::ENUM_EXTENSION
        );
        assert_eq!(
            kind_to_object_type("reportextension"),
            bc_object_type::REPORT_EXTENSION
        );
    }

    #[test]
    fn kind_to_object_type_is_case_insensitive() {
        // The file index may surface mixed-case kinds; matching is documented as
        // lowercased so "Codeunit", "CODEUNIT" and "codeunit" must all resolve.
        assert_eq!(kind_to_object_type("Codeunit"), bc_object_type::CODEUNIT);
        assert_eq!(kind_to_object_type("TABLE"), bc_object_type::TABLE);
        assert_eq!(
            kind_to_object_type("PageExtension"),
            bc_object_type::PAGE_EXTENSION
        );
    }

    #[test]
    fn kind_to_object_type_unknown_yields_unknown_sentinel() {
        // An unrecognised kind must map to the UNKNOWN sentinel (-1), never
        // accidentally collide with a valid type.
        assert_eq!(kind_to_object_type(""), bc_object_type::UNKNOWN);
        assert_eq!(kind_to_object_type("controladdin"), bc_object_type::UNKNOWN);
        assert_eq!(kind_to_object_type("not-a-type"), bc_object_type::UNKNOWN);
        assert_eq!(bc_object_type::UNKNOWN, -1);
    }

    #[test]
    fn make_response_failure_carries_message_and_no_body() {
        // A failed DAP response must report success=false and surface the error
        // message; it must NOT carry a body. Zed shows `message` to the user.
        let seq = AtomicU64::new(0);
        let resp = make_response(
            &seq,
            3,
            "launch",
            false,
            None,
            Some("Compilation failed".to_string()),
        );
        assert_eq!(resp.get("success").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            resp.get("message").and_then(|v| v.as_str()),
            Some("Compilation failed")
        );
        assert!(
            resp.get("body").is_none(),
            "failure response must not include a body"
        );
        assert_eq!(resp.get("type").and_then(|v| v.as_str()), Some("response"));
        assert_eq!(resp.get("command").and_then(|v| v.as_str()), Some("launch"));
    }

    #[test]
    fn make_response_success_carries_body_and_no_message() {
        let seq = AtomicU64::new(0);
        let resp = make_response(
            &seq,
            1,
            "threads",
            true,
            Some(serde_json::json!({"threads": []})),
            None,
        );
        assert_eq!(resp.get("success").and_then(|v| v.as_bool()), Some(true));
        assert!(
            resp.get("body").is_some(),
            "success response must have body"
        );
        assert!(
            resp.get("message").is_none(),
            "success response must not carry an error message"
        );
    }

    #[test]
    fn make_response_seq_is_monotonic() {
        // DAP requires non-decreasing seq across all messages. Each call must
        // consume the next value from the shared counter.
        let seq = AtomicU64::new(5);
        let first = make_response(&seq, 0, "a", true, None, None);
        let second = make_response(&seq, 0, "b", true, None, None);
        assert_eq!(first.get("seq").and_then(|v| v.as_u64()), Some(5));
        assert_eq!(second.get("seq").and_then(|v| v.as_u64()), Some(6));
    }

    #[test]
    fn make_event_with_and_without_body() {
        let seq = AtomicU64::new(10);
        let with_body = make_event(
            &seq,
            "stopped",
            Some(serde_json::json!({"reason": "breakpoint"})),
        );
        assert_eq!(
            with_body.get("type").and_then(|v| v.as_str()),
            Some("event")
        );
        assert_eq!(
            with_body.get("event").and_then(|v| v.as_str()),
            Some("stopped")
        );
        assert_eq!(
            with_body
                .get("body")
                .and_then(|b| b.get("reason"))
                .and_then(|v| v.as_str()),
            Some("breakpoint")
        );
        assert_eq!(with_body.get("seq").and_then(|v| v.as_u64()), Some(10));

        let without_body = make_event(&seq, "initialized", None);
        assert!(
            without_body.get("body").is_none(),
            "event with no body must omit the body key"
        );
        assert_eq!(without_body.get("seq").and_then(|v| v.as_u64()), Some(11));
    }

    #[test]
    fn onprem_browser_url_requires_server_provided_public_web_base_url() {
        let config = BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some("http://localhost".to_string()),
            server_instance: None,
            port: 7049,
            startup_object_id: 42,
            ..Default::default()
        };
        let error = build_debug_browser_url(&config, "cid", None)
            .expect_err("developer-services URL must not stand in for the Web client URL");
        assert!(error.to_string().contains("PublicWebBaseUrl"), "{error}");
    }

    #[test]
    fn onprem_browser_url_is_case_insensitive_for_env_type() {
        // environment_type matching uses eq_ignore_ascii_case, so "onprem"
        // (lowercase) must still take the on-prem branch, not the cloud branch.
        let config = BcDebugConfig {
            environment_type: "onprem".to_string(),
            server: Some("http://localhost".to_string()),
            server_instance: Some("BC".to_string()),
            port: 8080,
            startup_object_id: 9,
            ..Default::default()
        };
        let url = build_debug_browser_url(&config, "conn", Some("http://localhost:8080/BC"))
            .expect("server-provided on-prem URL");
        assert!(
            url.contains(":8080/BC?page=9"),
            "lowercase onprem must use the on-prem URL branch: {url}"
        );
        assert!(
            !url.contains("businesscentral.dynamics.com"),
            "must not fall through to the cloud branch: {url}"
        );
    }

    #[test]
    fn onprem_browser_url_uses_authoritative_web_base_not_developer_services_port() {
        let config = BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some("http://localhost".to_string()),
            server_instance: Some("BC".to_string()),
            port: 7049,
            startup_object_id: 5,
            ..Default::default()
        };
        let url =
            build_debug_browser_url(&config, "conn", Some("https://web.example.test:8443/BC"))
                .expect("server-provided on-prem URL");
        assert!(
            url.starts_with("https://web.example.test:8443/BC?page=5"),
            "on-prem URL must use the authoritative Web endpoint: {url}"
        );
        assert!(
            !url.contains(":7049"),
            "developer-services port must not leak into Web client URL: {url}"
        );
    }

    // -----------------------------------------------------------------------
    // bc_stack_to_dap — additional coverage for camelCase variants, frame
    // indexing, and field-default fallbacks. These exercise the real BC payload
    // shapes (newer BC versions serialise camelCase; some frames omit fields).
    // -----------------------------------------------------------------------

    #[test]
    fn bc_stack_to_dap_reads_camelcase_inner_keys() {
        // The mapper reads the *inner* position/object keys with a camelCase
        // fallback (Line→line, Column→column, ObjectType→objectType, etc.).
        // The outer container keys are still PascalCase (SourcePosition,
        // ApplicationObjectId); DisplayName has its own displayName fallback.
        let frames = serde_json::json!([{
            "displayName": "CamelProc",
            "SourcePosition": { "line": 12, "column": 3 },
            "ApplicationObjectId": { "objectType": 5, "objectNumber": 50100 }
        }]);
        let resolve = |ot: i32, on: i32| -> Option<PathBuf> {
            if ot == bc_object_type::CODEUNIT && on == 50100 {
                Some(PathBuf::from("/ws/Cu.al"))
            } else {
                None
            }
        };
        let result = bc_stack_to_dap(frames, &resolve);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "CamelProc");
        assert_eq!(result[0]["line"], 13);
        assert_eq!(result[0]["column"], 4);
        assert_eq!(
            result[0]["source"]["path"].as_str().unwrap_or(""),
            "/ws/Cu.al"
        );
    }

    #[test]
    fn bc_stack_to_dap_assigns_sequential_frame_ids() {
        // DAP stackTrace frame ids must be the array index so scopes/variables
        // can map a frameId back to the BC stack frame. A regression that reused
        // a constant id would break per-frame variable inspection.
        let frames = serde_json::json!([
            { "DisplayName": "Top", "SourcePosition": { "Line": 1, "Column": 0 } },
            { "DisplayName": "Mid", "SourcePosition": { "Line": 2, "Column": 0 } },
            { "DisplayName": "Bottom", "SourcePosition": { "Line": 3, "Column": 0 } },
        ]);
        let result = bc_stack_to_dap(frames, &|_: i32, _: i32| None::<PathBuf>);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0]["id"], 0);
        assert_eq!(result[1]["id"], 1);
        assert_eq!(result[2]["id"], 2);
        assert_eq!(result[0]["name"], "Top");
        assert_eq!(result[2]["name"], "Bottom");
    }

    #[test]
    fn bc_stack_to_dap_defaults_missing_fields() {
        // A frame missing DisplayName / SourcePosition must not panic and must
        // fall back to documented defaults: "(unknown)" name, line 0, column 0.
        let frames = serde_json::json!([{}]);
        let result = bc_stack_to_dap(frames, &|_: i32, _: i32| None::<PathBuf>);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "(unknown)");
        assert_eq!(result[0]["line"], 0);
        assert_eq!(result[0]["column"], 0);
        assert!(result[0].get("source").is_none());
    }

    #[test]
    fn bc_stack_to_dap_empty_array_yields_empty_vec() {
        let result = bc_stack_to_dap(serde_json::json!([]), &|_: i32, _: i32| None::<PathBuf>);
        assert!(result.is_empty());
    }

    #[test]
    fn bc_vars_to_dap_maps_pascal_and_camel_case_to_dap_shape() {
        // BC's PascalCase and camelCase LocalNode keys must both produce DAP
        // `name`/`value`/`type` + `variablesReference`. A non-string value is
        // stringified, not dropped. A node without a name is skipped.
        let bc = serde_json::json!([
            { "Name": "Customer", "Value": "10000", "TypeName": "Record" },
            { "name": "i", "value": 5, "typeName": "Integer" },
            { "Value": "orphan" }
        ]);
        let mut handles = VariableHandleStore::default();
        let vars = bc_vars_to_dap(bc.as_array().expect("fixture array"), 0, "", &mut handles);
        assert_eq!(vars.len(), 2, "nameless node must be skipped");
        assert_eq!(vars[0]["name"], "Customer");
        assert_eq!(vars[0]["value"], "10000");
        assert_eq!(vars[0]["type"], "Record");
        assert_eq!(vars[0]["evaluateName"], "\"Customer\"");
        assert_eq!(vars[0]["variablesReference"], 0);
        assert_eq!(vars[1]["name"], "i");
        assert_eq!(vars[1]["value"], "5", "numeric value stringified");
        assert_eq!(vars[1]["type"], "Integer");
    }

    #[test]
    fn json_array_non_array_yields_empty() {
        assert!(json_array(serde_json::json!(null)).is_empty());
        assert!(json_array(serde_json::json!({ "Name": "x" })).is_empty());
    }

    #[test]
    fn bc_vars_to_dap_retains_inline_and_lazy_child_nodes() {
        let bc = serde_json::json!([
            {
                "Name": "Customer",
                "Summary": "Record Customer",
                "TypeName": "Record Customer",
                "HasChildren": true,
                "Children": [{
                    "Name": "No.",
                    "Summary": "10000",
                    "TypeName": "Code[20]"
                }]
            },
            {
                "name": "Lines",
                "summary": "List of [Record Sales Line]",
                "typeName": "List",
                "hasChildren": true,
                "children": null
            }
        ]);
        let mut handles = VariableHandleStore::default();
        let vars = bc_vars_to_dap(bc.as_array().expect("fixture array"), 7, "", &mut handles);

        assert_eq!(vars[0]["variablesReference"], VARIABLE_HANDLE_BASE);
        assert_eq!(vars[0]["namedVariables"], 1);
        assert_eq!(vars[0]["evaluateName"], "\"Customer\"");
        let customer = handles.get(VARIABLE_HANDLE_BASE).expect("customer handle");
        assert_eq!(customer.frame_id, 7);
        assert_eq!(customer.parent_path, "\"Customer\"");
        assert_eq!(customer.nodes.expect("inline children").len(), 1);

        assert_eq!(vars[1]["variablesReference"], VARIABLE_HANDLE_BASE + 1);
        assert!(vars[1].get("namedVariables").is_none());
        let lines = handles
            .get(VARIABLE_HANDLE_BASE + 1)
            .expect("lazy list handle");
        assert_eq!(lines.parent_path, "\"Lines\"");
        assert!(lines.nodes.is_none(), "null children must expand lazily");
    }

    #[test]
    fn variable_scope_references_match_editorservices_wire_contract() {
        assert_eq!(scope_reference(SCOPE_GLOBALS, 7), Some((1 << 16) | 7));
        assert_eq!(scope_reference(SCOPE_LOCALS, 7), Some((2 << 16) | 7));
        assert_eq!(
            decode_scope_reference((1 << 16) | 7),
            Some((SCOPE_GLOBALS, 7))
        );
        assert_eq!(
            decode_scope_reference((2 << 16) | 7),
            Some((SCOPE_LOCALS, 7))
        );
        assert_eq!(scope_reference(SCOPE_LOCALS, 65_536), None);
        assert_eq!(decode_scope_reference(VARIABLE_HANDLE_BASE), None);
    }

    #[test]
    fn frame_variable_groups_strip_wrappers_without_losing_locals() {
        let roots = serde_json::json!([
            { "Name": "<Globals>", "HasChildren": true, "Children": null },
            { "Name": "<Database Statistics>", "HasChildren": true },
            { "Name": "Customer", "Summary": "10000" },
            { "Name": "Count", "Summary": "2" }
        ]);
        let locals = local_nodes_from_frame_variables(&roots);
        assert_eq!(locals.len(), 2);
        assert_eq!(bc_node_name(&locals[0]), Some("Customer"));
        assert!(
            inline_global_nodes(&roots).is_none(),
            "null advertised children require ExpandGlobals"
        );

        let inline = serde_json::json!([{
            "Name": "<Globals>",
            "HasChildren": true,
            "Children": [{ "Name": "GlobalValue", "Summary": "42" }]
        }]);
        assert_eq!(
            inline_global_nodes(&inline).expect("inline globals").len(),
            1
        );
    }

    #[test]
    fn bc_stack_to_dap_omits_source_when_only_object_type_present() {
        // resolve_path is only consulted when BOTH object_type and object_number
        // are present. A frame with object_type but no object_number must not
        // resolve a source (the (Some, None) match arm yields None).
        let frames = serde_json::json!([{
            "DisplayName": "Partial",
            "SourcePosition": { "Line": 1, "Column": 0 },
            "ApplicationObjectId": { "ObjectType": 5 }
        }]);
        let result = bc_stack_to_dap(frames, &|_: i32, _: i32| None::<PathBuf>);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].get("source").is_none(),
            "source must be absent when object_number is missing"
        );
    }

    #[test]
    fn try_spawn_returns_true_for_spawnable_command() {
        #[cfg(not(target_os = "windows"))]
        let spawned = try_spawn("true", &[]);
        #[cfg(target_os = "windows")]
        let spawned = try_spawn("cmd", &["/c", "exit"]);
        assert!(
            spawned,
            "spawning an existing no-op binary must report success"
        );
    }

    #[test]
    fn try_spawn_returns_false_for_missing_command() {
        // A binary name that cannot exist on PATH must make spawn() fail, and
        // try_spawn must surface that as `false` rather than panicking. This is
        // the branch open_browser relies on to know the opener was unavailable.
        let spawned = try_spawn("al-no-such-binary-xyzzy-1234567890", &["irrelevant"]);
        assert!(
            !spawned,
            "spawning a nonexistent binary must report failure, not panic"
        );
    }

    #[test]
    fn open_browser_does_not_panic_and_returns_bool() {
        // open_browser dispatches to the platform opener via try_spawn. Whether
        // the opener exists is environment-dependent (headless CI may lack
        // xdg-open), so we only assert it completes without panicking and yields
        // a concrete bool. The important invariant — that a failed spawn maps to
        // `false` — is pinned by try_spawn_returns_false_for_missing_command.
        let _: bool = open_browser("https://example.invalid/path?x=1");
    }

    #[tokio::test]
    async fn write_dap_emits_content_length_framed_body() {
        let seq = AtomicU64::new(1);
        let resp = make_response(&seq, 7, "threads", true, None, None);
        let body = serde_json::to_vec(&resp).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        crate::dap::framing::write_dap_frame(&mut buf, &body)
            .await
            .unwrap();

        let text = String::from_utf8(buf).unwrap();
        assert!(
            text.starts_with(&format!("Content-Length: {}\r\n\r\n", body.len())),
            "frame must lead with an accurate Content-Length header: {text:?}"
        );
        assert!(
            text.contains("\"command\":\"threads\""),
            "serialised body must follow the header: {text:?}"
        );
    }
}

#[cfg(test)]
mod handler_tests {
    //! per-request handler tests over `NativeDapState` — no
    //! stdio loop, no BC server. Handlers write DAP frames into a duplex
    //! pipe; tests read them back through the real framing parser.

    use super::*;

    type TokenFut = std::future::Ready<std::result::Result<String, String>>;
    type CompileFut = std::future::Ready<std::result::Result<String, String>>;

    /// The concrete `NativeDapState` specialization used across these tests:
    /// plain `fn` pointers for the token / object / path hooks.
    type TestState = NativeDapState<
        fn(String) -> TokenFut,
        fn(&str) -> Option<ResolvedObject>,
        fn(i32, i32) -> Option<PathBuf>,
        fn(PathBuf) -> CompileFut,
        fn(&Path) -> std::result::Result<Option<PathBuf>, String>,
    >;

    fn no_token(_tenant: String) -> TokenFut {
        std::future::ready(Err("no auth in tests".to_string()))
    }

    fn no_compile(_project_root: PathBuf) -> CompileFut {
        std::future::ready(Err("no compile in handler tests".to_string()))
    }

    fn test_state() -> TestState {
        let (cancel_tx, cancel_rx) = watch::channel(0u64);
        let (dap_event_tx, _dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        // Keep the receiver alive for the state's lifetime in tests that
        // never read events — dropping it would only matter for the
        // forwarder task, which these tests don't spawn.
        std::mem::forget(_dap_event_rx);
        NativeDapState {
            seq: Arc::new(AtomicU64::new(1)),
            session: Arc::new(Mutex::new(None)),
            debug_config: Arc::new(Mutex::new(None)),
            breakpoints: Arc::new(Mutex::new(HashMap::new())),
            pending_breakpoints: Arc::new(Mutex::new(HashMap::new())),
            configured: Arc::new(Mutex::new(false)),
            variable_handles: Arc::new(Mutex::new(VariableHandleStore::default())),
            cancel_tx,
            cancel_rx,
            dap_event_tx,
            project_root: "/nonexistent/test-project".to_string(),
            acquire_token: no_token,
            resolve_object: |_| None,
            resolve_path: |_, _| None,
            compile: no_compile,
            find_app: |_| Ok(None),
        }
    }

    /// Run one request through `handle_request` and return (terminate, frames).
    async fn run_request(
        command: &str,
        arguments: serde_json::Value,
    ) -> (bool, Vec<serde_json::Value>) {
        let state = test_state();
        run_request_on(&state, command, arguments).await
    }

    async fn run_request_on(
        state: &TestState,
        command: &str,
        arguments: serde_json::Value,
    ) -> (bool, Vec<serde_json::Value>) {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let terminate = state
            .handle_request(&mut client, command, 7, &arguments)
            .await
            .expect("handler must not error");
        use tokio::io::AsyncWriteExt;
        client.shutdown().await.expect("shutdown");
        drop(client);
        let mut reader = tokio::io::BufReader::new(server);
        let mut frames: Vec<serde_json::Value> = Vec::new();
        while let Ok(body) = read_dap_body(&mut reader).await {
            frames.push(serde_json::from_slice(&body).expect("valid JSON frame"));
        }
        (terminate, frames)
    }

    #[tokio::test]
    async fn initialize_reports_capabilities_and_initialized_event() {
        let (term, frames) = run_request("initialize", serde_json::json!({})).await;
        assert!(!term);
        assert_eq!(frames.len(), 2, "response + initialized event: {frames:?}");
        assert_eq!(frames[0]["type"], "response");
        assert_eq!(frames[0]["success"], true);
        assert_eq!(
            frames[0]["body"]["supportsConfigurationDoneRequest"], true,
            "capabilities must be advertised"
        );
        for capability in [
            "supportsFunctionBreakpoints",
            "supportsStepBack",
            "supportsSetVariable",
            "supportsRestartFrame",
            "supportsCompletionsRequest",
            "supportsRestartRequest",
        ] {
            assert_eq!(
                frames[0]["body"][capability], false,
                "{capability} must stay false until the native adapter has a real implementation"
            );
        }
        assert_eq!(frames[1]["event"], "initialized");
        // Monotonic seq across both messages.
        assert!(frames[0]["seq"].as_u64() < frames[1]["seq"].as_u64());
    }

    #[tokio::test]
    async fn threads_returns_single_al_thread() {
        let (_, frames) = run_request("threads", serde_json::json!({})).await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["body"]["threads"][0]["id"], 1);
    }

    #[tokio::test]
    async fn pause_fails_with_actionable_message() {
        let (_, frames) = run_request("pause", serde_json::json!({})).await;
        assert_eq!(frames[0]["success"], false);
        assert!(
            frames[0]["message"]
                .as_str()
                .unwrap_or("")
                .contains("breakpoint"),
            "must tell the user the BC alternative: {frames:?}"
        );
    }

    #[tokio::test]
    async fn unsupported_capability_requests_fail_explicitly_without_mutating_state() {
        let state = test_state();
        *state.debug_config.lock().await = Some(BcDebugConfig::from_dap_args(
            &serde_json::json!({"tenant": "state-sentinel"}),
        ));
        state
            .breakpoints
            .lock()
            .await
            .insert("/project/Sentinel.al".to_string(), vec![41]);
        state
            .variable_handles
            .lock()
            .await
            .create(VariableHandle {
                frame_id: 3,
                parent_path: "Sentinel".to_string(),
                nodes: Some(Vec::new()),
            })
            .expect("sentinel variable handle");

        let cases = [
            (
                "setFunctionBreakpoints",
                "function breakpoints are not supported by the BC debug hub; use source breakpoints instead",
            ),
            (
                "setVariable",
                "setVariable is not supported by the BC debug hub; variables can be inspected and evaluated but not mutated",
            ),
            (
                "completions",
                "completions are not supported by the native adapter; BC exposes no completion method and Microsoft's adapter delegates this request to its editor workspace",
            ),
            (
                "restart",
                "restart is not supported by the BC debug hub; disconnect and launch or attach a new debug session",
            ),
            (
                "stepBack",
                "stepBack is not supported by the BC debug hub; only continue, step-over, step-in, and step-out are available",
            ),
        ];

        for (command, expected_message) in cases {
            let (terminate, frames) = run_request_on(&state, command, serde_json::json!({})).await;
            assert!(!terminate, "{command} must not terminate the adapter");
            assert_eq!(frames.len(), 1, "{command}: {frames:?}");
            assert_eq!(frames[0]["type"], "response");
            assert_eq!(frames[0]["request_seq"], 7);
            assert_eq!(frames[0]["requestSeq"], 7);
            assert_eq!(frames[0]["command"], command);
            assert_eq!(frames[0]["success"], false);
            assert_eq!(frames[0]["message"], expected_message);
            assert!(
                frames[0].get("body").is_none(),
                "failed {command} response must not fabricate a body: {frames:?}"
            );

            assert!(state.session.lock().await.is_none());
            assert_eq!(
                state
                    .debug_config
                    .lock()
                    .await
                    .as_ref()
                    .map(|config| config.tenant.as_str()),
                Some("state-sentinel")
            );
            assert_eq!(
                state
                    .breakpoints
                    .lock()
                    .await
                    .get("/project/Sentinel.al")
                    .cloned(),
                Some(vec![41])
            );
            assert_eq!(state.variable_handles.lock().await.handles.len(), 1);
        }
    }

    #[tokio::test]
    async fn unknown_command_is_rejected_not_ignored() {
        let (term, frames) = run_request("bogusCommand", serde_json::json!({})).await;
        assert!(!term);
        assert_eq!(frames[0]["success"], false);
        assert!(frames[0]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Unsupported command: bogusCommand"));
    }

    #[tokio::test]
    async fn variables_without_session_returns_empty_array() {
        let (_, frames) =
            run_request("variables", serde_json::json!({"variablesReference": 101})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["variables"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn set_breakpoints_without_session_reports_unverified() {
        // Breakpoints set before launch/attach (DAP configuration phase)
        // must not be answered as permanently failed — they're queued and
        // will be verified once a session starts (see
        // `set_breakpoints_without_session_are_queued_for_later_verification`).
        let (_, frames) = run_request(
            "setBreakpoints",
            serde_json::json!({
                "source": {"path": "/proj/src/Foo.al"},
                "breakpoints": [{"line": 10}, {"line": 20}],
            }),
        )
        .await;
        let bps = frames[0]["body"]["breakpoints"]
            .as_array()
            .expect("breakpoints array");
        assert_eq!(bps.len(), 2);
        for bp in bps {
            assert_eq!(bp["verified"], false);
            assert!(bp.get("id").is_none(), "no BC id exists yet: {bp:?}");
            assert_eq!(
                bp["message"],
                "Debug session not started yet; breakpoint will be verified once the session starts"
            );
        }
    }

    fn resolve_foo_al(path: &str) -> Option<ResolvedObject> {
        if path == "/proj/src/Foo.al" {
            Some(ResolvedObject {
                object_type: bc_object_type::CODEUNIT,
                object_id: 50100,
            })
        } else {
            None
        }
    }

    #[tokio::test]
    async fn set_breakpoints_without_session_are_queued_and_reverified_on_launch() {
        // Regression for the pre-session setBreakpoints finding: breakpoints
        // set during DAP configuration (before launch/attach) must be
        // queued, not lost, and re-applied/re-verified once a session
        // starts.
        let (cancel_tx, cancel_rx) = watch::channel(0u64);
        let (dap_event_tx, _dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        std::mem::forget(_dap_event_rx);
        let state: TestState = NativeDapState {
            seq: Arc::new(AtomicU64::new(1)),
            session: Arc::new(Mutex::new(None)),
            debug_config: Arc::new(Mutex::new(None)),
            breakpoints: Arc::new(Mutex::new(HashMap::new())),
            pending_breakpoints: Arc::new(Mutex::new(HashMap::new())),
            configured: Arc::new(Mutex::new(false)),
            variable_handles: Arc::new(Mutex::new(VariableHandleStore::default())),
            cancel_tx,
            cancel_rx,
            dap_event_tx,
            project_root: "/proj".to_string(),
            acquire_token: no_token,
            resolve_object: resolve_foo_al,
            resolve_path: |_: i32, _: i32| -> Option<PathBuf> { None },
            compile: no_compile,
            find_app: |_| Ok(None),
        };

        // 1. setBreakpoints arrives before any session exists.
        let (_, frames) = run_request_on(
            &state,
            "setBreakpoints",
            serde_json::json!({
                "source": {"path": "/proj/src/Foo.al"},
                "breakpoints": [{"line": 10}, {"line": 20, "condition": "X > 1"}],
            }),
        )
        .await;
        let bps = frames[0]["body"]["breakpoints"].as_array().unwrap();
        assert_eq!(bps.len(), 2);
        assert!(bps.iter().all(|bp| bp["verified"] == false));
        assert_eq!(
            state
                .pending_breakpoints
                .lock()
                .await
                .get("/proj/src/Foo.al")
                .cloned(),
            Some(vec![(10, String::new()), (20, "X > 1".to_string())]),
            "queued exactly the requested (line, condition) pairs"
        );

        // 2. A session "starts" — drive `apply_pending_breakpoints` directly,
        // exactly as `handle_launch_attach` does right after connect/attach
        // succeed, against a fake BC hub.
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        fake.reply_ok("AddBreakpoint", serde_json::json!({ "Id": 501 }));
        fake.reply_ok("AddBreakpoint", serde_json::json!({ "Id": 502 }));

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        state
            .apply_pending_breakpoints(&session, &mut client)
            .await
            .expect("apply_pending_breakpoints must not error");
        use tokio::io::AsyncWriteExt;
        client.shutdown().await.unwrap();
        drop(client);
        let mut reader = tokio::io::BufReader::new(server);
        let mut events: Vec<serde_json::Value> = Vec::new();
        while let Ok(body) = read_dap_body(&mut reader).await {
            events.push(serde_json::from_slice(&body).expect("valid JSON frame"));
        }

        assert_eq!(
            events.len(),
            2,
            "one breakpoint event per queued breakpoint: {events:?}"
        );
        for event in &events {
            assert_eq!(event["event"], "breakpoint");
            assert_eq!(event["body"]["reason"], "changed");
            assert_eq!(event["body"]["breakpoint"]["verified"], true);
            assert_eq!(
                event["body"]["breakpoint"]["source"]["path"],
                "/proj/src/Foo.al"
            );
        }
        let lines: Vec<i64> = events
            .iter()
            .map(|e| e["body"]["breakpoint"]["line"].as_i64().unwrap())
            .collect();
        assert_eq!(lines, vec![10, 20], "verification events preserve order");

        // The pending queue is drained and the newly-added BC ids are now
        // tracked under `breakpoints` for future replace/remove.
        assert!(state.pending_breakpoints.lock().await.is_empty());
        assert_eq!(
            state
                .breakpoints
                .lock()
                .await
                .get("/proj/src/Foo.al")
                .cloned(),
            Some(vec![501, 502])
        );
    }

    #[tokio::test]
    async fn set_breakpoints_without_session_unresolvable_object_is_still_queued() {
        // A source path the workspace index can't resolve yet (e.g. still
        // indexing) must still queue rather than drop the request — it may
        // resolve by the time the session starts.
        let state = test_state(); // resolve_object always returns None
        let (_, frames) = run_request_on(
            &state,
            "setBreakpoints",
            serde_json::json!({
                "source": {"path": "/proj/src/Unresolvable.al"},
                "breakpoints": [{"line": 5}],
            }),
        )
        .await;
        assert_eq!(frames[0]["body"]["breakpoints"][0]["verified"], false);
        assert_eq!(
            state
                .pending_breakpoints
                .lock()
                .await
                .get("/proj/src/Unresolvable.al")
                .cloned(),
            Some(vec![(5, String::new())])
        );
    }

    #[tokio::test]
    async fn configuration_done_skips_rpc_when_client_not_yet_attached() {
        // Current BC online rejects DebugAdapterConfigurationDone before
        // OnAttachedToConnection fires; the handler must not even attempt
        // the RPC while unattached (it defers to the event-forwarder retry
        // instead of burning a doomed call).
        let state = test_state();
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        *state.session.lock().await = Some(Arc::new(session));
        *state.debug_config.lock().await = Some(BcDebugConfig::default());

        let (_, frames) = run_request_on(&state, "configurationDone", serde_json::json!({})).await;
        assert_eq!(
            frames[0]["success"], true,
            "response always acks: {frames:?}"
        );
        assert!(
            fake.sent_frames()
                .iter()
                .all(|f| f["target"] != "DebugAdapterConfigurationDone"),
            "must not attempt the RPC while BC reports not attached"
        );
        assert!(
            !*state.configured.lock().await,
            "must not mark configured when no attempt was made"
        );
    }

    #[tokio::test]
    async fn configuration_done_succeeds_immediately_when_already_attached() {
        let state = test_state();
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        // Drive OnAttachedToConnection through the session before wrapping
        // it in the state, so is_attached() reads true.
        fake.push_callback("OnAttachedToConnection", serde_json::Value::Null);
        session.try_drain_push_events().await;
        assert!(session.is_attached().await);

        fake.reply_ok("DebugAdapterConfigurationDone", serde_json::json!(null));
        *state.session.lock().await = Some(Arc::new(session));
        *state.debug_config.lock().await = Some(BcDebugConfig::default());

        let (_, frames) = run_request_on(&state, "configurationDone", serde_json::json!({})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(
            fake.sent_frames()
                .iter()
                .filter(|f| f["target"] == "DebugAdapterConfigurationDone")
                .count(),
            1
        );
        assert!(*state.configured.lock().await, "must mark configured");
    }

    #[tokio::test]
    async fn try_configuration_done_retries_after_attach_and_only_configures_once() {
        // Models the background event-forwarder's retry loop directly:
        // first call while unattached does nothing; once BC reports
        // attached, the same helper succeeds; a third call is a no-op
        // because it's already configured (BC must only see one call).
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        let debug_config = Mutex::new(Some(BcDebugConfig::default()));
        let configured = Mutex::new(false);

        let attempted = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(!attempted, "must skip while not attached");
        assert!(!*configured.lock().await);

        fake.push_callback("OnAttachedToConnection", serde_json::Value::Null);
        session.try_drain_push_events().await;
        fake.reply_ok("DebugAdapterConfigurationDone", serde_json::json!(null));

        let attempted = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(attempted, "must attempt once attached");
        assert!(*configured.lock().await);

        // A further call (e.g. the forwarder's next 50ms poll) must not
        // re-invoke BC now that configuration succeeded.
        let attempted_again = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(!attempted_again);
        assert_eq!(
            fake.sent_frames()
                .iter()
                .filter(|f| f["target"] == "DebugAdapterConfigurationDone")
                .count(),
            1,
            "BC must see exactly one DebugAdapterConfigurationDone call"
        );
    }

    #[tokio::test]
    async fn try_configuration_done_keeps_retrying_after_a_rejection() {
        // BC rejects the first attempt (still not really ready) — the next
        // call must retry rather than giving up permanently.
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        let debug_config = Mutex::new(Some(BcDebugConfig::default()));
        let configured = Mutex::new(false);

        fake.push_callback("OnAttachedToConnection", serde_json::Value::Null);
        session.try_drain_push_events().await;

        // `configuration_done` itself retries once with no args if the
        // debug-options form is rejected (older-BC compat); queue a
        // rejection for both attempts so the overall call fails.
        fake.reply_err("DebugAdapterConfigurationDone", "not ready");
        fake.reply_err("DebugAdapterConfigurationDone", "still not ready");
        let attempted = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(attempted);
        assert!(
            !*configured.lock().await,
            "a rejected attempt must not mark configured"
        );

        fake.reply_ok("DebugAdapterConfigurationDone", serde_json::json!(null));
        let attempted = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(attempted);
        assert!(*configured.lock().await, "retry must succeed");
    }

    #[tokio::test]
    async fn steps_without_session_still_acknowledge() {
        for cmd in ["next", "stepIn", "stepOut"] {
            let (_, frames) = run_request(cmd, serde_json::json!({})).await;
            assert_eq!(frames[0]["success"], true, "{cmd} must ack: {frames:?}");
            assert_eq!(frames[0]["command"], cmd);
        }
    }

    #[tokio::test]
    async fn continue_without_session_acknowledges_all_threads() {
        let (_, frames) = run_request("continue", serde_json::json!({})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["allThreadsContinued"], true);
    }

    #[tokio::test]
    async fn stack_trace_without_session_returns_empty_frames() {
        let (_, frames) = run_request("stackTrace", serde_json::json!({"threadId": 1})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["totalFrames"], 0);
    }

    #[tokio::test]
    async fn scopes_without_session_returns_no_scopes() {
        let (_, frames) = run_request("scopes", serde_json::json!({"frameId": 3})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["scopes"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn evaluate_without_session_returns_empty_result() {
        let (_, frames) = run_request(
            "evaluate",
            serde_json::json!({"expression": "Customer.Name", "frameId": 0}),
        )
        .await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["result"], "");
        assert_eq!(frames[0]["body"]["variablesReference"], 0);
    }

    #[tokio::test]
    async fn disconnect_terminates_with_response_then_terminated_event() {
        let (term, frames) = run_request("disconnect", serde_json::json!({})).await;
        assert!(term, "disconnect must signal loop exit");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["type"], "response");
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[1]["event"], "terminated");
    }

    #[tokio::test]
    async fn attach_without_auth_fails_with_message() {
        let (term, frames) = run_request(
            "attach",
            serde_json::json!({"tenant": "test-tenant", "environmentType": "Sandbox"}),
        )
        .await;
        assert!(!term);
        let last = frames.last().expect("at least one frame");
        assert_eq!(last["success"], false, "frames: {frames:?}");
        assert!(last["message"]
            .as_str()
            .unwrap_or("")
            .contains("Authentication failed"));
    }

    #[tokio::test]
    async fn launch_compiles_then_rejects_missing_manifest_selected_artifact() {
        let (cancel_tx, cancel_rx) = watch::channel(0u64);
        let (dap_event_tx, _dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        std::mem::forget(_dap_event_rx);
        let compiled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let compiled_by_hook = Arc::clone(&compiled);
        let state = NativeDapState {
            seq: Arc::new(AtomicU64::new(1)),
            session: Arc::new(Mutex::new(None)),
            debug_config: Arc::new(Mutex::new(None)),
            breakpoints: Arc::new(Mutex::new(HashMap::new())),
            pending_breakpoints: Arc::new(Mutex::new(HashMap::new())),
            configured: Arc::new(Mutex::new(false)),
            variable_handles: Arc::new(Mutex::new(VariableHandleStore::default())),
            cancel_tx,
            cancel_rx,
            dap_event_tx,
            project_root: "/test/project".to_string(),
            acquire_token: |_: String| std::future::ready(Ok("test-token".to_string())),
            resolve_object: |_: &str| -> Option<ResolvedObject> { None },
            resolve_path: |_: i32, _: i32| -> Option<PathBuf> { None },
            compile: move |_: PathBuf| {
                compiled_by_hook.store(true, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(Ok("shared build completed".to_string()))
            },
            // This is the shared manifest-name selector supplied by al-lsp;
            // no match must fail rather than publishing a stale neighbouring app.
            find_app: |_: &Path| -> std::result::Result<Option<PathBuf>, String> { Ok(None) },
        };

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let terminate = state
            .handle_request(
                &mut client,
                "launch",
                7,
                &serde_json::json!({"tenant": "test-tenant", "environmentType": "Sandbox"}),
            )
            .await
            .expect("launch handler");
        use tokio::io::AsyncWriteExt;
        client.shutdown().await.expect("shutdown");
        drop(client);
        let mut reader = tokio::io::BufReader::new(server);
        let mut frames: Vec<serde_json::Value> = Vec::new();
        while let Ok(body) = read_dap_body(&mut reader).await {
            frames.push(serde_json::from_slice(&body).expect("DAP JSON"));
        }
        assert!(!terminate);
        assert!(compiled.load(std::sync::atomic::Ordering::SeqCst));
        let response = frames.last().expect("launch response");
        assert_eq!(response["success"], false, "frames: {frames:?}");
        assert!(response["message"]
            .as_str()
            .unwrap_or("")
            .contains("No compiled .app found"));
        assert!(
            state.session.lock().await.is_none(),
            "must not connect after missing artifact"
        );
    }
}
