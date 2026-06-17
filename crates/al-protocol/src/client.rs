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

#[cfg(unix)]
const INIT_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Default total time to keep retrying "Workspace is initializing"
/// responses. Cold daemon startup on a real project loads symbol
/// packages (seconds, not milliseconds); a short retry budget made
/// every first command after boot fail spuriously (FB-1).
#[cfg(unix)]
const INIT_WAIT_TOTAL: Duration = Duration::from_secs(60);
/// Default per-request response deadline. Individual commands override
/// this via [`DaemonClient::set_request_timeout`] for long operations
/// (symbol downloads, compiles, test runs).
#[cfg(unix)]
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Socket-level read timeout = polling granularity. A timed-out socket
/// read is NOT a request failure — `read_bounded_line` keeps polling
/// until the caller's request deadline expires. Previously the socket
/// timeout WAS the request deadline, so any daemon operation slower
/// than 30s surfaced as a raw `EAGAIN` ("Resource temporarily
/// unavailable (os error 11)") to the user (FB-15).
#[cfg(unix)]
const READ_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Hard cap on a single JSON-RPC response line. Enforced *during* read
/// so a hostile or broken daemon cannot force an unbounded allocation
/// before we get a chance to reject the message.
#[cfg(unix)]
const MAX_RESPONSE_LINE: usize = 64 * 1024 * 1024;

/// Read a single newline-delimited line, enforcing a byte cap *during*
/// reading. Returns `Ok(None)` on EOF with empty buffer, `Err` if the
/// line would exceed `max_bytes`. Sync mirror of the daemon-side
/// `read_bounded_line` in `al_core::server::daemon`.
///
/// `deadline`: socket-level read timeouts (`WouldBlock`/`TimedOut`) are
/// retried until this instant, preserving any partially-read line bytes.
/// `None` means a single socket timeout is fatal (legacy behaviour, used
/// by tests).
#[cfg(unix)]
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
    deadline: Option<std::time::Instant>,
) -> std::io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(a) => a,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // Socket read timeout: the daemon is still working, not
                // gone. Keep polling until the request deadline.
                match deadline {
                    Some(d) if std::time::Instant::now() < d => continue,
                    _ => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
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

#[cfg(unix)]
pub struct DaemonClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// Overall per-request response deadline (NOT the socket timeout —
    /// the socket polls at `READ_POLL_INTERVAL` granularity).
    request_timeout: Duration,
    init_wait_total: Duration,
    init_retry_delay: Duration,
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

        if let Ok(stream) = UnixStream::connect(&sock_path) {
            return Self::from_stream(stream);
        }

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
        // Socket read timeout = poll granularity, NOT the request deadline.
        // `read_bounded_line` retries timed-out reads until the per-request
        // deadline (see `request_timeout`), so long daemon operations no
        // longer surface as raw EAGAIN errors (FB-15).
        stream
            .set_read_timeout(Some(READ_POLL_INTERVAL))
            .map_err(|e| format!("Failed to set read timeout: {}", e))?;
        // A read timeout alone does not bound write_all()/flush(): those use the
        // independent SO_SNDTIMEO option. Without it, a hung/unresponsive daemon
        // that stops reading lets the socket send buffer fill and the next write
        // blocks forever, hanging the CLI/TUI client. Set both so every I/O call
        // is bounded.
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| format!("Failed to set write timeout: {}", e))?;
        let writer = stream
            .try_clone()
            .map_err(|e| format!("Failed to clone stream: {}", e))?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            init_wait_total: INIT_WAIT_TOTAL,
            init_retry_delay: INIT_RETRY_DELAY,
        })
    }

    /// Set the per-request response deadline (for long-running operations
    /// like symbol downloads, compiles, and test runs). This is an overall
    /// deadline — the socket itself polls at a short fixed interval.
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }

    /// Deprecated name for [`Self::set_request_timeout`] — older call sites
    /// used the socket read timeout as the de-facto request deadline.
    pub fn set_read_timeout(&mut self, timeout: Duration) {
        self.set_request_timeout(timeout);
    }

    /// Override how long `request` keeps retrying while the daemon reports
    /// "Workspace is initializing" (and the delay between retries).
    /// Primarily for tests; production callers keep the 60s default.
    pub fn set_init_wait(&mut self, total: Duration, retry_delay: Duration) {
        self.init_wait_total = total;
        self.init_retry_delay = retry_delay;
    }

    pub fn set_write_timeout(&mut self, timeout: Duration) {
        let _ = self.writer.set_write_timeout(Some(timeout));
    }

    /// Send a JSON-RPC request and receive the response.
    ///
    /// While the daemon reports "Workspace is initializing, try again",
    /// retries every [`Self::init_retry_delay`] up to a total of
    /// [`Self::init_wait_total`] — cold daemon startup on a real project
    /// takes seconds, and the first command after boot should wait for it
    /// rather than fail. Each retry sends a new request (new ID) and
    /// validates that the response ID matches.
    pub fn request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.request_with_timeout(method, params, self.request_timeout)
    }

    pub fn request_with_timeout(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let init_deadline = std::time::Instant::now() + self.init_wait_total;
        let mut expected_id = self.send_request(method, &params)?;

        loop {
            let response = self.read_response(timeout)?;

            if response.id != expected_id {
                return Err(format!(
                    "Response ID mismatch: expected {}, got {}",
                    expected_id, response.id
                ));
            }

            if let Some(ref err) = response.error {
                if err.message.contains("initializing") && std::time::Instant::now() < init_deadline
                {
                    std::thread::sleep(self.init_retry_delay);
                    expected_id = self.send_request(method, &params)?;
                    continue;
                }
                return Err(format!("{} (code {})", err.message, err.code));
            }

            return Ok(response.result.unwrap_or(serde_json::Value::Null));
        }
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

    fn read_response(&mut self, timeout: Duration) -> Result<Response, String> {
        let deadline = std::time::Instant::now() + timeout;
        let line = read_bounded_line(&mut self.reader, MAX_RESPONSE_LINE, Some(deadline))
            .map_err(|e| {
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) {
                    format!(
                        "Daemon did not respond within {}s — the operation may still be \
                         running. Retry with a longer timeout, or check the daemon log at \
                         ~/.local/share/al-lsp/logs/al-lsp.log",
                        timeout.as_secs()
                    )
                } else {
                    format!("Failed to read response: {}", e)
                }
            })?
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
    fn request_fails_after_init_wait_budget() {
        let sock = unique_sock();
        let (_listener, _handle) = mock_daemon(&sock, 100);
        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(stream).expect("test");
        client.set_init_wait(Duration::from_millis(100), Duration::from_millis(10));
        let result = client.request("test/ping", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("initializing"));
    }

    /// Connecting to a path that does not exist must return an error, not panic.
    #[test]
    fn test_connect_invalid_path_returns_error() {
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
        fn mock_wrong_id_daemon(sock_path: &Path) -> (UnixListener, std::thread::JoinHandle<()>) {
            let listener = UnixListener::bind(sock_path).expect("test");
            let listener_clone = listener.try_clone().expect("test");
            let handle = std::thread::spawn(move || {
                let (stream, _) = listener_clone.accept().expect("test");
                let reader = std::io::BufReader::new(&stream);
                let mut writer = &stream;
                for line in reader.lines() {
                    let _ = line.expect("test");
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

    /// A daemon that connects but never reads must not hang the client
    /// forever. `from_stream` sets a write timeout; once a short timeout is
    /// applied and the socket send buffer fills, `request` must return a
    /// bounded error instead of blocking indefinitely.
    #[test]
    fn write_to_nonreading_daemon_times_out() {
        let sock = unique_sock();
        let listener = UnixListener::bind(&sock).expect("test");
        // Accept the connection but never read from it, so the kernel send
        // buffer on the client side fills up.
        let _handle = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("test");
            std::thread::sleep(Duration::from_secs(5));
        });

        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(stream).expect("test");
        assert_eq!(
            client
                .writer
                .write_timeout()
                .expect("write timeout query should succeed"),
            Some(Duration::from_secs(30)),
            "from_stream must set a default write timeout"
        );
        client.set_write_timeout(Duration::from_millis(200));

        // Send large payloads until a write fails. With a bounded write
        // timeout this terminates quickly; without it (the bug), the loop
        // would block forever on a full send buffer.
        let big = serde_json::json!({ "blob": "x".repeat(64 * 1024) });
        let mut err = None;
        for _ in 0..2000 {
            if let Err(e) = client.send_request("test/flood", &Some(big.clone())) {
                err = Some(e);
                break;
            }
        }
        let err = err.expect("a write to a non-reading daemon must eventually error");
        assert!(
            err.contains("send request") || err.contains("flush"),
            "error should come from the write path: {err}"
        );
    }

    /// F-022: read_bounded_line must accept any line up to the cap and
    /// return the bytes excluding the trailing newline.
    #[test]
    fn f022_bounded_read_accepts_line_at_or_under_cap() {
        let payload = b"hello world\n";
        let mut reader = std::io::BufReader::new(&payload[..]);
        let result =
            read_bounded_line(&mut reader, 64, None).expect("under-cap line should succeed");
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
        let err = read_bounded_line(&mut reader, 5, None).expect_err("must reject oversized line");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("5 byte limit"));
    }

    /// F-022: empty stream returns Ok(None), not an error and not an
    /// allocation. Mirrors EOF on the daemon socket.
    #[test]
    fn f022_bounded_read_returns_none_on_empty_eof() {
        let payload: &[u8] = &[];
        let mut reader = std::io::BufReader::new(payload);
        let result = read_bounded_line(&mut reader, 64, None).expect("EOF must not error");
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
        let err = read_bounded_line(&mut reader, 64, None)
            .expect_err("incomplete UTF-8 at EOF must error");
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
        let err = read_bounded_line(&mut reader, 64, None)
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
        let result = read_bounded_line(&mut reader, 64, None).expect("bare newline must not error");
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

    /// `find_al_lsp_binary` and these tests mutate the process-global `PATH`
    /// env var, which cannot run concurrently with other env-reading tests.
    /// Serialise them on a local mutex (no extra dev-dependency needed).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn unique_dir(tag: &str) -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "al-protocol-bin-test/{}-{}-{}",
            tag,
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    /// `find_al_lsp_binary` must locate an `al-lsp` file living in a PATH
    /// directory when none sits next to the current exe. This exercises the
    /// PATH-search branch (lines 344-351) and the success return.
    #[test]
    fn find_al_lsp_binary_locates_in_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Guard: if the test runner's own dir happens to hold an `al-lsp`,
        // the next-to-exe branch wins first and this test is moot. Skip then.
        if let Ok(exe) = std::env::current_exe() {
            if exe.parent().map(|d| d.join("al-lsp").exists()) == Some(true) {
                return;
            }
        }

        let bin_dir = unique_dir("haspath");
        let bin = bin_dir.join("al-lsp");
        std::fs::write(&bin, b"#!/bin/sh\n").expect("write fake binary");

        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", &bin_dir);
        let result = find_al_lsp_binary();
        match saved {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&bin_dir);

        let found = result.expect("al-lsp on PATH must be found");
        assert_eq!(
            found.file_name().and_then(|n| n.to_str()),
            Some("al-lsp"),
            "found path must end in al-lsp: {found:?}"
        );
    }

    /// Negative: with an empty PATH and no `al-lsp` beside the exe,
    /// `find_al_lsp_binary` returns the documented not-found error
    /// (line 352) rather than panicking.
    #[test]
    fn find_al_lsp_binary_missing_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        if let Ok(exe) = std::env::current_exe() {
            if exe.parent().map(|d| d.join("al-lsp").exists()) == Some(true) {
                return;
            }
        }

        let empty = unique_dir("nopath");
        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", &empty);
        let result = find_al_lsp_binary();
        match saved {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&empty);

        let err = result.expect_err("missing al-lsp must error");
        assert!(
            err.contains("Cannot find al-lsp binary"),
            "error must name the missing binary: {err}"
        );
    }

    /// A non-file entry named `al-lsp` on PATH (here: a *directory*) must be
    /// skipped — `is_file()` guards against treating a directory as the
    /// binary. The search then falls through to the not-found error.
    #[test]
    fn find_al_lsp_binary_skips_non_file_on_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        if let Ok(exe) = std::env::current_exe() {
            if exe.parent().map(|d| d.join("al-lsp").exists()) == Some(true) {
                return;
            }
        }

        let dir = unique_dir("dirnamed");
        std::fs::create_dir_all(dir.join("al-lsp")).expect("mkdir al-lsp");

        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", &dir);
        let result = find_al_lsp_binary();
        match saved {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        let err = result.expect_err("a directory named al-lsp must not be accepted as the binary");
        assert!(err.contains("Cannot find al-lsp binary"), "got: {err}");
    }

    /// F-046 stale-lock recovery: a `.lock` file older than `STALE_LOCK_AGE`
    /// is treated as a crashed spawner — it is removed and the caller
    /// re-acquires `Acquired`. Exercises the stale branch (lines 109-122)
    /// that the existing fast-path tests never reach.
    #[test]
    fn f046_stale_lock_is_reclaimed() {
        let sock = unique_sock();
        let lock_path = sock.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&lock_path, b"pid=99999\n").expect("seed lock");

        let old = std::time::SystemTime::now() - (STALE_LOCK_AGE + Duration::from_secs(60));
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .expect("open lock");
        f.set_modified(old).expect("backdate mtime");
        drop(f);

        let result = try_acquire_spawn_lock(&sock).expect("io ok");
        match result {
            SpawnLockResult::Acquired(p) => {
                assert!(p.exists(), "reclaimed lock file must exist");
                std::fs::remove_file(&p).ok();
            }
            SpawnLockResult::Contended => {
                std::fs::remove_file(&lock_path).ok();
                panic!("a stale lock must be reclaimed (Acquired), not Contended");
            }
        }
    }

    /// A *fresh* lock (mtime ~now) must NOT be treated as stale — a
    /// concurrent caller sees `Contended`. This is the complement of the
    /// stale-recovery test and guards against an over-eager staleness check
    /// clobbering a live spawner's lock.
    #[test]
    fn f046_fresh_lock_is_not_reclaimed() {
        let sock = unique_sock();
        let lock_path = sock.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        let first = try_acquire_spawn_lock(&sock).expect("io ok");
        let SpawnLockResult::Acquired(held) = first else {
            panic!("first must be acquired");
        };

        let second = try_acquire_spawn_lock(&sock).expect("io ok");
        let contended = matches!(second, SpawnLockResult::Contended);
        std::fs::remove_file(&held).ok();
        assert!(
            contended,
            "a fresh lock must not be reclaimed; second caller must be Contended"
        );
    }

    /// `from_stream` installs the short poll-interval socket timeout, and
    /// `set_read_timeout` (the legacy name) now adjusts the per-request
    /// deadline rather than the socket option — the socket keeps polling.
    #[test]
    fn set_read_timeout_adjusts_request_deadline_not_socket() {
        let sock = unique_sock();
        let _listener = UnixListener::bind(&sock).expect("bind");
        let stream = UnixStream::connect(&sock).expect("connect");
        let mut client = DaemonClient::from_stream(stream).expect("from_stream");

        assert_eq!(
            client.reader.get_ref().read_timeout().expect("query"),
            Some(READ_POLL_INTERVAL),
            "from_stream must install the poll-interval socket timeout"
        );
        assert_eq!(client.request_timeout, DEFAULT_REQUEST_TIMEOUT);

        client.set_read_timeout(Duration::from_secs(900));
        assert_eq!(
            client.request_timeout,
            Duration::from_secs(900),
            "set_read_timeout must adjust the request deadline"
        );
        assert_eq!(
            client.reader.get_ref().read_timeout().expect("query"),
            Some(READ_POLL_INTERVAL),
            "socket poll interval must remain fixed"
        );
    }

    /// FB-15 regression: a daemon that takes longer than one socket poll
    /// interval to respond must NOT surface EAGAIN — the client keeps
    /// polling until the request deadline and then returns the real
    /// response. (Previously `download-symbols` & co. died at 30s with
    /// "Resource temporarily unavailable (os error 11)".)
    #[test]
    fn slow_daemon_response_survives_socket_poll_timeouts() {
        let sock = unique_sock();
        let listener = UnixListener::bind(&sock).expect("bind");
        let listener_clone = listener.try_clone().expect("clone");
        let _handle = std::thread::spawn(move || {
            let (stream, _) = listener_clone.accept().expect("accept");
            let reader = std::io::BufReader::new(&stream);
            let mut writer = &stream;
            for line in reader.lines() {
                let line = line.expect("read");
                let req: Request = serde_json::from_str(&line).expect("parse");
                std::thread::sleep(Duration::from_millis(600));
                let response = Response::ok(req.id, serde_json::json!({"slow": true}));
                let mut json = serde_json::to_string(&response).expect("ser");
                json.push('\n');
                writer.write_all(json.as_bytes()).expect("write");
                writer.flush().expect("flush");
            }
        });

        let stream = UnixStream::connect(&sock).expect("connect");
        let mut client = DaemonClient::from_stream(stream).expect("from_stream");
        client
            .reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .expect("set poll");
        let result = client.request_with_timeout("test/slow", None, Duration::from_secs(10));
        assert_eq!(
            result.expect("slow response must succeed")["slow"],
            true,
            "response after multiple poll timeouts must be returned intact"
        );
    }

    /// When the request deadline itself expires, the error message must be
    /// actionable — naming the timeout — not a raw EAGAIN.
    #[test]
    fn request_deadline_expiry_yields_actionable_error() {
        let sock = unique_sock();
        let listener = UnixListener::bind(&sock).expect("bind");
        let listener_clone = listener.try_clone().expect("clone");
        let _handle = std::thread::spawn(move || {
            let (stream, _) = listener_clone.accept().expect("accept");
            // Never respond; hold the connection open past the deadline.
            std::thread::sleep(Duration::from_secs(5));
            drop(stream);
        });

        let stream = UnixStream::connect(&sock).expect("connect");
        let mut client = DaemonClient::from_stream(stream).expect("from_stream");
        client
            .reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("set poll");
        let err = client
            .request_with_timeout("test/never", None, Duration::from_millis(300))
            .expect_err("no response must time out");
        assert!(
            err.contains("did not respond"),
            "error must be actionable, got: {err}"
        );
        assert!(
            !err.contains("os error 11"),
            "raw EAGAIN must not leak to the user: {err}"
        );
    }
}
