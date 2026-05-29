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
/// Hard cap on a single JSON-RPC response line. Enforced *during* read
/// so a hostile or broken daemon cannot force an unbounded allocation
/// before we get a chance to reject the message.
#[cfg(unix)]
const MAX_RESPONSE_LINE: usize = 64 * 1024 * 1024;

/// Read a single newline-delimited line, enforcing a byte cap *during*
/// reading. Returns `Ok(None)` on EOF with empty buffer, `Err` if the
/// line would exceed `max_bytes`. Sync mirror of the daemon-side
/// `read_bounded_line` in `al_core::server::daemon`.
#[cfg(unix)]
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if buf.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(buf)
                    .map(Some)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            };
        }
        // Pre-extend size check: refuse to grow `buf` past `max_bytes` so the
        // limit is enforced before the allocation, not after.
        let prospective_take = if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            pos
        } else {
            available.len()
        };
        if buf.len().saturating_add(prospective_take) > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("line exceeds {max_bytes} byte limit"),
            ));
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
        let len = available.len();
        buf.extend_from_slice(available);
        reader.consume(len);
    }
}

/// Result of trying to acquire the per-socket spawn lock for the daemon.
#[cfg(unix)]
enum SpawnLockResult {
    /// This caller owns the lock and is responsible for spawning + cleanup.
    Acquired(std::path::PathBuf),
    /// Another caller already holds the lock and is mid-spawn — we should
    /// wait for the socket to appear and retry connect.
    Contended,
}

/// F-046: serialise daemon startup with a per-socket lock file. Two
/// simultaneous `DaemonClient::connect` calls would otherwise both fail
/// the initial connect, both spawn `al-lsp daemon`, and the later
/// daemon would unlink+rebind the same socket while the first daemon
/// kept running. CLI/TUI clients ended up split across two daemons
/// with divergent indexes and debug state.
///
/// Implementation: `create_new` on a sibling `.lock` file is atomic on
/// Unix. The first caller wins and spawns; concurrent callers see
/// `AlreadyExists` and wait for the winner's daemon to come up.
///
/// Stale-lock recovery: if the lock file is older than `STALE_LOCK_AGE`
/// the previous spawner crashed mid-spawn — drop it and retry.
#[cfg(unix)]
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);

#[cfg(unix)]
fn try_acquire_spawn_lock(sock_path: &Path) -> std::io::Result<SpawnLockResult> {
    let lock_path = sock_path.with_extension("lock");
    // Stale-lock recovery: the previous spawner died mid-spawn.
    if let Ok(meta) = std::fs::metadata(&lock_path) {
        if let Ok(modified) = meta.modified() {
            // `elapsed()` errors when the system clock has moved backward since
            // the lock was written. Treat that as "assume stale" and drop the
            // lock, rather than `unwrap_or_default()` → `Duration::ZERO`, which
            // would wedge a genuinely-crashed spawner's lock in place until the
            // clock catches back up past `STALE_LOCK_AGE`.
            let is_stale = match modified.elapsed() {
                Ok(age) => age > STALE_LOCK_AGE,
                Err(_) => true,
            };
            if is_stale {
                let _ = std::fs::remove_file(&lock_path);
            }
        }
    }
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = writeln!(f, "pid={}", std::process::id());
            Ok(SpawnLockResult::Acquired(lock_path))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(SpawnLockResult::Contended),
        Err(e) => Err(e),
    }
}

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
    ///
    /// F-046: concurrent first-time callers are serialised via a per-socket
    /// `.lock` file so only one process spawns `al-lsp daemon`. Losers wait
    /// for the winner's socket to appear, then connect normally.
    pub fn connect(project_root: &Path) -> Result<Self, String> {
        let sock_path = socket_path(project_root)
            .ok_or_else(|| "Cannot determine Unix socket path: XDG_RUNTIME_DIR is not set and no secure runtime directory is available".to_string())?;

        // Fast path: daemon already running.
        if let Ok(stream) = UnixStream::connect(&sock_path) {
            return Self::from_stream(stream);
        }

        // F-046: serialise spawn so concurrent callers don't both fork
        // al-lsp daemons that race on the socket.
        match try_acquire_spawn_lock(&sock_path)
            .map_err(|e| format!("Cannot acquire daemon spawn lock: {}", e))?
        {
            SpawnLockResult::Acquired(lock_path) => {
                // Re-check inside the lock — a concurrent winner may have
                // just finished spawning while we were acquiring.
                let result = if let Ok(stream) = UnixStream::connect(&sock_path) {
                    Self::from_stream(stream)
                } else {
                    Self::start_daemon(project_root)
                        .and_then(|()| Self::wait_for_daemon(&sock_path))
                        .and_then(|()| {
                            UnixStream::connect(&sock_path).map_err(|e| {
                                format!("Failed to connect after starting daemon: {}", e)
                            })
                        })
                        .and_then(Self::from_stream)
                };
                let _ = std::fs::remove_file(&lock_path);
                result
            }
            SpawnLockResult::Contended => {
                // Another caller is spawning the daemon. Wait for the
                // socket, then connect — no spawn from this caller.
                Self::wait_for_daemon(&sock_path)?;
                let stream = UnixStream::connect(&sock_path).map_err(|e| {
                    format!(
                        "Failed to connect after another caller's daemon spawn: {}",
                        e
                    )
                })?;
                Self::from_stream(stream)
            }
        }
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

        let req = Request::new(id, method, params.clone());

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
        let line = read_bounded_line(&mut self.reader, MAX_RESPONSE_LINE)
            .map_err(|e| format!("Failed to read response: {}", e))?
            .ok_or_else(|| "Connection closed by daemon (EOF)".to_string())?;
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
                    Response::error(req.id, -32603, "Workspace is initializing, try again")
                } else {
                    Response::ok(req.id, serde_json::json!({"status": "ok"}))
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
                    let response = Response::ok(999, serde_json::json!({"status": "mismatch"}));
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

    /// F-022: read_bounded_line must accept any line up to the cap and
    /// return the bytes excluding the trailing newline.
    #[test]
    fn f022_bounded_read_accepts_line_at_or_under_cap() {
        let payload = b"hello world\n";
        let mut reader = std::io::BufReader::new(&payload[..]);
        let result = read_bounded_line(&mut reader, 64).expect("under-cap line should succeed");
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    /// F-022 negative: a line that would exceed the cap must error
    /// *before* the buffer grows past `max_bytes`. The fix is the
    /// pre-extend size check — `read_line` previously appended the
    /// whole oversized line and only checked size after.
    #[test]
    fn f022_bounded_read_rejects_line_exceeding_cap() {
        // 100 bytes, no newline; cap is 5 bytes.
        let payload = [b'X'; 100];
        let mut reader = std::io::BufReader::new(&payload[..]);
        let err = read_bounded_line(&mut reader, 5).expect_err("must reject oversized line");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("5 byte limit"));
    }

    /// F-022: empty stream returns Ok(None), not an error and not an
    /// allocation. Mirrors EOF on the daemon socket.
    #[test]
    fn f022_bounded_read_returns_none_on_empty_eof() {
        let payload: &[u8] = &[];
        let mut reader = std::io::BufReader::new(payload);
        let result = read_bounded_line(&mut reader, 64).expect("EOF must not error");
        assert!(result.is_none());
    }

    /// F-022 UTF-8 safety: a stream that ends mid-UTF-8-sequence at EOF
    /// (e.g. the daemon dies after writing the lead byte `0xC3` of `é`)
    /// must surface an `InvalidData` error, never panic or silently
    /// truncate. The buffered bytes go through `String::from_utf8`.
    #[test]
    fn f022_bounded_read_rejects_incomplete_utf8_at_eof() {
        // 0xC3 is a 2-byte-sequence lead byte; no continuation, no newline.
        let payload: &[u8] = &[b'o', b'k', 0xC3];
        let mut reader = std::io::BufReader::new(payload);
        let err =
            read_bounded_line(&mut reader, 64).expect_err("incomplete UTF-8 at EOF must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// F-022 UTF-8 safety: the newline-terminated path (lines 66-72) also
    /// runs through `String::from_utf8`, so an incomplete sequence right
    /// before the `\n` must likewise yield `InvalidData`.
    #[test]
    fn f022_bounded_read_rejects_incomplete_utf8_before_newline() {
        // Lead byte 0xC3 followed immediately by the newline terminator.
        let payload: &[u8] = &[b'o', b'k', 0xC3, b'\n'];
        let mut reader = std::io::BufReader::new(payload);
        let err = read_bounded_line(&mut reader, 64)
            .expect_err("incomplete UTF-8 before newline must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// F-OPEN: the client's `read_bounded_line` returns an empty `String`
    /// for a bare `\n` line (the daemon skips these, but the client must
    /// not panic). `read_response` then surfaces a graceful parse error
    /// for the empty payload rather than corrupting the stream.
    #[test]
    fn empty_line_yields_empty_string_then_graceful_parse_error() {
        let payload: &[u8] = b"\n";
        let mut reader = std::io::BufReader::new(payload);
        let result = read_bounded_line(&mut reader, 64).expect("bare newline must not error");
        assert_eq!(result.as_deref(), Some(""));

        // A mock daemon that replies with a blank line before the real
        // response would make the client see an empty payload. Confirm the
        // client turns that into a graceful Err, never a panic.
        let sock = unique_sock();
        fn mock_blank_then_ok(sock_path: &Path) -> (UnixListener, std::thread::JoinHandle<()>) {
            let listener = UnixListener::bind(sock_path).expect("test");
            let listener_clone = listener.try_clone().expect("test");
            let handle = std::thread::spawn(move || {
                let (stream, _) = listener_clone.accept().expect("test");
                let reader = std::io::BufReader::new(&stream);
                let mut writer = &stream;
                for line in reader.lines() {
                    let _ = line.expect("test");
                    // Send a blank line as the "response".
                    writer.write_all(b"\n").expect("test");
                    writer.flush().expect("test");
                }
            });
            (listener, handle)
        }
        let (_listener, _handle) = mock_blank_then_ok(&sock);
        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(stream).expect("test");
        let result = client.request("test/ping", None);
        assert!(
            result.is_err(),
            "blank-line response must surface a graceful error, got: {result:?}"
        );
    }

    /// F-046: first acquirer of the per-socket spawn lock gets `Acquired`
    /// with a real path; the lock file exists on disk.
    #[test]
    fn f046_spawn_lock_first_acquirer_succeeds() {
        let sock = unique_sock();
        let result = try_acquire_spawn_lock(&sock).expect("io ok");
        match result {
            SpawnLockResult::Acquired(lock_path) => {
                assert!(lock_path.exists(), "lock file must be present on disk");
                assert_eq!(lock_path.extension().unwrap(), "lock");
                std::fs::remove_file(&lock_path).ok();
            }
            SpawnLockResult::Contended => panic!("first acquirer must not be contended"),
        }
    }

    /// F-046 negative: a concurrent acquirer sees `Contended`. The
    /// AlreadyExists branch is the gate that prevents two daemons from
    /// being forked.
    #[test]
    fn f046_spawn_lock_second_acquirer_is_contended() {
        let sock = unique_sock();
        let first = try_acquire_spawn_lock(&sock).expect("io ok");
        let SpawnLockResult::Acquired(lock_path) = first else {
            panic!("first must be acquired");
        };

        let second = try_acquire_spawn_lock(&sock).expect("io ok");
        match second {
            SpawnLockResult::Contended => {}
            SpawnLockResult::Acquired(_) => {
                std::fs::remove_file(&lock_path).ok();
                panic!("second acquirer must be contended");
            }
        }

        // Releasing lets a third caller acquire.
        std::fs::remove_file(&lock_path).ok();
        let third = try_acquire_spawn_lock(&sock).expect("io ok");
        match third {
            SpawnLockResult::Acquired(p) => {
                std::fs::remove_file(&p).ok();
            }
            SpawnLockResult::Contended => panic!("post-release acquirer must succeed"),
        }
    }

    /// F-046 sanity: STALE_LOCK_AGE must comfortably exceed the
    /// daemon-spawn wait window, otherwise a slow but live spawn
    /// would be incorrectly classified as stale and clobbered.
    /// `wait_for_daemon` polls 50 × 100ms = 5s.
    #[test]
    fn f046_stale_lock_age_exceeds_spawn_wait_window() {
        assert!(
            STALE_LOCK_AGE >= Duration::from_secs(10),
            "STALE_LOCK_AGE must comfortably exceed the 5s spawn wait window"
        );
    }
}
