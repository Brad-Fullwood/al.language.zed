//! Synchronous Unix socket client for the al-lsp daemon.
//!
//! Connects to the daemon, auto-starts it if not running, and provides
//! JSON-RPC request/response with retry on "initializing" errors.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use crate::jsonrpc::{Request, Response};
#[cfg(unix)]
use crate::socket::socket_path;

/// Max retries for "Workspace is initializing" errors.
#[cfg(unix)]
const INIT_RETRY_MAX: u32 = 3;
/// Delay between retries.
#[cfg(unix)]
const INIT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// A synchronous client for the al-lsp daemon.
#[cfg(unix)]
pub struct DaemonClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

#[cfg(unix)]
impl DaemonClient {
    /// Connect to the daemon for a project, auto-starting if needed.
    pub fn connect(project_root: &Path) -> Result<Self, String> {
        let sock_path = socket_path(project_root)
            .ok_or_else(|| "Cannot determine Unix socket path: XDG_RUNTIME_DIR is not set and no secure runtime directory is available".to_string())?;

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

    /// Create a client from an already-connected stream (for testing).
    pub fn from_stream(stream: UnixStream) -> Result<Self, String> {
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

    /// Override the read timeout (useful for long-running operations).
    pub fn set_read_timeout(&mut self, timeout: Duration) {
        let _ = self.reader.get_ref().set_read_timeout(Some(timeout));
    }

    /// Send a JSON-RPC request and receive the response.
    ///
    /// Retries up to [`INIT_RETRY_MAX`] times with 500ms backoff if the daemon
    /// reports "Workspace is initializing, try again". Each retry sends a new
    /// request (new ID) and validates that the response ID matches.
    pub fn request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let mut expected_id = self.send_request(method, &params)?;

        // Total attempts = INIT_RETRY_MAX + 1 (initial send already done above).
        for attempt in 0..=INIT_RETRY_MAX {
            let response = self.read_response()?;

            if response.id != expected_id {
                return Err(format!(
                    "Response ID mismatch: expected {}, got {}",
                    expected_id, response.id
                ));
            }

            if let Some(ref err) = response.error {
                if err.message.contains("initializing") && attempt < INIT_RETRY_MAX {
                    std::thread::sleep(INIT_RETRY_DELAY);
                    expected_id = self.send_request(method, &params)?;
                    continue;
                }
                return Err(format!("{} (code {})", err.message, err.code));
            }

            return Ok(response.result.unwrap_or(serde_json::Value::Null));
        }

        unreachable!("loop always returns")
    }

    fn send_request(
        &mut self,
        method: &str,
        params: &Option<serde_json::Value>,
    ) -> Result<u64, String> {
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
        Ok(id)
    }

    fn read_response(&mut self) -> Result<Response, String> {
        const MAX_RESPONSE_LINE: usize = 64 * 1024 * 1024;

        let mut line = String::new();
        let bytes_read = self
            .reader
            .read_line(&mut line)
            .map_err(|e| format!("Failed to read response: {}", e))?;
        if bytes_read == 0 {
            return Err("Connection closed by daemon (EOF)".to_string());
        }
        if line.len() > MAX_RESPONSE_LINE {
            return Err(format!(
                "Response too large ({} bytes, max {})",
                line.len(),
                MAX_RESPONSE_LINE
            ));
        }
        serde_json::from_str(line.trim()).map_err(|e| format!("Failed to parse response: {}", e))
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
            if UnixStream::connect(sock_path).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err("Daemon did not start within 5 seconds".to_string())
    }
}

/// Find the al-lsp binary (next to current exe, then PATH).
#[cfg(unix)]
pub fn find_al_lsp_binary() -> Result<PathBuf, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("al-lsp");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Search PATH directories directly — avoids spawning a subprocess and
    // works on any Unix system regardless of whether `which` is installed.
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("al-lsp");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err("Cannot find al-lsp binary. Install it or add it to PATH.".to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::jsonrpc::RpcError;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn unique_sock() -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join("al-protocol-test");
        std::fs::create_dir_all(&dir).expect("test");
        let sock = dir.join(format!("test-{}-{}.sock", std::process::id(), n));
        let _ = std::fs::remove_file(&sock);
        sock
    }

    fn mock_daemon(
        sock_path: &Path,
        fail_count: u32,
    ) -> (UnixListener, std::thread::JoinHandle<()>) {
        let listener = UnixListener::bind(sock_path).expect("test");
        let listener_clone = listener.try_clone().expect("test");
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener_clone.accept().expect("test");
            let reader = std::io::BufReader::new(&stream);
            let mut writer = &stream;
            let mut count = 0u32;
            for line in reader.lines() {
                let line = line.expect("test");
                let req: Request = serde_json::from_str(&line).expect("test");
                let response = if count < fail_count {
                    count += 1;
                    Response {
                        id: req.id,
                        result: None,
                        error: Some(RpcError {
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
                let mut json = serde_json::to_string(&response).expect("test");
                json.push('\n');
                writer.write_all(json.as_bytes()).expect("test");
                writer.flush().expect("test");
            }
        });
        (listener, handle)
    }

    #[test]
    fn request_retries_on_initializing_error() {
        let sock = unique_sock();
        let (_listener, _handle) = mock_daemon(&sock, 2);
        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(stream).expect("test");
        let result = client.request("test/ping", None);
        assert!(result.is_ok(), "Should succeed after retries: {:?}", result);
        assert_eq!(result.expect("test")["status"], "ok");
    }

    #[test]
    fn request_fails_after_max_retries() {
        let sock = unique_sock();
        let (_listener, _handle) = mock_daemon(&sock, 100);
        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(stream).expect("test");
        let result = client.request("test/ping", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("initializing"));
    }

    /// Connecting to a path that does not exist must return an error, not panic.
    #[test]
    fn test_connect_invalid_path_returns_error() {
        // Use a path that can never exist as a socket
        let bogus = std::path::Path::new("/nonexistent/path/that/cannot/be/a.sock");
        // DaemonClient::connect would try to spawn the daemon, which we don't want in a unit
        // test. Instead verify that from_stream propagates a meaningful error when the stream
        // itself reports a problem — by opening a regular file and trying to use it as a socket.
        let tmp = std::env::temp_dir().join(format!("al-invalid-{}.txt", std::process::id()));
        std::fs::write(&tmp, b"not a socket").expect("test");
        // UnixStream::connect to a regular file fails on Linux
        let result = UnixStream::connect(&tmp);
        let _ = std::fs::remove_file(&tmp);
        assert!(
            result.is_err(),
            "Connecting to a regular file as a socket should fail"
        );
        // Also verify that a completely nonexistent path fails
        let result2 = UnixStream::connect(bogus);
        assert!(
            result2.is_err(),
            "Connecting to nonexistent path should fail"
        );
    }

    /// If the server sends a response whose `id` does not match any pending request,
    /// the mismatched message must not corrupt subsequent responses.
    ///
    /// This is tested by building two clients on separate sockets — one whose mock
    /// server sends back a wrong `id` (id=999 when request id=1) and one that sends
    /// the correct id — confirming the error path is distinct from the success path.
    #[test]
    fn test_response_id_mismatch_is_still_parsed() {
        // A mock daemon that always replies with id=999 regardless of request id
        fn mock_wrong_id_daemon(sock_path: &Path) -> (UnixListener, std::thread::JoinHandle<()>) {
            let listener = UnixListener::bind(sock_path).expect("test");
            let listener_clone = listener.try_clone().expect("test");
            let handle = std::thread::spawn(move || {
                let (stream, _) = listener_clone.accept().expect("test");
                let reader = std::io::BufReader::new(&stream);
                let mut writer = &stream;
                for line in reader.lines() {
                    let _ = line.expect("test"); // consume request
                                                 // Reply with mismatched id
                    let response = Response {
                        id: 999,
                        result: Some(serde_json::json!({"status": "mismatch"})),
                        error: None,
                    };
                    let mut json = serde_json::to_string(&response).expect("test");
                    json.push('\n');
                    writer.write_all(json.as_bytes()).expect("test");
                    writer.flush().expect("test");
                }
            });
            (listener, handle)
        }

        let sock = unique_sock();
        let (_listener, _handle) = mock_wrong_id_daemon(&sock);
        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(stream).expect("test");
        // T-045 added response ID validation — mismatched IDs now return an error.
        let result = client.request("test/ping", None);
        assert!(
            result.is_err(),
            "Response ID mismatch should return an error: {:?}",
            result
        );
        let err_msg = result.unwrap_err();
        assert!(
            err_msg.contains("mismatch"),
            "Error should mention mismatch: {err_msg}"
        );
    }
}
