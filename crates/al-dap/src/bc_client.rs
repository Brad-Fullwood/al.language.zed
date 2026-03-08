//! Business Central server communication bridge.
//!
//! Spawns and communicates with the AlDap .NET process, which handles
//! actual BC server interaction using Microsoft's deployment DLLs.
//! Communication uses line-delimited JSON-RPC over stdin/stdout.

use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use al_discovery::AlToolchain;

use crate::protocol::{
    AlLaunchConfig, Breakpoint, BridgeRequest, BridgeResponse, Scope, SourceBreakpoint, StackFrame,
    Thread, Variable,
};
use crate::DapError;

/// Bridge to the .NET AlDap subprocess.
///
/// The .NET process handles BC server communication. We talk to it
/// via line-delimited JSON-RPC on stdin/stdout.
pub struct BcBridge {
    child: Mutex<Child>,
    stdin: Mutex<BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
}

impl BcBridge {
    /// Spawn the AlDap .NET bridge process.
    ///
    /// Requires a working `dotnet` installation and the AlDap project.
    /// The .NET process receives the ALTool directory as its first argument.
    pub async fn spawn(toolchain: &AlToolchain) -> Result<Self, DapError> {
        let bridge_exe = find_bridge_exe(toolchain)?;

        let mut child = tokio::process::Command::new(&bridge_exe.program)
            .args(&bridge_exe.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| DapError::BridgeSpawnFailed(format!("Failed to spawn AlDap bridge: {e}")))?;

        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| DapError::BridgeSpawnFailed("No stdin on child process".into()))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| DapError::BridgeSpawnFailed("No stdout on child process".into()))?;

        let bridge = Self {
            child: Mutex::new(child),
            stdin: Mutex::new(BufWriter::new(child_stdin)),
            stdout: Mutex::new(BufReader::new(child_stdout)),
            next_id: AtomicU64::new(1),
        };

        // Verify the bridge is alive
        bridge.ping().await?;

        Ok(bridge)
    }

    /// Send a ping to verify the bridge is alive.
    async fn ping(&self) -> Result<(), DapError> {
        let resp = self.call("ping", None).await?;
        if resp.get("status").and_then(|v| v.as_str()) == Some("ok") {
            Ok(())
        } else {
            Err(DapError::BridgeSpawnFailed(
                "Bridge ping did not return ok".into(),
            ))
        }
    }

    /// Connect to a Business Central server.
    pub async fn connect(&self, config: &AlLaunchConfig) -> Result<(), DapError> {
        let params = serde_json::to_value(config)
            .map_err(|e| DapError::ProtocolError(format!("Failed to serialize config: {e}")))?;
        let resp = self.call("connect", Some(params)).await?;
        check_error(&resp)?;
        Ok(())
    }

    /// Set breakpoints for a source file.
    pub async fn set_breakpoints(
        &self,
        file: &str,
        breakpoints: Vec<SourceBreakpoint>,
    ) -> Result<Vec<Breakpoint>, DapError> {
        let params = serde_json::json!({
            "file": file,
            "breakpoints": breakpoints,
        });
        let resp = self.call("setBreakpoints", Some(params)).await?;
        check_error(&resp)?;

        let breakpoints: Vec<Breakpoint> = resp
            .get("breakpoints")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();
        Ok(breakpoints)
    }

    /// Continue execution of a thread.
    pub async fn continue_execution(&self, thread_id: u64) -> Result<(), DapError> {
        let resp = self
            .call("continue", Some(serde_json::json!({ "threadId": thread_id })))
            .await?;
        check_error(&resp)
    }

    /// Step into the next statement.
    pub async fn step_in(&self, thread_id: u64) -> Result<(), DapError> {
        let resp = self
            .call("stepIn", Some(serde_json::json!({ "threadId": thread_id })))
            .await?;
        check_error(&resp)
    }

    /// Step out of the current function.
    pub async fn step_out(&self, thread_id: u64) -> Result<(), DapError> {
        let resp = self
            .call("stepOut", Some(serde_json::json!({ "threadId": thread_id })))
            .await?;
        check_error(&resp)
    }

    /// Step over to the next statement.
    pub async fn step_over(&self, thread_id: u64) -> Result<(), DapError> {
        let resp = self
            .call("stepOver", Some(serde_json::json!({ "threadId": thread_id })))
            .await?;
        check_error(&resp)
    }

    /// Get the list of active threads.
    pub async fn threads(&self) -> Result<Vec<Thread>, DapError> {
        let resp = self.call("threads", None).await?;
        check_error(&resp)?;
        let threads: Vec<Thread> = resp
            .get("threads")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();
        Ok(threads)
    }

    /// Get the stack trace for a thread.
    pub async fn stack_trace(&self, thread_id: u64) -> Result<Vec<StackFrame>, DapError> {
        let resp = self
            .call(
                "stackTrace",
                Some(serde_json::json!({ "threadId": thread_id })),
            )
            .await?;
        check_error(&resp)?;
        let frames: Vec<StackFrame> = resp
            .get("stackFrames")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();
        Ok(frames)
    }

    /// Get the scopes for a stack frame.
    pub async fn scopes(&self, frame_id: u64) -> Result<Vec<Scope>, DapError> {
        let resp = self
            .call("scopes", Some(serde_json::json!({ "frameId": frame_id })))
            .await?;
        check_error(&resp)?;
        let scopes: Vec<Scope> = resp
            .get("scopes")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();
        Ok(scopes)
    }

    /// Get the variables for a variables reference.
    pub async fn variables(&self, reference: u64) -> Result<Vec<Variable>, DapError> {
        let resp = self
            .call(
                "variables",
                Some(serde_json::json!({ "variablesReference": reference })),
            )
            .await?;
        check_error(&resp)?;
        let variables: Vec<Variable> = resp
            .get("variables")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();
        Ok(variables)
    }

    /// Evaluate an expression in the context of a stack frame.
    pub async fn evaluate(
        &self,
        expression: &str,
        frame_id: Option<u64>,
    ) -> Result<String, DapError> {
        let mut params = serde_json::json!({ "expression": expression });
        if let Some(fid) = frame_id {
            params["frameId"] = serde_json::Value::Number(fid.into());
        }
        let resp = self.call("evaluate", Some(params)).await?;
        check_error(&resp)?;
        Ok(resp
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Disconnect from the BC server and shut down the bridge.
    pub async fn disconnect(&self) -> Result<(), DapError> {
        // Send disconnect command (best-effort)
        let _ = self.call("disconnect", None).await;

        // Kill the child process
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal: JSON-RPC communication
    // -----------------------------------------------------------------------

    /// Send a JSON-RPC request and wait for the response.
    async fn call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, DapError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let request = BridgeRequest {
            id,
            method: method.to_string(),
            params,
        };

        let request_json = serde_json::to_string(&request)
            .map_err(|e| DapError::ProtocolError(format!("Failed to serialize request: {e}")))?;

        // Write request as a single line
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(request_json.as_bytes())
                .await
                .map_err(|_| DapError::BridgeDied)?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|_| DapError::BridgeDied)?;
            stdin.flush().await.map_err(|_| DapError::BridgeDied)?;
        }

        // Read response line
        let mut line = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            let bytes_read = stdout
                .read_line(&mut line)
                .await
                .map_err(|_| DapError::BridgeDied)?;
            if bytes_read == 0 {
                return Err(DapError::BridgeDied);
            }
        }

        let response: BridgeResponse = serde_json::from_str(line.trim()).map_err(|e| {
            DapError::ProtocolError(format!("Invalid bridge response: {e} (line: {line})"))
        })?;

        if response.id != id {
            return Err(DapError::ProtocolError(format!(
                "Response ID mismatch: expected {id}, got {}",
                response.id
            )));
        }

        Ok(response.result)
    }
}

/// Check a bridge response for an error field.
fn check_error(value: &serde_json::Value) -> Result<(), DapError> {
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        Err(DapError::ConnectionFailed(err.to_string()))
    } else {
        Ok(())
    }
}

/// Information needed to launch the bridge process.
struct BridgeExe {
    program: String,
    args: Vec<String>,
}

/// Find the bridge executable. Looks for a prebuilt AlDap binary next to
/// the crate, or falls back to `dotnet run`.
fn find_bridge_exe(toolchain: &AlToolchain) -> Result<BridgeExe, DapError> {
    let altool_dir = toolchain.dotnet_root.display().to_string();

    // Strategy 1: Look for a compiled AlDap executable next to the al-dap crate.
    // In production, AlDap.dll would be published alongside the extension.
    // Check for AlDap.dll in a few common locations:
    let candidate_dirs = [
        // Next to the running binary
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf())),
        // AL_DAP_BRIDGE_PATH env var
        std::env::var("AL_DAP_BRIDGE_PATH")
            .ok()
            .map(std::path::PathBuf::from),
    ];

    for dir in candidate_dirs.iter().flatten() {
        let dll = dir.join("AlDap.dll");
        if dll.is_file() {
            return Ok(BridgeExe {
                program: "dotnet".to_string(),
                args: vec![dll.display().to_string(), altool_dir],
            });
        }
        // Check for a native binary
        let exe = dir.join("AlDap");
        if exe.is_file() {
            return Ok(BridgeExe {
                program: exe.display().to_string(),
                args: vec![altool_dir],
            });
        }
    }

    // Strategy 2: Fall back to `dotnet run` with the project directory.
    // This is used during development.
    let project_dir = find_dotnet_project_dir()?;
    Ok(BridgeExe {
        program: "dotnet".to_string(),
        args: vec![
            "run".to_string(),
            "--project".to_string(),
            project_dir.display().to_string(),
            "--".to_string(),
            altool_dir,
        ],
    })
}

/// Find the AlDap .NET project directory relative to the crate source.
fn find_dotnet_project_dir() -> Result<std::path::PathBuf, DapError> {
    // Try the manifest dir (works when running from the repo)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let project_dir = std::path::Path::new(manifest_dir).join("dotnet/AlDap");
    if project_dir.join("AlDap.csproj").is_file() {
        return Ok(project_dir);
    }

    Err(DapError::BridgeSpawnFailed(
        "Could not find AlDap .NET project directory. Set AL_DAP_BRIDGE_PATH to the directory containing AlDap.dll".to_string(),
    ))
}
