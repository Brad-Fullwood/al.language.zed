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

use super::bc_debug::{percent_encode_url, publish_app, BcDebugConfig, BcDebugSession, BcEvent};
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

/// Run the native DAP server on stdio.
///
/// `acquire_token` is a callback to get an OAuth access token for the given tenant.
/// `resolve_object` maps a file path to its AL object type + ID using the workspace index.
/// `resolve_path` is the reverse: given a BC (ObjectType, ObjectNumber) returns the source file.
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
    /// Cancellation channel for the background event-forwarding task.
    /// When a new debug session starts we send a new value so the old task exits.
    cancel_tx: watch::Sender<u64>,
    cancel_rx: watch::Receiver<u64>,
    /// Channel for the BC-event forwarding task to send pre-serialized DAP
    /// event bytes to the main loop (bounded to 1024 messages).
    dap_event_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    project_root: String,
    alc_path: Option<PathBuf>,
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

impl<F, Fut, R, P, C, A> NativeDapState<F, R, P, C, A>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    R: Fn(&str) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(&Path) -> std::result::Result<String, String> + Send + Sync + 'static,
    A: Fn(&Path) -> Option<PathBuf> + Send + Sync + 'static,
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
            "continue" => self.handle_continue(out, request_seq, command).await?,
            "threads" => self.handle_threads(out, request_seq, command).await?,
            "stackTrace" => self.handle_stack_trace(out, request_seq, command).await?,
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
        // Clone both Arc and config before dropping locks so we don't hold
        // the mutex guard across the async invoke() call.
        let session_arc = self.session.lock().await.clone();
        let cfg = self.debug_config.lock().await.clone();
        if let (Some(s), Some(cfg)) = (session_arc, cfg) {
            if let Err(e) = s.configuration_done(&cfg).await {
                warn!("configurationDone: {e}");
            }
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
        // Store config for use in the configurationDone handler.
        *self.debug_config.lock().await = Some(config.clone());

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
            // Native-first: build the deploy `.app` with the pure-Rust native
            // emitter (no `alc`, no C# bridge). A configured toolchain gates the
            // compile step; the emitter itself does not need `alc`.
            if self.alc_path.is_some() {
                let compile_outcome: std::result::Result<String, String> =
                    (self.compile)(std::path::Path::new(&self.project_root));
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

            let app_path = (self.find_app)(Path::new(&self.project_root));
            if let Some(app_path) = app_path {
                let http = reqwest::Client::builder()
                    .danger_accept_invalid_certs(config.accept_invalid_certs)
                    .build()
                    .map_err(|e| DapError::PublishFailed(e.to_string()))?;

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
                *self.session.lock().await = Some(Arc::new(debug_session));

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

                // Open browser with debug context params (must match SignalR ConnectionId)
                if config.launch_browser {
                    let web_url = build_debug_browser_url(&config, &conn_id);

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

        // Clone the Arc<BcDebugSession> while holding the session mutex,
        // then drop the guard immediately so no mutex is held across
        // the async breakpoint operations below (prevents deadlock).
        let session_arc = self.session.lock().await.clone();
        let mut result_bps = Vec::new();

        if let Some(s) = session_arc {
            // Resolve object type and ID from workspace symbol index.
            let resolved = (self.resolve_object)(&source_path);

            if let Some(obj) = resolved {
                let obj_type = obj.object_type;
                let obj_id = obj.object_id;

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
                let old_ids: Vec<i64> = bps.remove(&source_path).unwrap_or_default();
                for id in old_ids {
                    if let Err(e) = s.remove_breakpoint(id).await {
                        tracing::warn!(
                            breakpoint_id = id,
                            error = %e,
                            "DAP setBreakpoints: removing prior breakpoint failed; \
                             local state will be overwritten regardless"
                        );
                    }
                }

                let mut new_ids = Vec::new();
                for bp in &bp_requests {
                    let line = bp.get("line").and_then(|v| v.as_i64()).unwrap_or(1);
                    let server_line = line.saturating_sub(1);
                    let condition = bp.get("condition").and_then(|v| v.as_str()).unwrap_or("");

                    match s
                        .add_breakpoint(obj_type, obj_id, server_line, 0, condition)
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
                bps.insert(source_path.clone(), new_ids);
                drop(bps);
            } else {
                for bp in &bp_requests {
                    let line = bp.get("line").and_then(|v| v.as_i64()).unwrap_or(1);
                    result_bps.push(serde_json::json!({
                                "verified": false, "line": line,
                                "message": format!("Could not resolve AL object from workspace index for: {source_path}"),
                            }));
                }
            }
        } else {
            for bp in &bp_requests {
                let line = bp.get("line").and_then(|v| v.as_i64()).unwrap_or(1);
                result_bps.push(serde_json::json!({
                    "verified": false, "line": line, "message": "No active debug session"
                }));
            }
        }

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
        let total = stack_frames.len();
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({
                    "stackFrames": stack_frames,
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
        // variablesReference is encoded as (frame_id * 100 + scope_index) so the
        // "variables" handler can decode which frame and scope to fetch.
        let frame_id = arguments
            .get("frameId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        // Clone Arc and drop guard before any async work.
        let session_arc = self.session.lock().await.clone();
        let mut scopes = Vec::new();
        if let Some(s) = session_arc {
            // Locals scope (variablesReference = frame_id * 100 + 1)
            // Always include locals — GetVariables returns per-frame locals.
            let locals_ref = frame_id * 100 + 1;
            scopes.push(serde_json::json!({
                "name": "Locals",
                "variablesReference": locals_ref,
                "expensive": false,
            }));

            match s.get_globals(frame_id).await {
                Ok(globals) if !globals.as_array().map(|a| a.is_empty()).unwrap_or(true) => {
                    let globals_ref = frame_id * 100 + 2;
                    let count = globals.as_array().map(|a| a.len()).unwrap_or(0);
                    scopes.push(serde_json::json!({
                        "name": "Globals",
                        "variablesReference": globals_ref,
                        "expensive": true,
                        "namedVariables": count,
                    }));
                }
                Ok(_) => {
                    // Empty globals — still add scope so Zed shows it
                    let globals_ref = frame_id * 100 + 2;
                    scopes.push(serde_json::json!({
                        "name": "Globals",
                        "variablesReference": globals_ref,
                        "expensive": true,
                    }));
                }
                Err(e) => {
                    debug!("get_globals failed for frame {frame_id}: {e}");
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
        // Decode the variablesReference encoding from the "scopes" handler:
        // variablesReference = frame_id * 100 + scope_index
        // scope_index 2 → globals, otherwise → locals
        let frame_id = vars_ref / 100;
        let scope_index = vars_ref % 100;
        // Clone Arc and drop guard before async work.
        let session_arc = self.session.lock().await.clone();
        let variables = if let Some(s) = session_arc {
            if scope_index == 2 {
                match s.get_globals(frame_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            frame_id,
                            error = %e,
                            "DAP variables: get_globals failed; returning empty array"
                        );
                        serde_json::json!([])
                    }
                }
            } else {
                match s.get_variables(frame_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            frame_id,
                            error = %e,
                            "DAP variables: get_variables failed; returning empty array"
                        );
                        serde_json::json!([])
                    }
                }
            }
        } else {
            serde_json::json!([])
        };
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({"variables": bc_vars_to_dap(&variables)})),
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
        let result = if let Some(s) = session_arc {
            s.evaluate(frame_id, expression)
                .await
                .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };
        // LocalNode has value, name, type fields
        let display = result
            .get("value")
            .or(result.get("Value"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                true,
                Some(serde_json::json!({"result": display, "variablesReference": 0})),
                None,
            ),
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
                // All async calls happen without holding the session mutex.
                let mut bc_events = bc_session.try_drain_push_events().await;
                // Also flush pending events buffered during invoke() calls.
                bc_events.extend(bc_session.flush_pending_events().await);

                for bc_event in bc_events {
                    let dap_evt = match &bc_event {
                        BcEvent::Break {
                            reason, thread_id, ..
                        } => make_event(
                            &seq_clone,
                            "stopped",
                            Some(serde_json::json!({
                                "reason": reason,
                                "threadId": thread_id,
                                "allThreadsStopped": true,
                            })),
                        ),
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

pub async fn run_native_dap<F, Fut, R, P, C, A>(
    project_root: &str,
    alc_path: Option<&Path>,
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
    C: Fn(&Path) -> std::result::Result<String, String> + Send + Sync + 'static,
    A: Fn(&Path) -> Option<PathBuf> + Send + Sync + 'static,
{
    let (cancel_tx, cancel_rx) = watch::channel(0u64);
    let (dap_event_tx, mut dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);

    let state = NativeDapState {
        seq: Arc::new(AtomicU64::new(1)),
        session: Arc::new(Mutex::new(None)),
        debug_config: Arc::new(Mutex::new(None)),
        breakpoints: Arc::new(Mutex::new(HashMap::new())),
        cancel_tx,
        cancel_rx,
        dap_event_tx,
        project_root: project_root.to_string(),
        alc_path: alc_path.map(|p| p.to_path_buf()),
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
pub fn build_debug_browser_url(config: &BcDebugConfig, conn_id: &str) -> String {
    if config.environment_type.eq_ignore_ascii_case("OnPrem") {
        // Use onprem_base() to include the port number in the URL.
        let base = config.onprem_base();
        format!(
            "{base}/?page={}&connectioncontext={conn_id}&debuggingcontext={conn_id}&sk={conn_id}",
            config.startup_object_id
        )
    } else {
        let tenant = percent_encode_url(&config.tenant);
        let env = percent_encode_url(config.environment_name.as_deref().unwrap_or("sandbox"));
        format!(
            "https://businesscentral.dynamics.com/{tenant}/{env}/?page={}&noSignUpCheck=1&connectioncontext={conn_id}&debuggingcontext={conn_id}&sk={conn_id}",
            config.startup_object_id
        )
    }
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

/// Converts BC variable nodes to flat DAP variables.
///
/// BC casing varies by server version. Compound values are not expandable, so
/// every returned `variablesReference` is zero.
fn bc_vars_to_dap(variables: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(arr) = variables.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|node| {
            let name = node
                .get("Name")
                .or_else(|| node.get("name"))
                .and_then(|v| v.as_str())?;
            let value = match node.get("Value").or_else(|| node.get("value")) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            let type_name = node
                .get("TypeName")
                .or_else(|| node.get("typeName"))
                .or_else(|| node.get("Type"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(serde_json::json!({
                "name": name,
                "value": value,
                "type": type_name,
                "variablesReference": 0,
            }))
        })
        .collect()
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

            let line = frame
                .get("SourcePosition")
                .and_then(|sp| sp.get("Line").or_else(|| sp.get("line")))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let col = frame
                .get("SourcePosition")
                .and_then(|sp| sp.get("Column").or_else(|| sp.get("column")))
                .and_then(|v| v.as_i64())
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
        assert_eq!(frame["line"], 10);
        assert_eq!(frame["column"], 4);
        assert_eq!(
            frame["source"]["path"].as_str().unwrap_or(""),
            "/workspace/src/MyCodeunit.al"
        );
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
        let url = build_debug_browser_url(&config, "abc123");
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
        let url = build_debug_browser_url(&config, "conn");
        assert!(
            url.contains("/tenant1/sandbox/?"),
            "missing env should default to sandbox: {url}"
        );
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
    fn onprem_browser_url_defaults_instance_when_absent() {
        // When server_instance is None, onprem_base() falls back to "BC". The
        // browser URL must reflect that default so the user lands on a valid
        // dev endpoint rather than a malformed one.
        let config = BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some("http://localhost".to_string()),
            server_instance: None,
            port: 7049,
            startup_object_id: 42,
            ..Default::default()
        };
        let url = build_debug_browser_url(&config, "cid");
        assert!(
            url.contains(":7049/BC/?page=42"),
            "default instance BC and port must appear: {url}"
        );
        assert!(
            url.contains("connectioncontext=cid"),
            "connection id must be threaded into the URL: {url}"
        );
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
        let url = build_debug_browser_url(&config, "conn");
        assert!(
            url.contains(":8080/BC/?page=9"),
            "lowercase onprem must use the on-prem URL branch: {url}"
        );
        assert!(
            !url.contains("businesscentral.dynamics.com"),
            "must not fall through to the cloud branch: {url}"
        );
    }

    #[test]
    fn onprem_browser_url_uses_onprem_base() {
        let config = BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some("http://localhost".to_string()),
            server_instance: Some("BC".to_string()),
            port: 7049,
            startup_object_id: 5,
            ..Default::default()
        };
        let url = build_debug_browser_url(&config, "conn");
        assert!(
            url.contains(":7049/BC/?page=5"),
            "on-prem URL should include port and instance: {url}"
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
        assert_eq!(result[0]["line"], 12);
        assert_eq!(result[0]["column"], 3);
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
        let vars = bc_vars_to_dap(&bc);
        assert_eq!(vars.len(), 2, "nameless node must be skipped");
        assert_eq!(vars[0]["name"], "Customer");
        assert_eq!(vars[0]["value"], "10000");
        assert_eq!(vars[0]["type"], "Record");
        assert_eq!(vars[0]["variablesReference"], 0);
        assert_eq!(vars[1]["name"], "i");
        assert_eq!(vars[1]["value"], "5", "numeric value stringified");
        assert_eq!(vars[1]["type"], "Integer");
    }

    #[test]
    fn bc_vars_to_dap_non_array_yields_empty() {
        assert!(bc_vars_to_dap(&serde_json::json!(null)).is_empty());
        assert!(bc_vars_to_dap(&serde_json::json!({ "Name": "x" })).is_empty());
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

    /// The concrete `NativeDapState` specialization used across these tests:
    /// plain `fn` pointers for the token / object / path hooks.
    type TestState = NativeDapState<
        fn(String) -> TokenFut,
        fn(&str) -> Option<ResolvedObject>,
        fn(i32, i32) -> Option<PathBuf>,
        fn(&Path) -> std::result::Result<String, String>,
        fn(&Path) -> Option<PathBuf>,
    >;

    fn no_token(_tenant: String) -> TokenFut {
        std::future::ready(Err("no auth in tests".to_string()))
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
            cancel_tx,
            cancel_rx,
            dap_event_tx,
            project_root: "/nonexistent/test-project".to_string(),
            alc_path: None,
            acquire_token: no_token,
            resolve_object: |_| None,
            resolve_path: |_, _| None,
            compile: |_| Err("no compile in handler tests".to_string()),
            find_app: |_| None,
        }
    }

    /// Run one request through `handle_request` and return (terminate, frames).
    async fn run_request(
        command: &str,
        arguments: serde_json::Value,
    ) -> (bool, Vec<serde_json::Value>) {
        let state = test_state();
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let terminate = state
            .handle_request(&mut client, command, 7, &arguments)
            .await
            .expect("handler must not error");
        use tokio::io::AsyncWriteExt;
        client.shutdown().await.expect("shutdown");
        drop(client);
        let mut reader = tokio::io::BufReader::new(server);
        let mut frames = Vec::new();
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
            assert_eq!(bp["message"], "No active debug session");
        }
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
}
