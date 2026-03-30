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

use al_core::toolchain::AlToolchain;
use al_dap_client::framing::{read_dap_body, write_dap_frame};
use thiserror::Error;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tracing::{debug, error, info, warn};

pub use editor_services::find_editor_services;

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

    // DAP protocol capture log — writes all messages to a file for reverse-engineering
    let capture_log: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>> =
        std::env::var("AL_DAP_CAPTURE").ok().map(|path| {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("Failed to open DAP capture log");
            std::sync::Arc::new(std::sync::Mutex::new(file))
        });

    // Capture EditorServices stderr to the DAP log if enabled
    if let Some(child_stderr) = child.stderr.take() {
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
                            if let Ok(mut f) = log.lock() {
                                use std::io::Write as _;
                                let _ = write!(f, "### ES-STDERR: {}", line);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let seq_counter = AtomicI64::new(1);
    let toolchain = toolchain.clone();
    let project_root = project_root.to_string();

    let capture_out = capture_log.clone();
    let capture_in = capture_log.clone();

    // Zed → EditorServices.Host (compile on launch, patch config)
    let mut stdin_writer = child_stdin;
    let seq_counter_ref = &seq_counter;
    let stdin_to_child = async {
        let mut reader = BufReader::new(io::stdin());
        let mut stdout_writer = io::stdout();
        loop {
            match read_dap_body(&mut reader).await {
                Ok(body) => {
                    if let Some(ref log) = capture_out {
                        if let Ok(mut f) = log.lock() {
                            use std::io::Write as _;
                            let _ = writeln!(f, ">>> ZED→ES: {}", String::from_utf8_lossy(&body));
                        }
                    }
                    let patched = patch_outgoing(
                        &body,
                        &toolchain,
                        &project_root,
                        &mut stdout_writer,
                        seq_counter_ref,
                    )
                    .await;
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
    let mut stdout_writer = io::stdout();
    let child_to_stdout = async {
        let mut reader = BufReader::new(child_stdout);
        loop {
            match read_dap_body(&mut reader).await {
                Ok(body) => {
                    if let Some(ref log) = capture_in {
                        if let Ok(mut f) = log.lock() {
                            use std::io::Write as _;
                            let _ = writeln!(f, "<<< ES→ZED: {}", String::from_utf8_lossy(&body));
                        }
                    }
                    let patched = patch_incoming(&body, &seq_counter);
                    write_dap_frame(&mut stdout_writer, &patched).await?;
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
        Err(_) => return body.to_vec(),
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
