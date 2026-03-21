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

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::collections::HashMap;

use tokio::io::{self, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::bc_debug::{BcDebugConfig, BcDebugSession, publish_app};
use crate::{DapError, Result};

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
/// Both are provided by the caller (al-lsp) since they depend on al-core/al-symbols.
pub async fn run_native_dap<F, Fut, R>(
    project_root: &str,
    alc_path: Option<&Path>,
    acquire_token: F,
    resolve_object: R,
) -> Result<()>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    R: Fn(&str) -> Option<ResolvedObject> + Send + Sync + 'static,
{
    let seq = AtomicI64::new(1);
    let session: Arc<Mutex<Option<BcDebugSession>>> = Arc::new(Mutex::new(None));
    let breakpoints: Arc<Mutex<HashMap<String, Vec<i64>>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut stdin = BufReader::new(io::stdin());
    let mut stdout = io::stdout();

    loop {
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

        let command = msg.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let request_seq = msg.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
        let arguments = msg.get("arguments").cloned().unwrap_or(serde_json::json!({}));

        debug!("DAP request: {command} (seq={request_seq})");

        match command.as_str() {
            "initialize" => {
                // Respond with our capabilities
                let resp = make_response(&seq, request_seq, &command, true, Some(serde_json::json!({
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
                })), None);
                write_dap(&mut stdout, &resp).await?;

                // Send initialized event
                let evt = make_event(&seq, "initialized", None);
                write_dap(&mut stdout, &evt).await?;
            }

            "configurationDone" => {
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true, None, None)).await?;
            }

            "launch" | "attach" => {
                let config = BcDebugConfig::from_dap_args(&arguments);

                // Send output: compiling
                write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                    "category": "console",
                    "output": "Compiling AL project...\r\n"
                })))).await?;

                // Compile if alc is available and this is a launch
                if command == "launch" {
                    if let Some(alc) = alc_path {
                        match compile_project(alc, project_root).await {
                            Ok(output) => {
                                if !output.is_empty() {
                                    write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                                        "category": "console",
                                        "output": format!("{output}\r\n")
                                    })))).await?;
                                }
                                write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                                    "category": "console",
                                    "output": "Compilation succeeded.\r\n"
                                })))).await?;
                            }
                            Err(e) => {
                                write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                                    "category": "stderr",
                                    "output": format!("Compilation failed: {e}\r\n")
                                })))).await?;
                                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                                    Some(format!("Compilation failed: {e}")))).await?;
                                continue;
                            }
                        }
                    }
                }

                // Acquire token
                write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                    "category": "console",
                    "output": format!("Authenticating to tenant {}...\r\n", config.tenant)
                })))).await?;

                let token = match acquire_token(config.tenant.clone()).await {
                    Ok(t) => t,
                    Err(e) => {
                        write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                            Some(format!("Authentication failed: {e}")))).await?;
                        continue;
                    }
                };

                // Publish .app if launching
                if command == "launch" {
                    write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                        "category": "console",
                        "output": "Publishing package...\r\n"
                    })))).await?;

                    // Find the .app file
                    let app_path = find_app_file(project_root).await;
                    if let Some(app_path) = app_path {
                        let http = reqwest::Client::builder()
                            .danger_accept_invalid_certs(config.accept_invalid_certs)
                            .build()
                            .map_err(|e| DapError::PublishFailed(e.to_string()))?;

                        match publish_app(&http, &config, &token, &app_path).await {
                            Ok(()) => {
                                write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                                    "category": "console",
                                    "output": "Package published successfully.\r\n"
                                })))).await?;
                            }
                            Err(e) => {
                                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                                    Some(format!("Publish failed: {e}")))).await?;
                                continue;
                            }
                        }
                    } else {
                        write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                            "category": "stderr",
                            "output": "Warning: No .app file found. Skipping publish.\r\n"
                        })))).await?;
                    }
                }

                // Connect to debug hub
                write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                    "category": "console",
                    "output": "Connecting to debug hub...\r\n"
                })))).await?;

                match BcDebugSession::connect(&config, &token).await {
                    Ok(debug_session) => {
                        // Attach to debug session
                        if let Err(e) = debug_session.attach(&config).await {
                            write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                                Some(format!("Attach failed: {e}")))).await?;
                            continue;
                        }

                        debug_session.configuration_done(&config).await.ok();

                        // Capture connection ID before moving session into mutex
                        let conn_id = debug_session.connection_id.clone();
                        *session.lock().await = Some(debug_session);

                        write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                            "category": "console",
                            "output": "Debug session started.\r\n"
                        })))).await?;

                        write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true, None, None)).await?;

                        // Open browser with debug context params (must match SignalR ConnectionId)
                        if config.launch_browser {
                            let web_url = if config.environment_type.eq_ignore_ascii_case("OnPrem") {
                                let server = config.server.as_deref().unwrap_or("http://localhost");
                                let instance = config.server_instance.as_deref().unwrap_or("BC");
                                format!("{server}/{instance}/?page={}&connectioncontext={conn_id}&debuggingcontext={conn_id}&sk={conn_id}",
                                    config.startup_object_id)
                            } else {
                                let env = config.environment_name.as_deref().unwrap_or("sandbox");
                                format!("https://businesscentral.dynamics.com/{}/{env}?page={}&noSignUpCheck=1&connectioncontext={conn_id}&debuggingcontext={conn_id}&sk={conn_id}",
                                    config.tenant, config.startup_object_id)
                            };

                            // Send the URL as an event for Zed to handle
                            write_dap(&mut stdout, &make_event(&seq, "al/openUri", Some(serde_json::json!({
                                "uri": web_url
                            })))).await?;

                            let _ = open_browser(&web_url);
                        }
                    }
                    Err(e) => {
                        write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                            Some(format!("Debug hub connection failed: {e}")))).await?;
                    }
                }
            }

            "setBreakpoints" => {
                let source_path = arguments.get("source").and_then(|s| s.get("path")).and_then(|v| v.as_str()).unwrap_or("");
                let bp_requests = arguments.get("breakpoints").and_then(|v| v.as_array()).cloned().unwrap_or_default();

                let guard = session.lock().await;
                let mut result_bps = Vec::new();

                if let Some(ref s) = *guard {
                    // Resolve object type and ID from workspace symbol index
                    let resolved = resolve_object(source_path);

                    if let Some(obj) = resolved {
                        let obj_type = obj.object_type;
                        let obj_id = obj.object_id;
                        // Remove old breakpoints for this file
                        let mut bps = breakpoints.lock().await;
                        if let Some(old_ids) = bps.remove(source_path) {
                            for id in old_ids {
                                let _ = s.remove_breakpoint(id).await;
                            }
                        }

                        let mut new_ids = Vec::new();
                        for bp in &bp_requests {
                            let line = bp.get("line").and_then(|v| v.as_i64()).unwrap_or(1);
                            let condition = bp.get("condition").and_then(|v| v.as_str()).unwrap_or("");

                            match s.add_breakpoint(obj_type, obj_id, line, 0, condition).await {
                                Ok(result) => {
                                    let bp_id = result.get("Id").or(result.get("id"))
                                        .and_then(|v| v.as_i64()).unwrap_or(0);
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
                        bps.insert(source_path.to_string(), new_ids);
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

                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true,
                    Some(serde_json::json!({"breakpoints": result_bps})), None)).await?;
            }

            "next" => {
                // Step over — not a direct hub method; stepping is done by BC internally
                // when you respond to a Break with appropriate options
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true, None, None)).await?;
            }

            "stepIn" | "stepOut" | "pause" => {
                // BC handles stepping through the Break/SetBreakpointResponse cycle.
                // These are acknowledged but the actual stepping is managed by the hub.
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true, None, None)).await?;
            }

            "continue" => {
                let guard = session.lock().await;
                if let Some(ref s) = *guard {
                    // BC uses SetBreakpointResponse to continue; pass empty response for now
                    let _ = s.continue_execution(serde_json::json!({})).await;
                }
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true,
                    Some(serde_json::json!({"allThreadsContinued": true})), None)).await?;
            }

            "threads" => {
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true,
                    Some(serde_json::json!({"threads": [{"id": 1, "name": "AL Thread"}]})), None)).await?;
            }

            "stackTrace" => {
                // Simplified stack trace — full implementation needs GetStackTrace from SignalR
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true,
                    Some(serde_json::json!({
                        "stackFrames": [],
                        "totalFrames": 0,
                    })), None)).await?;
            }

            "scopes" => {
                let frame_id = arguments.get("frameId").and_then(|v| v.as_i64()).unwrap_or(0);
                let guard = session.lock().await;
                if let Some(ref s) = *guard {
                    let _ = s.get_globals(frame_id).await;
                }
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true,
                    Some(serde_json::json!({
                        "scopes": [
                            {"name": "Globals", "variablesReference": 1, "expensive": false},
                            {"name": "Locals", "variablesReference": 2, "expensive": false},
                        ]
                    })), None)).await?;
            }

            "variables" => {
                let vars_ref = arguments.get("variablesReference").and_then(|v| v.as_i64()).unwrap_or(0);
                let guard = session.lock().await;
                let variables = if let Some(ref s) = *guard {
                    match s.get_variables(vars_ref).await {
                        Ok(v) => v,
                        Err(_) => serde_json::json!([]),
                    }
                } else {
                    serde_json::json!([])
                };
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true,
                    Some(serde_json::json!({"variables": variables})), None)).await?;
            }

            "evaluate" => {
                let expression = arguments.get("expression").and_then(|v| v.as_str()).unwrap_or("");
                let frame_id = arguments.get("frameId").and_then(|v| v.as_i64()).unwrap_or(0);
                let guard = session.lock().await;
                let result = if let Some(ref s) = *guard {
                    s.evaluate(frame_id, expression).await.unwrap_or(serde_json::Value::Null)
                } else {
                    serde_json::Value::Null
                };
                // LocalNode has value, name, type fields
                let display = result.get("value").or(result.get("Value"))
                    .and_then(|v| v.as_str()).unwrap_or("").to_string();
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true,
                    Some(serde_json::json!({"result": display, "variablesReference": 0})), None)).await?;
            }

            "disconnect" | "terminate" => {
                let mut guard = session.lock().await;
                if let Some(ref s) = *guard {
                    let _ = s.stop_debugging().await;
                }
                *guard = None;
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, true, None, None)).await?;

                // Send terminated event
                write_dap(&mut stdout, &make_event(&seq, "terminated", None)).await?;
                break;
            }

            _ => {
                // Unknown command — respond with error
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                    Some(format!("Unsupported command: {command}")))).await?;
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
    seq: &AtomicI64,
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

fn make_event(
    seq: &AtomicI64,
    event: &str,
    body: Option<serde_json::Value>,
) -> serde_json::Value {
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

async fn write_dap(
    writer: &mut io::Stdout,
    msg: &serde_json::Value,
) -> std::result::Result<(), std::io::Error> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

async fn read_dap_body(
    reader: &mut BufReader<io::Stdin>,
) -> std::result::Result<Vec<u8>, std::io::Error> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "EOF"));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(val.trim().parse::<usize>()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?);
        }
    }
    let length = content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing Content-Length")
    })?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Ok(body)
}

/// Compile via `dotnet alc` and return raw output.
///
/// This is a DAP-local version of compilation. It returns raw output as a string
/// rather than structured diagnostics because the DAP path streams output to the
/// client as console events. al-core has a richer `compile_project` with diagnostics
/// and analyzer support, but al-dap-client cannot import al-core (boundary rule).
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

    let output = cmd.output().await
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
    let result = {
        #[cfg(target_os = "linux")]
        { try_spawn("xdg-open", &[url]) }
        #[cfg(target_os = "macos")]
        { try_spawn("open", &[url]) }
        #[cfg(target_os = "windows")]
        { try_spawn("cmd", &["/c", "start", url]) }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        { false }
    };

    if !result {
        // Platform opener failed or unknown — try common fallbacks
        for opener in &["xdg-open", "open", "firefox", "chromium", "chromium-browser", "google-chrome"] {
            if try_spawn(opener, &[url]) {
                return true;
            }
        }
        warn!("open_browser: all openers failed for URL: {url}");
        return false;
    }

    result
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
