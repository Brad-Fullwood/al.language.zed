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

use super::bc_debug::{publish_app, BcDebugConfig, BcDebugSession, BcEvent};
use super::framing::{read_dap_body, write_dap_frame};
use super::{DapError, Result};

// ---------------------------------------------------------------------------
// BC ObjectTypeWrapper constants
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// ResolvedObject
// ---------------------------------------------------------------------------

/// Object info resolved from the workspace symbol index.
/// Used to map file paths to BC object types and IDs for breakpoints.
#[derive(Debug, Clone)]
pub struct ResolvedObject {
    /// BC ObjectTypeWrapper value — use `bc_object_type` constants.
    pub object_type: i32,
    /// Object numeric ID
    pub object_id: i32,
}

/// Run the native DAP server on stdio.
///
/// `acquire_token` is a callback to get an OAuth access token for the given tenant.
/// `resolve_object` maps a file path to its AL object type + ID using the workspace index.
/// `resolve_path` is the reverse: given a BC (ObjectType, ObjectNumber) returns the source file.
/// Both are provided by the caller (al-lsp binary) since they depend on `crate::symbols`.
pub async fn run_native_dap<F, Fut, R, P>(
    project_root: &str,
    alc_path: Option<&Path>,
    acquire_token: F,
    resolve_object: R,
    resolve_path: P,
) -> Result<()>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    R: Fn(&str) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
{
    // Single monotonic sequence counter shared between the main loop and the background
    // event-forwarding task. DAP spec requires non-decreasing seq values across all
    // messages (responses, events) sent to the client. Using a single AtomicU64
    // prevents the two previously-separate counters from interleaving non-monotonically.
    let seq: Arc<AtomicU64> = Arc::new(AtomicU64::new(1));
    let session: Arc<Mutex<Option<Arc<BcDebugSession>>>> = Arc::new(Mutex::new(None));
    let debug_config: Arc<Mutex<Option<BcDebugConfig>>> = Arc::new(Mutex::new(None));
    let breakpoints: Arc<Mutex<HashMap<String, Vec<i64>>>> = Arc::new(Mutex::new(HashMap::new()));

    // Cancellation channel for the background event-forwarding task.
    // When a new debug session starts we send a new value so the old task exits.
    let (cancel_tx, cancel_rx) = watch::channel(0u64);

    // Channel for the BC-event forwarding task to send pre-serialized DAP event bytes
    // to the main loop. The main loop drains this channel before processing each
    // incoming DAP message, ensuring BC push events reach Zed promptly.
    //
    // Bounded at 1024 (T010 / spec-concurrency-007): under a misbehaving BC
    // session that fires events faster than Zed drains them, an unbounded
    // channel could grow to GB before any back-pressure. 1024 is generous
    // for realistic debug-event rates (steps, breakpoints, output) and
    // collapses to a try_send + warn-log at the producer end so we never
    // block the SignalR forwarder waiting for stdin to drain.
    let (dap_event_tx, mut dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);

    let mut stdin = BufReader::new(io::stdin());
    let mut stdout = io::stdout();

    loop {
        // Drain any BC push events (e.g. stopped, output) before blocking on stdin.
        while let Ok(frame) = dap_event_rx.try_recv() {
            use super::framing::write_dap_frame;
            if let Err(e) = write_dap_frame(&mut stdout, &frame).await {
                warn!("Failed to write BC event to Zed: {e}");
            }
        }

        // Read DAP message from Zed
        let body = match read_dap_body(&mut stdin).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                error!("Read error: {e}");
                break;
            }
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
            .unwrap_or(serde_json::json!({}));

        debug!("DAP request: {command} (seq={request_seq})");

        match command.as_str() {
            "initialize" => {
                // Respond with our capabilities
                let resp = make_response(
                    &seq,
                    request_seq,
                    &command,
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
                write_dap(&mut stdout, &resp).await?;

                // Send initialized event
                let evt = make_event(&seq, "initialized", None);
                write_dap(&mut stdout, &evt).await?;
            }

            "configurationDone" => {
                // Clone both Arc and config before dropping locks so we don't hold
                // the mutex guard across the async invoke() call.
                let session_arc = session.lock().await.clone();
                let cfg = debug_config.lock().await.clone();
                if let (Some(s), Some(cfg)) = (session_arc, cfg) {
                    if let Err(e) = s.configuration_done(&cfg).await {
                        warn!("configurationDone: {e}");
                    }
                }
                write_dap(
                    &mut stdout,
                    &make_response(&seq, request_seq, &command, true, None, None),
                )
                .await?;
            }

            "launch" | "attach" => {
                let config = BcDebugConfig::from_dap_args(&arguments);
                // Store config for use in the configurationDone handler.
                *debug_config.lock().await = Some(config.clone());

                // Compile if alc is available and this is a launch
                if command == "launch" {
                    // Fix #7: only emit "Compiling" for launch, not attach
                    write_dap(
                        &mut stdout,
                        &make_event(
                            &seq,
                            "output",
                            Some(serde_json::json!({
                                "category": "console",
                                "output": "Compiling AL project...\r\n"
                            })),
                        ),
                    )
                    .await?;
                    if let Some(alc) = alc_path {
                        match compile_project(alc, project_root).await {
                            Ok(output) => {
                                if !output.is_empty() {
                                    write_dap(
                                        &mut stdout,
                                        &make_event(
                                            &seq,
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
                                    &mut stdout,
                                    &make_event(
                                        &seq,
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
                                    &mut stdout,
                                    &make_event(
                                        &seq,
                                        "output",
                                        Some(serde_json::json!({
                                            "category": "stderr",
                                            "output": format!("Compilation failed: {e}\r\n")
                                        })),
                                    ),
                                )
                                .await?;
                                write_dap(
                                    &mut stdout,
                                    &make_response(
                                        &seq,
                                        request_seq,
                                        &command,
                                        false,
                                        None,
                                        Some(format!("Compilation failed: {e}")),
                                    ),
                                )
                                .await?;
                                continue;
                            }
                        }
                    }
                }

                // Acquire token
                write_dap(
                    &mut stdout,
                    &make_event(
                        &seq,
                        "output",
                        Some(serde_json::json!({
                            "category": "console",
                            "output": format!("Authenticating to tenant {}...\r\n", config.tenant)
                        })),
                    ),
                )
                .await?;

                let token = match acquire_token(config.tenant.clone()).await {
                    Ok(t) => t,
                    Err(e) => {
                        write_dap(
                            &mut stdout,
                            &make_response(
                                &seq,
                                request_seq,
                                &command,
                                false,
                                None,
                                Some(format!("Authentication failed: {e}")),
                            ),
                        )
                        .await?;
                        continue;
                    }
                };

                // Publish .app if launching
                if command == "launch" {
                    write_dap(
                        &mut stdout,
                        &make_event(
                            &seq,
                            "output",
                            Some(serde_json::json!({
                                "category": "console",
                                "output": "Publishing package...\r\n"
                            })),
                        ),
                    )
                    .await?;

                    // Find the .app file
                    let app_path = find_app_file(project_root).await;
                    if let Some(app_path) = app_path {
                        let http = reqwest::Client::builder()
                            .danger_accept_invalid_certs(config.accept_invalid_certs)
                            .build()
                            .map_err(|e| DapError::PublishFailed(e.to_string()))?;

                        match publish_app(&http, &config, &token, &app_path).await {
                            Ok(()) => {
                                write_dap(
                                    &mut stdout,
                                    &make_event(
                                        &seq,
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
                                    &mut stdout,
                                    &make_response(
                                        &seq,
                                        request_seq,
                                        &command,
                                        false,
                                        None,
                                        Some(format!("Publish failed: {e}")),
                                    ),
                                )
                                .await?;
                                continue;
                            }
                        }
                    } else {
                        write_dap(
                            &mut stdout,
                            &make_event(
                                &seq,
                                "output",
                                Some(serde_json::json!({
                                    "category": "stderr",
                                    "output": "Warning: No .app file found. Skipping publish.\r\n"
                                })),
                            ),
                        )
                        .await?;
                    }
                }

                // Connect to debug hub
                write_dap(
                    &mut stdout,
                    &make_event(
                        &seq,
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
                        // Attach to debug session
                        if let Err(e) = debug_session.attach(&config).await {
                            write_dap(
                                &mut stdout,
                                &make_response(
                                    &seq,
                                    request_seq,
                                    &command,
                                    false,
                                    None,
                                    Some(format!("Attach failed: {e}")),
                                ),
                            )
                            .await?;
                            continue;
                        }

                        // Capture connection ID before moving session into Arc+mutex
                        let conn_id = debug_session.connection_id.clone();
                        *session.lock().await = Some(Arc::new(debug_session));

                        // Spawn background task to forward BC push events to Zed.
                        //
                        // BC sends Break events via SignalR push at any time (not just in
                        // response to our invocations). This task polls `try_drain_push_events()`
                        // which uses try_lock() on event_rx — if an invoke() is running it skips,
                        // knowing the event will be captured in pending_events and forwarded after
                        // the invoke returns via flush_pending_events().
                        //
                        // Cancellation: send a new value on cancel_tx before spawning a new task
                        // so the old task exits cleanly on reconnect (prevents task leaks).
                        {
                            // Notify any previously spawned task to exit, then give the new task
                            // its own receiver starting from the current generation.
                            let generation = *cancel_tx.borrow() + 1;
                            let _ = cancel_tx.send(generation);
                            let mut cancel_rx_clone = cancel_rx.clone();
                            let session_clone = session.clone();
                            let event_tx_clone = dap_event_tx.clone();
                            let seq_clone = seq.clone();
                            tokio::spawn(async move {
                                // Snapshot the generation we were spawned in.
                                // If cancel_rx_clone sees a newer value, the task exits.
                                let my_generation = generation;
                                loop {
                                    // Check for cancellation (non-blocking).
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
                                            BcEvent::Break { reason, thread_id } => make_event(
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
                                        // rather than block this task forever (T010).
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

                                    // Wait for cancellation or next poll interval.
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

                        write_dap(
                            &mut stdout,
                            &make_event(
                                &seq,
                                "output",
                                Some(serde_json::json!({
                                    "category": "console",
                                    "output": "Debug session started.\r\n"
                                })),
                            ),
                        )
                        .await?;

                        write_dap(
                            &mut stdout,
                            &make_response(&seq, request_seq, &command, true, None, None),
                        )
                        .await?;

                        // Open browser with debug context params (must match SignalR ConnectionId)
                        if config.launch_browser {
                            let web_url = if config.environment_type.eq_ignore_ascii_case("OnPrem")
                            {
                                // Use onprem_base() to include the port number in the URL.
                                let base = config.onprem_base();
                                format!("{base}/?page={}&connectioncontext={conn_id}&debuggingcontext={conn_id}&sk={conn_id}",
                                    config.startup_object_id)
                            } else {
                                let env = config.environment_name.as_deref().unwrap_or("sandbox");
                                format!("https://businesscentral.dynamics.com/{}/{env}?page={}&noSignUpCheck=1&connectioncontext={conn_id}&debuggingcontext={conn_id}&sk={conn_id}",
                                    config.tenant, config.startup_object_id)
                            };

                            // Send the URL as an event for Zed to handle
                            write_dap(
                                &mut stdout,
                                &make_event(
                                    &seq,
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
                            &mut stdout,
                            &make_response(
                                &seq,
                                request_seq,
                                &command,
                                false,
                                None,
                                Some(format!("Debug hub connection failed: {e}")),
                            ),
                        )
                        .await?;
                    }
                }
            }

            "setBreakpoints" => {
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
                let session_arc = session.lock().await.clone();
                let mut result_bps = Vec::new();

                if let Some(s) = session_arc {
                    // Resolve object type and ID from workspace symbol index.
                    let resolved = resolve_object(&source_path);

                    if let Some(obj) = resolved {
                        let obj_type = obj.object_type;
                        let obj_id = obj.object_id;

                        // Collect old breakpoint IDs under the breakpoints lock, then
                        // drop the lock before awaiting (remove_breakpoint is async).
                        let old_ids: Vec<i64> = {
                            let mut bps = breakpoints.lock().await;
                            bps.remove(&source_path).unwrap_or_default()
                        };
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
                            let condition =
                                bp.get("condition").and_then(|v| v.as_str()).unwrap_or("");

                            match s.add_breakpoint(obj_type, obj_id, line, 0, condition).await {
                                Ok(result) => {
                                    let bp_id = result
                                        .get("Id")
                                        .or(result.get("id"))
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0);
                                    new_ids.push(bp_id);
                                    result_bps.push(serde_json::json!({
                                        "id": bp_id,
                                        "verified": true,
                                        "line": line,
                                    }));
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
                        breakpoints
                            .lock()
                            .await
                            .insert(source_path.clone(), new_ids);
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
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        true,
                        Some(serde_json::json!({"breakpoints": result_bps})),
                        None,
                    ),
                )
                .await?;
            }

            "next" => {
                // Step over: BC BreakpointExitReason = 1 via SetBreakpointResponse.
                let session_arc = session.lock().await.clone();
                if let Some(s) = session_arc {
                    if let Err(e) = s.step_over().await {
                        write_dap(
                            &mut stdout,
                            &make_response(
                                &seq,
                                request_seq,
                                &command,
                                false,
                                None,
                                Some(e.to_string()),
                            ),
                        )
                        .await?;
                        continue;
                    }
                }
                write_dap(
                    &mut stdout,
                    &make_response(&seq, request_seq, &command, true, None, None),
                )
                .await?;
            }

            "stepIn" => {
                // Step into: BC BreakpointExitReason = 2 via SetBreakpointResponse.
                let session_arc = session.lock().await.clone();
                if let Some(s) = session_arc {
                    if let Err(e) = s.step_in().await {
                        write_dap(
                            &mut stdout,
                            &make_response(
                                &seq,
                                request_seq,
                                &command,
                                false,
                                None,
                                Some(e.to_string()),
                            ),
                        )
                        .await?;
                        continue;
                    }
                }
                write_dap(
                    &mut stdout,
                    &make_response(&seq, request_seq, &command, true, None, None),
                )
                .await?;
            }

            "stepOut" => {
                // Step out: BC BreakpointExitReason = 3 via SetBreakpointResponse.
                let session_arc = session.lock().await.clone();
                if let Some(s) = session_arc {
                    if let Err(e) = s.step_out().await {
                        write_dap(
                            &mut stdout,
                            &make_response(
                                &seq,
                                request_seq,
                                &command,
                                false,
                                None,
                                Some(e.to_string()),
                            ),
                        )
                        .await?;
                        continue;
                    }
                }
                write_dap(
                    &mut stdout,
                    &make_response(&seq, request_seq, &command, true, None, None),
                )
                .await?;
            }

            "pause" => {
                // BC's SignalR debug hub does not expose a "pause while running" method.
                // The BC debugger only pauses at breakpoints or on error; there is no
                // equivalent of a SIGSTOP that the client can trigger mid-execution.
                // Respond with failure so Zed shows the user a clear error instead of
                // silently doing nothing.
                write_dap(
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        false,
                        None,
                        Some(
                            "pause is not supported by the BC debug hub; set a breakpoint instead"
                                .to_string(),
                        ),
                    ),
                )
                .await?;
            }

            "continue" => {
                let session_arc = session.lock().await.clone();
                if let Some(s) = session_arc {
                    // BC expects BreakpointExitReason integer 0 (continue)
                    if let Err(e) = s.continue_execution(serde_json::json!(0)).await {
                        write_dap(
                            &mut stdout,
                            &make_response(
                                &seq,
                                request_seq,
                                &command,
                                false,
                                None,
                                Some(e.to_string()),
                            ),
                        )
                        .await?;
                        continue;
                    }
                }
                write_dap(
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        true,
                        Some(serde_json::json!({"allThreadsContinued": true})),
                        None,
                    ),
                )
                .await?;
            }

            "threads" => {
                write_dap(
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        true,
                        Some(serde_json::json!({"threads": [{"id": 1, "name": "AL Thread"}]})),
                        None,
                    ),
                )
                .await?;
            }

            "stackTrace" => {
                // Fix #5: call get_call_stack() and map BC StackFrame[] to DAP StackFrames.
                let session_arc = session.lock().await.clone();
                let stack_frames = if let Some(s) = session_arc {
                    match s.get_call_stack().await {
                        Ok(frames) => bc_stack_to_dap(frames, &resolve_path),
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
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        true,
                        Some(serde_json::json!({
                            "stackFrames": stack_frames,
                            "totalFrames": total,
                        })),
                        None,
                    ),
                )
                .await?;
            }

            "scopes" => {
                // Fix #6: use result of get_globals() to build proper scope entries.
                // variablesReference is encoded as (frame_id * 100 + scope_index) so the
                // "variables" handler can decode which frame and scope to fetch.
                let frame_id = arguments
                    .get("frameId")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                // Clone Arc and drop guard before any async work (T-023).
                let session_arc = session.lock().await.clone();
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

                    // Globals scope — only add if get_globals succeeds and returns data
                    match s.get_globals(frame_id).await {
                        Ok(globals)
                            if !globals.as_array().map(|a| a.is_empty()).unwrap_or(true) =>
                        {
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
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        true,
                        Some(serde_json::json!({ "scopes": scopes })),
                        None,
                    ),
                )
                .await?;
            }

            "variables" => {
                let vars_ref = arguments
                    .get("variablesReference")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                // Decode the variablesReference encoding from the "scopes" handler:
                // variablesReference = frame_id * 100 + scope_index
                // scope_index 2 → globals, otherwise → locals
                let frame_id = vars_ref / 100;
                let scope_index = vars_ref % 100;
                // Clone Arc and drop guard before async work (T-023).
                let session_arc = session.lock().await.clone();
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
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        true,
                        Some(serde_json::json!({"variables": variables})),
                        None,
                    ),
                )
                .await?;
            }

            "evaluate" => {
                let expression = arguments
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let frame_id = arguments
                    .get("frameId")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                // Clone Arc and drop guard before async work (T-023).
                let session_arc = session.lock().await.clone();
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
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        true,
                        Some(serde_json::json!({"result": display, "variablesReference": 0})),
                        None,
                    ),
                )
                .await?;
            }

            "disconnect" | "terminate" => {
                // Clone Arc, drop guard, then stop (T-023: don't hold mutex across await).
                let session_arc = session.lock().await.clone();
                if let Some(s) = session_arc {
                    let _ = s.stop_debugging().await;
                }
                *session.lock().await = None;
                write_dap(
                    &mut stdout,
                    &make_response(&seq, request_seq, &command, true, None, None),
                )
                .await?;

                // Send terminated event
                write_dap(&mut stdout, &make_event(&seq, "terminated", None)).await?;
                break;
            }

            _ => {
                // Unknown command — respond with error
                write_dap(
                    &mut stdout,
                    &make_response(
                        &seq,
                        request_seq,
                        &command,
                        false,
                        None,
                        Some(format!("Unsupported command: {command}")),
                    ),
                )
                .await?;
            }
        }
    }

    info!("Native DAP server shutting down");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// Serialize `msg` to JSON and write a DAP frame to `writer`.
///
/// Delegates to [`framing::write_dap_frame`] after serialization.
async fn write_dap(
    writer: &mut io::Stdout,
    msg: &serde_json::Value,
) -> std::result::Result<(), std::io::Error> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_dap_frame(writer, &body).await
}

/// Compile via `dotnet alc` and return raw output.
///
/// This is a DAP-local version of compilation. It returns raw output as a string
/// rather than structured diagnostics because the DAP path streams output to the
/// client as console events. al-core has a richer `compile_project` with diagnostics
/// and analyzer support, but crate::dap cannot import al-core (boundary rule).
async fn compile_project(alc: &Path, project_root: &str) -> std::result::Result<String, DapError> {
    let project_path = Path::new(project_root);
    if !project_path.join("app.json").is_file() {
        return Err(DapError::CompilationFailed(format!(
            "No app.json found in {project_root}"
        )));
    }

    let mut cmd = tokio::process::Command::new("dotnet");
    cmd.arg(alc.display().to_string());
    cmd.arg(format!("/project:{project_root}"));

    let packages_dir = project_path.join(".alpackages");
    if packages_dir.is_dir() {
        cmd.arg(format!("/packagecachepath:{}", packages_dir.display()));
    }

    cmd.stderr(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| DapError::CompilationFailed(format!("Failed to run alc: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    if output.status.success() {
        Ok(combined)
    } else {
        Err(DapError::CompilationFailed(combined))
    }
}

async fn find_app_file(project_root: &str) -> Option<std::path::PathBuf> {
    let root = Path::new(project_root);
    // Look for .app files in the project root
    let mut entries = tokio::fs::read_dir(root).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("app") {
            return Some(path);
        }
    }
    None
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

/// Convert a BC `GetStackTrace` result (array of StackFrame objects) to DAP StackFrame objects.
///
/// BC StackFrame fields (from EditorServices.Protocol.dll reverse-engineering):
///   - `ApplicationObjectId.ObjectType` / `ApplicationObjectId.ObjectNumber` — BC object ref
///   - `SourcePosition.Line` / `SourcePosition.Column` — source location
///   - `DisplayName` — human-readable frame name (procedure name, trigger name, etc.)
///
/// Convert a BC `GetStackTrace` result (array of StackFrame objects) to DAP StackFrame objects.
///
/// `resolve_path` maps a BC (ObjectType integer, ObjectNumber) to a workspace source file path.
/// When a match is found the DAP `source` object is populated so Zed can navigate to the frame.
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

    /// DAP responses must carry both the spec-mandated `request_seq` (snake_case)
    /// and the `requestSeq` (camelCase) variant some clients accept. Without the
    /// camelCase key, clients that only look for `requestSeq` cannot correlate
    /// any response back to its request.
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
}
