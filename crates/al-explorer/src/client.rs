//! Daemon client — connects to al-lsp daemon via Unix socket.
//!
//! Computes the deterministic socket path (same algorithm as daemon.rs in al-lsp
//! and client.rs in al-cli), connects, auto-starts the daemon if not running,
//! and sends/receives newline-delimited JSON-RPC messages.
//!
//! This is a self-contained copy so al-explorer has ZERO compile-time dependency
//! on al-core, al-symbols, or any other al-* analysis crate.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// JSON-RPC types (inlined — no shared crate dependency)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

// ---------------------------------------------------------------------------
// DaemonClient
// ---------------------------------------------------------------------------

/// A client for the al-lsp daemon.
pub struct DaemonClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl DaemonClient {
    /// Connect to the daemon for a project, auto-starting if needed.
    pub fn connect(project_root: &Path) -> Result<Self, String> {
        let sock_path = socket_path(project_root);

        // Try connecting first
        if let Ok(stream) = UnixStream::connect(&sock_path) {
            return Self::from_stream(stream);
        }
        // Daemon not running — start it
        Self::start_daemon(project_root)?;
        Self::wait_for_daemon(&sock_path)?;
        let stream = UnixStream::connect(&sock_path)
            .map_err(|e| format!("Failed to connect after starting daemon: {}", e))?;
        Self::from_stream(stream)
    }

    fn from_stream(stream: UnixStream) -> Result<Self, String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| format!("Failed to set timeout: {}", e))?;
        let writer = stream
            .try_clone()
            .map_err(|e| format!("Failed to clone stream: {}", e))?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
        })
    }

    fn start_daemon(project_root: &Path) -> Result<(), String> {
        let al_lsp = find_al_lsp_binary()?;
        let _child = std::process::Command::new(&al_lsp)
            .arg("daemon")
            .arg("--project")
            .arg(project_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start al-lsp daemon: {}", e))?;
        Ok(())
    }

    fn wait_for_daemon(sock_path: &Path) -> Result<(), String> {
        for _ in 0..50 {
            // 5 seconds total (50 × 100 ms)
            std::thread::sleep(Duration::from_millis(100));
            if UnixStream::connect(sock_path).is_ok() {
                return Ok(());
            }
        }
        Err("Daemon did not start within 5 seconds".to_string())
    }

    /// Send a JSON-RPC request and receive the response.
    ///
    /// Retries up to `INIT_RETRY_MAX` times with `INIT_RETRY_DELAY` backoff if
    /// the daemon reports "Workspace is initializing, try again".
    pub fn request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.send_request(method, &params)?;

        for retry in 0..=INIT_RETRY_MAX {
            let response = self.read_response()?;

            if let Some(ref err) = response.error {
                if err.message.contains("initializing") && retry < INIT_RETRY_MAX {
                    std::thread::sleep(INIT_RETRY_DELAY);
                    self.send_request(method, &params)?;
                    continue;
                }
                return Err(format!("{} (code {})", err.message, err.code));
            }

            return Ok(response.result.unwrap_or(serde_json::Value::Null));
        }

        Err("Workspace is initializing, try again".to_string())
    }

    fn send_request(
        &mut self,
        method: &str,
        params: &Option<serde_json::Value>,
    ) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;

        let req = RpcRequest {
            id,
            method: method.to_string(),
            params: params.clone(),
        };

        let mut json = serde_json::to_string(&req)
            .map_err(|e| format!("Failed to serialize request: {}", e))?;
        json.push('\n');

        self.writer
            .write_all(json.as_bytes())
            .map_err(|e| format!("Failed to send request: {}", e))?;
        self.writer
            .flush()
            .map_err(|e| format!("Failed to flush: {}", e))?;
        Ok(())
    }

    fn read_response(&mut self) -> Result<RpcResponse, String> {
        let mut line = String::new();
        let bytes_read = self
            .reader
            .read_line(&mut line)
            .map_err(|e| format!("Failed to read response: {}", e))?;
        if bytes_read == 0 {
            return Err("Connection closed by daemon (EOF)".to_string());
        }
        serde_json::from_str(line.trim())
            .map_err(|e| format!("Failed to parse response: {}", e))
    }
}

// ---------------------------------------------------------------------------
// Socket path (must match al-lsp/src/daemon.rs and al-cli/src/client.rs)
// ---------------------------------------------------------------------------

/// FNV-1a 64-bit hash — stable across Rust compiler versions.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Compute the deterministic socket path for a project root.
pub fn socket_path(project_root: &Path) -> PathBuf {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let hash = format!("{:016x}", fnv1a64(canonical.as_os_str().as_encoded_bytes()));
    let runtime_dir =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{}/al-lsp/{}.sock", runtime_dir, hash))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const INIT_RETRY_MAX: u32 = 3;
const INIT_RETRY_DELAY: Duration = Duration::from_millis(500);

fn find_al_lsp_binary() -> Result<PathBuf, String> {
    // 1. Check next to current binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("al-lsp");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // 2. Check PATH
    if let Ok(output) = std::process::Command::new("which").arg("al-lsp").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }
    Err("Cannot find al-lsp binary. Install it or add it to PATH.".to_string())
}
