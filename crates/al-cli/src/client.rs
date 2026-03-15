//! Daemon client — connects to al-lsp daemon via Unix socket.
//!
//! Computes the deterministic socket path (same algorithm as daemon.rs),
//! connects, auto-starts the daemon if not running, and sends/receives
//! newline-delimited JSON-RPC messages.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use al_protocol::jsonrpc::{Request, Response};

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
        // Daemon not running, start it
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
            // 5 seconds total (50 * 100ms)
            std::thread::sleep(Duration::from_millis(100));
            if UnixStream::connect(sock_path).is_ok() {
                return Ok(());
            }
        }
        Err("Daemon did not start within 5 seconds".to_string())
    }

    /// Send a JSON-RPC request and receive the response.
    /// Retries up to 3 times with 500ms backoff if the daemon reports
    /// "Workspace is initializing, try again".
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

        Err("Workspace is initializing, try again (code -32603)".to_string())
    }

    fn send_request(
        &mut self,
        method: &str,
        params: &Option<serde_json::Value>,
    ) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;

        let req = Request {
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

    fn read_response(&mut self) -> Result<Response, String> {
        let mut line = String::new();
        let bytes_read = self.reader
            .read_line(&mut line)
            .map_err(|e| format!("Failed to read response: {}", e))?;
        if bytes_read == 0 {
            return Err("Connection closed by daemon (EOF)".to_string());
        }
        serde_json::from_str(line.trim())
            .map_err(|e| format!("Failed to parse response: {}", e))
    }
}

/// Compute the deterministic socket path for a project root.
/// Must match the algorithm in al-lsp/src/daemon.rs.
pub fn socket_path(project_root: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut h = DefaultHasher::new();
    canonical.hash(&mut h);
    let hash = format!("{:016x}", h.finish());

    let runtime_dir =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{}/al-lsp/{}.sock", runtime_dir, hash))
}

/// Max retries for "Workspace is initializing" errors.
const INIT_RETRY_MAX: u32 = 3;
/// Delay between retries (500ms).
const INIT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Find the al-lsp binary.
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
    if let Ok(output) = std::process::Command::new("which")
        .arg("al-lsp")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }
    Err("Cannot find al-lsp binary. Install it or add it to PATH.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn unique_sock() -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join("al-cli-test-retry");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join(format!("test-{}-{}.sock", std::process::id(), n));
        let _ = std::fs::remove_file(&sock);
        sock
    }

    /// Bind a listener, then spawn a thread that accepts one connection and
    /// responds with "initializing" `fail_count` times before succeeding.
    fn mock_daemon(sock_path: &Path, fail_count: u32) -> (UnixListener, std::thread::JoinHandle<()>) {
        let listener = UnixListener::bind(sock_path).unwrap();
        let listener_clone = listener.try_clone().unwrap();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener_clone.accept().unwrap();
            let reader = std::io::BufReader::new(&stream);
            let mut writer = &stream;
            let mut count = 0u32;
            for line in reader.lines() {
                let line = line.unwrap();
                let req: Request = serde_json::from_str(&line).unwrap();
                let response = if count < fail_count {
                    count += 1;
                    Response {
                        id: req.id,
                        result: None,
                        error: Some(al_protocol::jsonrpc::RpcError {
                            code: -32603,
                            message: "Workspace is initializing, try again".to_string(),
                        }),
                    }
                } else {
                    Response {
                        id: req.id,
                        result: Some(serde_json::json!({"status": "ok"})),
                        error: None,
                    }
                };
                let mut json = serde_json::to_string(&response).unwrap();
                json.push('\n');
                writer.write_all(json.as_bytes()).unwrap();
                writer.flush().unwrap();
            }
        });
        (listener, handle)
    }

    #[test]
    fn request_retries_on_initializing_error() {
        let sock = unique_sock();
        let (_listener, _handle) = mock_daemon(&sock, 2);

        let stream = UnixStream::connect(&sock).unwrap();
        let mut client = DaemonClient::from_stream(stream).unwrap();

        let result = client.request("test/ping", None);
        assert!(result.is_ok(), "Should succeed after retries: {:?}", result);
        assert_eq!(result.unwrap()["status"], "ok");
    }

    #[test]
    fn request_fails_after_max_retries() {
        let sock = unique_sock();
        let (_listener, _handle) = mock_daemon(&sock, 100);

        let stream = UnixStream::connect(&sock).unwrap();
        let mut client = DaemonClient::from_stream(stream).unwrap();

        let result = client.request("test/ping", None);
        assert!(result.is_err(), "Should fail after exhausting retries");
        let err = result.unwrap_err();
        assert!(
            err.contains("initializing"),
            "Error should mention initializing: {}",
            err
        );
    }

    #[test]
    fn request_non_initializing_error_not_retried() {
        let sock = unique_sock();
        let listener = UnixListener::bind(&sock).unwrap();
        let listener_clone = listener.try_clone().unwrap();
        let _handle = std::thread::spawn(move || {
            let (stream, _) = listener_clone.accept().unwrap();
            let reader = std::io::BufReader::new(&stream);
            let mut writer = &stream;
            for line in reader.lines() {
                let line = line.unwrap();
                let req: Request = serde_json::from_str(&line).unwrap();
                let response = Response {
                    id: req.id,
                    result: None,
                    error: Some(al_protocol::jsonrpc::RpcError {
                        code: -32601,
                        message: "Method not found".to_string(),
                    }),
                };
                let mut json = serde_json::to_string(&response).unwrap();
                json.push('\n');
                writer.write_all(json.as_bytes()).unwrap();
                writer.flush().unwrap();
                break;
            }
        });

        let stream = UnixStream::connect(&sock).unwrap();
        let mut client = DaemonClient::from_stream(stream).unwrap();

        let result = client.request("test/unknown", None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Method not found"), "Should get immediate error: {}", err);
    }
}
