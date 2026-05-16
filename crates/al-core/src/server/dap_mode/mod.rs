//! Debug Adapter Protocol proxy for AL/Business Central.
//!
//! Instead of implementing DAP ourselves, we proxy to Microsoft's
//! `EditorServices.Host` binary which already speaks DAP over stdio.
//!
//! The proxy patches messages in both directions:
//! - Zed → EditorServices: compile project on `launch`, transform config values
//! - EditorServices → Zed: inject missing `seq` field (MS omits this required field)
//!
//! Architecture:
//! ```text
//! Zed ──DAP/stdio──► al-lsp --dap ──stdio──► EditorServices.Host /startDebugging
//! ```
//!
//! Merged from the standalone `al-dap` crate (T401).

mod editor_services;

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::dap::framing::{read_dap_body, write_dap_frame};
use crate::toolchain::AlToolchain;
use thiserror::Error;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tracing::{debug, error, info, warn};

pub use editor_services::find_editor_services;

/// Redact known credential / secret fields from a DAP message body before
/// writing to the `AL_DAP_CAPTURE` log. The DAP `launch` request carries
/// `arguments` like `password`, `accessToken`, `apiKey`, `clientSecret` —
/// fields that should never end up in a developer's debug log file.
///
/// Parses the body as JSON, walks the tree, replaces the string value of
/// any field whose lowercased name appears in `SENSITIVE_FIELDS` with
/// `<redacted>`. If the body doesn't parse as JSON, returns it via lossy
/// UTF-8 unchanged — capture is opt-in via env var, and an unparseable
/// body in the capture log is no worse than the pre-fix behaviour.
///
/// Defence-in-depth only. Stops a developer's accidentally-shared log
/// file from leaking credentials.
fn redact_dap_body_for_log(body: &[u8]) -> String {
    /// Field names (lowercased) whose string value should be replaced.
    const SENSITIVE_FIELDS: &[&str] = &[
        "password",
        "accesstoken",
        "access_token",
        "refreshtoken",
        "refresh_token",
        "token",
        "apikey",
        "api_key",
        "clientsecret",
        "client_secret",
        "bearer",
        "authorization",
        "secret",
    ];

    fn walk(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    if SENSITIVE_FIELDS.contains(&key.to_lowercase().as_str()) {
                        if let serde_json::Value::String(s) = v {
                            // Preserve "empty value" — there's nothing to hide.
                            if !s.is_empty() {
                                *s = "<redacted>".to_string();
                            }
                        }
                    } else {
                        walk(v);
                    }
                }
            }
            serde_json::Value::Array(arr) => {
                for v in arr.iter_mut() {
                    walk(v);
                }
            }
            _ => {}
        }
    }

    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(mut v) => {
            walk(&mut v);
            serde_json::to_string(&v).unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned())
        }
        // Non-JSON body — pass through. Capture log is best-effort.
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    }
}

#[derive(Debug, Error)]
pub enum DapError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("EditorServices.Host not found: {0}")]
    EditorServicesNotFound(String),

    #[error("EditorServices.Host failed to start: {0}")]
    SpawnFailed(String),

    #[error("AL compilation failed: {0}")]
    CompilationFailed(String),
}

/// Run the DAP proxy: spawn EditorServices.Host and pipe stdio bidirectionally.
pub async fn run_dap_server(toolchain: &AlToolchain) -> Result<(), DapError> {
    let project_root = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    run_dap_proxy(toolchain, &project_root).await
}

/// Spawn EditorServices.Host in DAP mode and proxy stdin/stdout,
/// patching messages for compatibility in both directions.
/// Run the legacy EditorServices.Host DAP proxy.
///
/// **Cancellation note** (F-OPEN-041). Unlike the native DAP backend
/// (`crate::dap::native_dap`), this proxy does NOT implement the DAP
/// `cancel` request. Cancellation is delegated to EditorServices.Host
/// itself; if the BC server takes a long time to honour a Step / Continue
/// the user can't cancel from Zed via DAP. The proxy DOES however kill
/// the subprocess cleanly on `disconnect` / shutdown, so a stuck session
/// gets torn down at the OS level. If the legacy proxy ever needs
/// in-flight cancellation, route it through the existing watch channel
/// the native backend already uses (`cancel_rx` in `native_dap.rs`).
pub async fn run_dap_proxy(toolchain: &AlToolchain, project_root: &str) -> Result<(), DapError> {
    let host_path = find_editor_services(toolchain)?;

    info!("Starting EditorServices.Host DAP proxy");
    info!("  Binary: {}", host_path.display());
    info!("  Project root: {project_root}");

    let mut args = vec!["/startDebugging".to_string()];
    if !project_root.is_empty() {
        args.push(format!("/projectRoot:{project_root}"));
    }

    let mut child = tokio::process::Command::new(&host_path)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(if std::env::var("AL_DAP_CAPTURE").is_ok() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::inherit()
        })
        .spawn()
        .map_err(|e| DapError::SpawnFailed(format!("{e}")))?;

    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| DapError::SpawnFailed("child stdin not available".to_string()))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| DapError::SpawnFailed("child stdout not available".to_string()))?;

    // DAP protocol capture log — writes all messages to a file for reverse-engineering.
    // If the file cannot be opened, log a warning and disable capture rather than panicking.
    let capture_log: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>> =
        std::env::var("AL_DAP_CAPTURE").ok().and_then(|path| {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(file) => Some(std::sync::Arc::new(std::sync::Mutex::new(file))),
                Err(e) => {
                    warn!(
                        "AL_DAP_CAPTURE set but could not open '{}': {} — capture disabled",
                        path, e
                    );
                    None
                }
            }
        });

    // Capture EditorServices stderr to the DAP log if enabled.
    // Retain the JoinHandle so we can abort the task on shutdown — letting
    // the spawned task outlive the proxy session leaks resources and may
    // continue writing to a now-closed log file.
    let stderr_task: Option<tokio::task::JoinHandle<()>> =
        child.stderr.take().map(|child_stderr| {
            let capture_stderr = capture_log.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(child_stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            eprint!("{}", line); // Also print to our stderr
                            if let Some(ref log) = capture_stderr {
                                let mut f = log.lock().unwrap_or_else(|e| e.into_inner());
                                use std::io::Write as _;
                                let _ = write!(f, "### ES-STDERR: {}", line);
                            }
                        }
                        Err(e) => {
                            warn!("ES stderr read error: {e}");
                            break;
                        }
                    }
                }
            })
        });

    let seq_counter = AtomicI64::new(1);
    let toolchain = toolchain.clone();
    let project_root = project_root.to_string();

    let capture_out = capture_log.clone();
    let capture_in = capture_log.clone();

    // F-012: both directions need to write Zed-bound DAP frames — the
    // stdin_to_child branch fabricates Zed-bound output events during
    // compile / patch_outgoing, while child_to_stdout forwards real
    // EditorServices.Host frames. Without coordination they share the
    // underlying stdout fd and interleave bytes, corrupting frame headers
    // and bodies. Wrap stdout in an async-aware Mutex and have every write
    // path lock it for the duration of one frame's worth of work.
    let stdout_writer: std::sync::Arc<tokio::sync::Mutex<io::Stdout>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(io::stdout()));

    // Zed → EditorServices.Host (compile on launch, patch config)
    let mut stdin_writer = child_stdin;
    let seq_counter_ref = &seq_counter;
    let stdout_writer_out = stdout_writer.clone();
    let stdin_to_child = async {
        let mut reader = BufReader::new(io::stdin());
        loop {
            match read_dap_body(&mut reader).await {
                Ok(body) => {
                    if let Some(ref log) = capture_out {
                        if let Ok(mut f) = log.lock() {
                            use std::io::Write as _;
                            let _ = writeln!(f, ">>> ZED→ES: {}", redact_dap_body_for_log(&body));
                        }
                    }
                    let patched = {
                        let mut guard = stdout_writer_out.lock().await;
                        let writer: &mut io::Stdout = &mut guard;
                        patch_outgoing(&body, &toolchain, &project_root, writer, seq_counter_ref)
                            .await
                    };
                    write_dap_frame(&mut stdin_writer, &patched).await?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        drop(stdin_writer);
        Ok::<(), std::io::Error>(())
    };

    // EditorServices.Host → Zed (patch missing `seq` field)
    let stdout_writer_in = stdout_writer.clone();
    let child_to_stdout = async {
        let mut reader = BufReader::new(child_stdout);
        loop {
            match read_dap_body(&mut reader).await {
                Ok(body) => {
                    if let Some(ref log) = capture_in {
                        if let Ok(mut f) = log.lock() {
                            use std::io::Write as _;
                            let _ = writeln!(f, "<<< ES→ZED: {}", redact_dap_body_for_log(&body));
                        }
                    }
                    let patched = patch_incoming(&body, &seq_counter);
                    let mut guard = stdout_writer_in.lock().await;
                    let writer: &mut io::Stdout = &mut guard;
                    write_dap_frame(writer, &patched).await?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        Ok::<(), std::io::Error>(())
    };

    tokio::select! {
        result = stdin_to_child => {
            if let Err(e) = result {
                error!("stdin→child pipe error: {e}");
            }
            info!("DAP client disconnected");
        }
        result = child_to_stdout => {
            if let Err(e) = result {
                error!("child→stdout pipe error: {e}");
            }
            info!("EditorServices.Host exited");
        }
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
    if let Some(task) = stderr_task {
        task.abort();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Message patching
// ---------------------------------------------------------------------------

async fn patch_outgoing(
    body: &[u8],
    toolchain: &AlToolchain,
    project_root: &str,
    output_writer: &mut io::Stdout,
    seq_counter: &AtomicI64,
) -> Vec<u8> {
    let mut msg: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                "patch_outgoing: cannot parse DAP body (len={} bytes), passing through raw: {e}",
                body.len()
            );
            return body.to_vec();
        }
    };

    let command = msg.get("command").and_then(|v| v.as_str()).unwrap_or("");

    if command == "launch" {
        let _ = send_output_event(output_writer, seq_counter, "Compiling AL project...\r\n").await;

        match compile_project(toolchain, project_root).await {
            Ok(output) => {
                if !output.is_empty() {
                    let _ = send_output_event(output_writer, seq_counter, &output).await;
                }
                let _ = send_output_event(output_writer, seq_counter, "Compilation succeeded.\r\n")
                    .await;
            }
            Err(DapError::CompilationFailed(msg)) => {
                let _ = send_output_event(
                    output_writer,
                    seq_counter,
                    &format!("Compilation failed: {msg}\r\n"),
                )
                .await;
            }
            Err(e) => {
                warn!("AL compilation failed: {e}");
                let _ =
                    send_output_event(output_writer, seq_counter, &format!("Error: {e}\r\n")).await;
            }
        }
    }

    if command == "launch" || command == "attach" {
        if let Some(args) = msg.get_mut("arguments").and_then(|v| v.as_object_mut()) {
            patch_launch_args(args);
        }
    }

    serde_json::to_vec(&msg).unwrap_or_else(|_| body.to_vec())
}

async fn send_output_event(
    writer: &mut io::Stdout,
    seq_counter: &AtomicI64,
    text: &str,
) -> Result<(), std::io::Error> {
    let seq = seq_counter.fetch_add(1, Ordering::Relaxed);
    let event = serde_json::json!({
        "seq": seq,
        "type": "event",
        "event": "output",
        "body": {
            "category": "console",
            "output": text
        }
    });
    let body = serde_json::to_vec(&event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_dap_frame(writer, &body).await
}

fn patch_launch_args(args: &mut serde_json::Map<String, serde_json::Value>) {
    if let Some(val) = args.get("breakOnError").cloned() {
        if let Some(s) = val.as_str() {
            let patched = !s.eq_ignore_ascii_case("none");
            debug!("Patched breakOnError: {s:?} → {patched}");
            args.insert("breakOnError".to_string(), serde_json::Value::Bool(patched));
        }
    }

    if let Some(val) = args.get("breakOnRecordWrite").cloned() {
        if let Some(s) = val.as_str() {
            let patched = !s.eq_ignore_ascii_case("none");
            debug!("Patched breakOnRecordWrite: {s:?} → {patched}");
            args.insert(
                "breakOnRecordWrite".to_string(),
                serde_json::Value::Bool(patched),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// AL compilation
// ---------------------------------------------------------------------------

async fn compile_project(toolchain: &AlToolchain, project_root: &str) -> Result<String, DapError> {
    let project_path = Path::new(project_root);
    if !project_path.join("app.json").is_file() {
        return Err(DapError::CompilationFailed(format!(
            "No app.json found in {project_root}"
        )));
    }

    let alc = &toolchain.alc;
    info!("Compiling AL project: {project_root}");

    let mut cmd = tokio::process::Command::new("dotnet");
    cmd.arg(alc.display().to_string());
    cmd.arg(format!("/project:{project_root}"));
    // Don't pass /out: — alc defaults to the project directory with auto-generated .app name

    let packages_dir = project_path.join(".alpackages");
    if packages_dir.is_dir() {
        cmd.arg(format!("/packagecachepath:{}", packages_dir.display()));
    }

    let mut analyzer_paths = Vec::new();
    for analyzer in [
        &toolchain.analyzers.code_cop,
        &toolchain.analyzers.app_source_cop,
        &toolchain.analyzers.ui_cop,
        &toolchain.analyzers.per_tenant_cop,
    ] {
        if analyzer.is_file() {
            analyzer_paths.push(analyzer.display().to_string());
        }
    }
    if !analyzer_paths.is_empty() {
        cmd.arg(format!("/analyzer:{}", analyzer_paths.join(",")));
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
        info!("AL compilation succeeded");
        Ok(combined)
    } else {
        Err(DapError::CompilationFailed(combined))
    }
}

/// Patch incoming messages from EditorServices.Host before forwarding to Zed.
///
/// EditorServices.Host sends non-standard DAP messages:
/// - Missing `seq` field (EditorServices.Host often omits this required field)
/// - Null values for required string fields like `command`, `event`, `message`
///   which Zed's DAP deserializer expects as non-null strings
fn patch_incoming(body: &[u8], counter: &AtomicI64) -> Vec<u8> {
    let mut msg: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return body.to_vec(),
    };

    let obj = match msg.as_object_mut() {
        Some(o) => o,
        None => return body.to_vec(),
    };

    // Inject seq if missing (EditorServices.Host often omits it)
    if !obj.contains_key("seq") {
        obj.insert(
            "seq".to_string(),
            serde_json::Value::Number(counter.fetch_add(1, Ordering::Relaxed).into()),
        );
    }

    // Patch null string fields that Zed requires to be non-null
    for field in &["command", "event", "message", "type"] {
        if let Some(val) = obj.get(*field) {
            if val.is_null() {
                obj.insert(field.to_string(), serde_json::Value::String(String::new()));
                debug!("Patched null {field} → empty string in DAP message");
            }
        }
    }

    serde_json::to_vec(&msg).unwrap_or_else(|_| body.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- redact_dap_body_for_log (F-OPEN-(dap-audit-1)) ---------------------

    #[test]
    fn redacts_password_in_launch_arguments() {
        let body =
            br#"{"type":"request","command":"launch","arguments":{"password":"supersecret"}}"#;
        let out = redact_dap_body_for_log(body);
        assert!(!out.contains("supersecret"), "got: {out}");
        assert!(out.contains("\"password\":\"<redacted>\""), "got: {out}");
    }

    #[test]
    fn redacts_access_token_field_variants() {
        // Both camelCase and snake_case, both compact and with space.
        let body = br#"{"accessToken":"AAA","access_token": "BBB"}"#;
        let out = redact_dap_body_for_log(body);
        assert!(!out.contains("AAA"));
        assert!(!out.contains("BBB"));
    }

    #[test]
    fn redactor_preserves_non_sensitive_fields() {
        // Positive: a field like "name" must NOT be redacted.
        let body = br#"{"name":"keep me","password":"drop me"}"#;
        let out = redact_dap_body_for_log(body);
        assert!(out.contains("\"name\":\"keep me\""));
        assert!(!out.contains("drop me"));
    }

    #[test]
    fn redactor_handles_empty_value() {
        let body = br#"{"password":""}"#;
        let out = redact_dap_body_for_log(body);
        // Empty value stays empty (nothing between the quotes to redact).
        assert!(out.contains("\"password\":\"\""), "got: {out}");
    }

    #[test]
    fn redactor_is_case_insensitive_on_field_name() {
        let body = br#"{"Password":"foo","BEARER":"bar"}"#;
        let out = redact_dap_body_for_log(body);
        assert!(!out.contains("foo"));
        assert!(!out.contains("bar"));
    }

    #[test]
    fn redactor_handles_multiple_occurrences_of_same_field() {
        let body = br#"{"password":"first","other":{"password":"second"}}"#;
        let out = redact_dap_body_for_log(body);
        assert!(!out.contains("first"));
        assert!(!out.contains("second"));
    }
}
