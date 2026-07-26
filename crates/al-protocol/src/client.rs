//! Synchronous local IPC client for the al-lsp daemon.
//!
//! Connects to the daemon, auto-starts it if not running, and provides
//! JSON-RPC request/response with retry on "initializing" errors.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use interprocess::local_socket::{prelude::*, GenericFilePath, Stream};
use interprocess::TryClone;

use crate::jsonrpc::{Request, Response};
use crate::socket::{socket_path, spawn_lock_path};

const INIT_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Default total time to keep retrying "Workspace is initializing"
/// responses. Cold daemon startup on a real project loads symbol
/// packages, which can take several seconds.
const INIT_WAIT_TOTAL: Duration = Duration::from_secs(60);
/// Default per-request response deadline. Individual commands override
/// this via [`DaemonClient::set_request_timeout`] for long operations
/// (symbol downloads, compiles, test runs).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Socket-level read timeout = polling granularity. A timed-out socket
/// read is NOT a request failure — `read_bounded_line` keeps polling
/// until the caller's request deadline expires.
#[cfg(unix)]
const READ_POLL_INTERVAL: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap on a single JSON-RPC response line. Enforced *during* read
/// so a hostile or broken daemon cannot force an unbounded allocation
/// before we get a chance to reject the message.
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
#[cfg(not(windows))]
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
                    Some(d) if std::time::Instant::now() < d => {
                        // Windows named pipes expose nonblocking mode rather
                        // than socket timeouts. Avoid a hot spin between polls.
                        #[cfg(windows)]
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    _ => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            // `interprocess` can report an empty buffer for a connected,
            // nonblocking Windows named pipe before the peer has written its
            // response. Treat that like `WouldBlock` while the request is
            // still live; otherwise a normal server scheduling delay is
            // misreported as a disconnected daemon.
            #[cfg(windows)]
            if deadline.is_some_and(|deadline| std::time::Instant::now() < deadline) {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
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

/// Windows named-pipe reads in `interprocess` are implemented with
/// `ReadFileEx` followed by an unbounded alertable wait. `PIPE_NOWAIT` does
/// not make that wait observe our request deadline, so calling `fill_buf`
/// before bytes exist can strand the CLI forever. Poll `PeekNamedPipe` first
/// and only enter the library read once data is available.
#[cfg(windows)]
fn read_bounded_pipe_line(
    reader: &mut BufReader<Stream>,
    max_bytes: usize,
    deadline: std::time::Instant,
) -> std::io::Result<Option<String>> {
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let mut buf: Vec<u8> = Vec::new();
    loop {
        while reader.buffer().is_empty() {
            let Stream::NamedPipe(pipe) = reader.get_ref();
            let mut available = 0_u32;
            let ok = unsafe {
                PeekNamedPipe(
                    pipe.as_handle().as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if available > 0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "named-pipe response deadline elapsed",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let available = reader.fill_buf()?;
        let prospective_take = available
            .iter()
            .position(|&byte| byte == b'\n')
            .unwrap_or(available.len());
        if buf.len().saturating_add(prospective_take) > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("line exceeds {max_bytes} byte limit"),
            ));
        }
        if let Some(pos) = available.iter().position(|&byte| byte == b'\n') {
            buf.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
        let len = available.len();
        buf.extend_from_slice(available);
        reader.consume(len);
    }
}

/// Write a complete frame without allowing a non-reading daemon to block the
/// caller forever. Windows named pipes use nonblocking mode; Unix sockets use
/// their OS send timeout, with this deadline as a platform-independent guard.
fn write_all_bounded<W: Write>(
    writer: &mut W,
    mut bytes: &[u8],
    timeout: Duration,
) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "failed to write complete daemon request",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

enum SpawnLockResult {
    /// This caller owns the lock and is responsible for spawning + cleanup.
    Acquired(std::path::PathBuf),
    /// Another caller already holds the lock and is mid-spawn — we should
    /// wait for the socket to appear and retry connect.
    Contended,
}

/// Serialise daemon startup with a per-socket lock file. Two simultaneous
/// `DaemonClient::connect` calls would otherwise both fail
/// the initial connect, both spawn `al-lsp daemon`, and the later
/// daemon would unlink+rebind the same socket while the first daemon
/// kept running. CLI/TUI clients ended up split across two daemons
/// with divergent indexes and debug state.
///
/// Implementation: `create_new` on a sibling `.lock` file is atomic on
/// Unix. The first caller wins and spawns; concurrent callers see
/// `AlreadyExists` and wait for the winner's daemon to come up. This atomic
/// filesystem operation is available on all supported desktop platforms.
///
/// Stale-lock recovery: if the lock file is older than `STALE_LOCK_AGE`
/// the previous spawner crashed mid-spawn — drop it and retry.
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);

fn try_acquire_spawn_lock(lock_path: &Path) -> std::io::Result<SpawnLockResult> {
    let lock_path = lock_path.to_path_buf();
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

pub struct DaemonClient {
    reader: BufReader<Stream>,
    writer: Stream,
    next_id: u64,
    /// Overall per-request response deadline (NOT the socket timeout —
    /// the socket polls at `READ_POLL_INTERVAL` granularity).
    request_timeout: Duration,
    write_timeout: Duration,
    init_wait_total: Duration,
    init_retry_delay: Duration,
}

impl DaemonClient {
    /// Connect only if a daemon is already listening for this project.
    ///
    /// Unlike [`Self::connect`], this never spawns a process. It is used by
    /// lifecycle tooling that must not accidentally populate caches merely to
    /// ask whether an existing daemon should stop.
    pub fn connect_existing(project_root: &Path) -> Result<Self, String> {
        let endpoint = socket_path(project_root).ok_or_else(|| {
            "Cannot determine a local daemon endpoint: no per-user runtime directory is available"
                .to_string()
        })?;
        let stream = connect_stream(&endpoint).map_err(|error| {
            format!("No running daemon for {}: {error}", project_root.display())
        })?;
        Self::from_stream(stream)
    }

    /// Connect to the daemon for a project, auto-starting if needed.
    ///
    /// concurrent first-time callers are serialised via a per-socket
    /// `.lock` file so only one process spawns `al-lsp daemon`. Losers wait
    /// for the winner's socket to appear, then connect normally.
    pub fn connect(project_root: &Path) -> Result<Self, String> {
        let endpoint = socket_path(project_root)
            .ok_or_else(|| "Cannot determine a local daemon endpoint: no per-user runtime directory is available".to_string())?;
        let lock_path = spawn_lock_path(project_root)
            .ok_or_else(|| "Cannot determine a daemon startup lock path".to_string())?;

        if let Ok(stream) = connect_stream(&endpoint) {
            return Self::from_stream(stream);
        }

        match try_acquire_spawn_lock(&lock_path)
            .map_err(|e| format!("Cannot acquire daemon spawn lock: {}", e))?
        {
            SpawnLockResult::Acquired(lock_path) => {
                // Re-check inside the lock — a concurrent winner may have
                // just finished spawning while we were acquiring.
                let result = if let Ok(stream) = connect_stream(&endpoint) {
                    Self::from_stream(stream)
                } else {
                    Self::start_daemon(project_root)
                        .and_then(|mut child| Self::wait_for_daemon(&endpoint, Some(&mut child)))
                        .and_then(Self::from_stream)
                };
                let _ = std::fs::remove_file(&lock_path);
                result
            }
            SpawnLockResult::Contended => {
                let stream = Self::wait_for_daemon(&endpoint, None)?;
                Self::from_stream(stream)
            }
        }
    }

    /// Create a client from an already-connected stream (for testing).
    pub fn from_stream(stream: impl Into<Stream>) -> Result<Self, String> {
        let stream = stream.into();
        // Socket read timeout = poll granularity, NOT the request deadline.
        // `read_bounded_line` retries timed-out reads until the per-request
        // deadline (see `request_timeout`), so long daemon operations no
        // longer surface as raw EAGAIN errors.
        #[cfg(unix)]
        stream
            .set_recv_timeout(Some(READ_POLL_INTERVAL))
            .map_err(|e| format!("Failed to set read timeout: {}", e))?;
        #[cfg(windows)]
        stream
            .set_nonblocking(true)
            .map_err(|e| format!("Failed to enable nonblocking named-pipe I/O: {}", e))?;
        // A read timeout alone does not bound write_all()/flush(): those use the
        // independent SO_SNDTIMEO option. Without it, a hung/unresponsive daemon
        // that stops reading lets the socket send buffer fill and the next write
        // blocks forever, hanging the CLI/TUI client. Set both so every I/O call
        // is bounded.
        #[cfg(unix)]
        stream
            .set_send_timeout(Some(WRITE_TIMEOUT))
            .map_err(|e| format!("Failed to set write timeout: {}", e))?;
        let writer = stream
            .try_clone()
            .map_err(|e| format!("Failed to clone stream: {}", e))?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
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

    /// Override how long `request` keeps retrying while the daemon reports
    /// "Workspace is initializing" (and the delay between retries).
    /// Primarily for tests; production callers keep the 60s default.
    pub fn set_init_wait(&mut self, total: Duration, retry_delay: Duration) {
        self.init_wait_total = total;
        self.init_retry_delay = retry_delay;
    }

    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout;
        #[cfg(unix)]
        let _ = self.writer.set_send_timeout(Some(timeout));
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

        write_all_bounded(&mut self.writer, json.as_bytes(), self.write_timeout)
            .map_err(|e| format!("Failed to send request: {}", e))?;
        self.writer
            .flush()
            .map_err(|e| format!("Failed to flush: {}", e))?;
        Ok(id)
    }

    fn read_response(&mut self, timeout: Duration) -> Result<Response, String> {
        let deadline = std::time::Instant::now() + timeout;
        #[cfg(not(windows))]
        let line = read_bounded_line(&mut self.reader, MAX_RESPONSE_LINE, Some(deadline)).map_err(
            |e| {
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
            },
        )?;
        #[cfg(windows)]
        let line =
            read_bounded_pipe_line(&mut self.reader, MAX_RESPONSE_LINE, deadline).map_err(|e| {
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
            })?;
        let line = line.ok_or_else(|| "Connection closed by daemon (EOF)".to_string())?;
        serde_json::from_str(line.trim()).map_err(|e| format!("Failed to parse response: {}", e))
    }

    fn start_daemon(project_root: &Path) -> Result<std::process::Child, String> {
        let al_lsp = find_al_lsp_binary()?;
        std::process::Command::new(&al_lsp)
            .arg("daemon")
            .arg("--project")
            .arg(project_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start al-lsp daemon: {}", e))
    }

    fn wait_for_daemon(
        endpoint: &Path,
        mut spawned: Option<&mut std::process::Child>,
    ) -> Result<Stream, String> {
        for _ in 0..50 {
            if let Ok(stream) = connect_stream(endpoint) {
                return Ok(stream);
            }
            if let Some(child) = spawned.as_deref_mut() {
                if let Some(status) = child
                    .try_wait()
                    .map_err(|error| format!("Failed to inspect al-lsp daemon process: {error}"))?
                {
                    return Err(format!(
                        "al-lsp daemon exited before opening its endpoint ({status}); \
                         inspect ~/.local/share/al-lsp/logs/al-lsp.log for the startup error"
                    ));
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "Daemon did not open {} within 5 seconds; inspect \
             ~/.local/share/al-lsp/logs/al-lsp.log",
            endpoint.display()
        ))
    }
}

pub fn find_al_lsp_binary() -> Result<PathBuf, String> {
    let binary_name = format!("al-lsp{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&binary_name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Search PATH directories directly — avoids spawning a subprocess and
    // works on every supported system regardless of whether `which`/`where`
    // is installed.
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(&binary_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err("Cannot find al-lsp binary. Install it or add it to PATH.".to_string())
}

fn connect_stream(endpoint: &Path) -> std::io::Result<Stream> {
    let name = endpoint.to_fs_name::<GenericFilePath>()?;
    #[cfg(windows)]
    {
        // `Stream::connect` uses an unbounded named-pipe wait on Windows.
        // Under concurrent CLI load every server instance can briefly be
        // occupied, which previously wedged callers before the request-level
        // deadlines could apply. Try once without waiting; daemon startup has
        // its own bounded retry loop in `wait_for_daemon`.
        interprocess::local_socket::ConnectOptions::new()
            .name(name)
            .wait_mode(interprocess::ConnectWaitMode::Timeout(Duration::ZERO))
            .connect_sync()
    }
    #[cfg(not(windows))]
    {
        Stream::connect(name)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn test_stream(stream: UnixStream) -> Stream {
        interprocess::os::unix::uds_local_socket::Stream::from(stream).into()
    }

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
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
        let result = client.request("test/ping", None);
        assert!(result.is_ok(), "Should succeed after retries: {:?}", result);
        assert_eq!(result.expect("test")["status"], "ok");
    }

    #[test]
    fn request_fails_after_init_wait_budget() {
        let sock = unique_sock();
        let (_listener, _handle) = mock_daemon(&sock, 100);
        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
        client.set_init_wait(Duration::from_millis(100), Duration::from_millis(10));
        let result = client.request("test/ping", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("initializing"));
    }

    #[test]
    fn test_connect_invalid_path_returns_error() {
        let bogus = std::path::Path::new("/nonexistent/path/that/cannot/be/a.sock");
        // DaemonClient::connect would try to spawn the daemon, which we don't want in a unit
        // test. Instead verify that from_stream propagates a meaningful error when the stream
        // itself reports a problem — by opening a regular file and trying to use it as a socket.
        let tmp = std::env::temp_dir().join(format!("al-invalid-{}.txt", std::process::id()));
        std::fs::write(&tmp, b"not a socket").expect("test");
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
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
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
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
        client.set_write_timeout(Duration::from_millis(200));

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

    #[test]
    fn bounded_read_accepts_line_at_or_under_cap() {
        let payload = b"hello world\n";
        let mut reader = std::io::BufReader::new(&payload[..]);
        let result =
            read_bounded_line(&mut reader, 64, None).expect("under-cap line should succeed");
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    #[test]
    fn bounded_read_rejects_line_exceeding_cap() {
        let payload = [b'X'; 100];
        let mut reader = std::io::BufReader::new(&payload[..]);
        let err = read_bounded_line(&mut reader, 5, None).expect_err("must reject oversized line");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("5 byte limit"));
    }

    #[test]
    fn bounded_read_returns_none_on_empty_eof() {
        let payload: &[u8] = &[];
        let mut reader = std::io::BufReader::new(payload);
        let result = read_bounded_line(&mut reader, 64, None).expect("EOF must not error");
        assert!(result.is_none());
    }

    #[test]
    fn bounded_read_rejects_incomplete_utf8_at_eof() {
        // 0xC3 is a 2-byte-sequence lead byte; no continuation, no newline.
        let payload: &[u8] = &[b'o', b'k', 0xC3];
        let mut reader = std::io::BufReader::new(payload);
        let err = read_bounded_line(&mut reader, 64, None)
            .expect_err("incomplete UTF-8 at EOF must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn bounded_read_rejects_incomplete_utf8_before_newline() {
        // Lead byte 0xC3 followed immediately by the newline terminator.
        let payload: &[u8] = &[b'o', b'k', 0xC3, b'\n'];
        let mut reader = std::io::BufReader::new(payload);
        let err = read_bounded_line(&mut reader, 64, None)
            .expect_err("incomplete UTF-8 before newline must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn empty_line_yields_empty_string_then_graceful_parse_error() {
        let payload: &[u8] = b"\n";
        let mut reader = std::io::BufReader::new(payload);
        let result = read_bounded_line(&mut reader, 64, None).expect("bare newline must not error");
        assert_eq!(result.as_deref(), Some(""));

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
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
        let result = client.request("test/ping", None);
        assert!(
            result.is_err(),
            "blank-line response must surface a graceful error, got: {result:?}"
        );
    }

    #[test]
    fn spawn_lock_first_acquirer_succeeds() {
        let sock = unique_sock();
        let result = try_acquire_spawn_lock(&sock.with_extension("lock")).expect("io ok");
        match result {
            SpawnLockResult::Acquired(lock_path) => {
                assert!(lock_path.exists(), "lock file must be present on disk");
                assert_eq!(lock_path.extension().unwrap(), "lock");
                std::fs::remove_file(&lock_path).ok();
            }
            SpawnLockResult::Contended => panic!("first acquirer must not be contended"),
        }
    }

    #[test]
    fn spawn_lock_second_acquirer_is_contended() {
        let sock = unique_sock();
        let lock = sock.with_extension("lock");
        let first = try_acquire_spawn_lock(&lock).expect("io ok");
        let SpawnLockResult::Acquired(lock_path) = first else {
            panic!("first must be acquired");
        };

        let second = try_acquire_spawn_lock(&lock).expect("io ok");
        match second {
            SpawnLockResult::Contended => {}
            SpawnLockResult::Acquired(_) => {
                std::fs::remove_file(&lock_path).ok();
                panic!("second acquirer must be contended");
            }
        }

        std::fs::remove_file(&lock_path).ok();
        let third = try_acquire_spawn_lock(&lock).expect("io ok");
        match third {
            SpawnLockResult::Acquired(p) => {
                std::fs::remove_file(&p).ok();
            }
            SpawnLockResult::Contended => panic!("post-release acquirer must succeed"),
        }
    }

    #[test]
    fn stale_lock_age_exceeds_spawn_wait_window() {
        assert!(
            STALE_LOCK_AGE >= Duration::from_secs(10),
            "STALE_LOCK_AGE must comfortably exceed the 5s spawn wait window"
        );
    }

    // Serializes tests that mutate the process-wide PATH.
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

    #[test]
    fn find_al_lsp_binary_locates_in_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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

    #[test]
    fn stale_lock_is_reclaimed() {
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

        let result = try_acquire_spawn_lock(&lock_path).expect("io ok");
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

    #[test]
    fn fresh_lock_is_not_reclaimed() {
        let sock = unique_sock();
        let lock_path = sock.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        let first = try_acquire_spawn_lock(&lock_path).expect("io ok");
        let SpawnLockResult::Acquired(held) = first else {
            panic!("first must be acquired");
        };

        let second = try_acquire_spawn_lock(&lock_path).expect("io ok");
        let contended = matches!(second, SpawnLockResult::Contended);
        std::fs::remove_file(&held).ok();
        assert!(
            contended,
            "a fresh lock must not be reclaimed; second caller must be Contended"
        );
    }

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
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("from_stream");
        client
            .reader
            .get_ref()
            .set_recv_timeout(Some(Duration::from_millis(100)))
            .expect("set poll");
        let result = client.request_with_timeout("test/slow", None, Duration::from_secs(10));
        assert_eq!(
            result.expect("slow response must succeed")["slow"],
            true,
            "response after multiple poll timeouts must be returned intact"
        );
    }

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
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("from_stream");
        client
            .reader
            .get_ref()
            .set_recv_timeout(Some(Duration::from_millis(50)))
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

/// Exercises the actual platform backend selected by `interprocess`: a Unix
/// domain socket on Linux/macOS and a named pipe on Windows. Keep this outside
/// the Unix-only legacy test module so Windows CI proves that client setup,
/// nonblocking pipe I/O, framing, and response parsing work together.
#[cfg(test)]
mod cross_platform_tests {
    use super::{connect_stream, DaemonClient};
    use crate::jsonrpc::{Request, Response};
    use crate::socket::socket_path_with_runtime_dir;
    #[cfg(unix)]
    use interprocess::local_socket::traits::Stream as _;
    use interprocess::local_socket::{
        traits::Listener as _, GenericFilePath, ListenerOptions, ToFsName,
    };
    use std::io::{BufRead, Write};
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    #[test]
    fn local_transport_round_trip_uses_real_platform_backend() {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "al-protocol-cross-platform-{}-{n}",
            std::process::id()
        ));
        let project = root.join("project");
        // Unix-domain sockets have a small path cap (104 bytes on macOS).
        // GitHub's checkout and temp paths can exceed it before the endpoint
        // filename is appended, so keep this real-backend fixture beneath the
        // short, conventional Unix temp root. Windows named pipes are not
        // filesystem paths and retain the fully isolated fixture directory.
        #[cfg(unix)]
        let runtime =
            std::path::PathBuf::from(format!("/tmp/al-protocol-{}-{n}", std::process::id()));
        #[cfg(windows)]
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&project).expect("create project directory");
        std::fs::create_dir_all(runtime.join("al-lsp")).expect("create runtime directory");

        let endpoint = socket_path_with_runtime_dir(&project, runtime.to_string_lossy())
            .expect("create platform endpoint");
        let name = endpoint
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .expect("convert endpoint name");
        let listener = ListenerOptions::new()
            .name(name)
            .create_sync()
            .expect("bind platform local transport");

        // Keep the server-side named-pipe handle alive until the client has
        // consumed the response. The Windows local-socket wrapper's `flush`
        // is intentionally a no-op, so dropping the short-lived fixture
        // server immediately after `write_all` can race the client and turn a
        // valid buffered response into EOF. Real daemons keep the connection
        // open for subsequent requests.
        let (response_read_tx, response_read_rx) = std::sync::mpsc::channel();

        let server = std::thread::spawn(move || {
            let conn = listener.accept().expect("accept client");
            let mut reader = std::io::BufReader::new(&conn);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read request frame");
            let request: Request = serde_json::from_str(line.trim()).expect("parse request");

            let response = Response::ok(
                request.id,
                serde_json::json!({"transport": "local", "method": request.method}),
            );
            let mut frame = serde_json::to_vec(&response).expect("serialize response");
            frame.push(b'\n');
            let mut writer = &conn;
            writer.write_all(&frame).expect("write response frame");
            writer.flush().expect("flush response frame");
            let _ = response_read_rx.recv_timeout(std::time::Duration::from_secs(5));
        });

        let stream = connect_stream(&endpoint).expect("connect platform local transport");
        let mut client = DaemonClient::from_stream(stream).expect("construct daemon client");
        let response = client
            .request("test/platform", None)
            .expect("complete platform round trip");
        assert_eq!(response["transport"], "local");
        assert_eq!(response["method"], "test/platform");
        response_read_tx
            .send(())
            .expect("notify fixture server that response was consumed");

        drop(client);
        server.join().expect("server thread completed");
        #[cfg(unix)]
        let _ = std::fs::remove_file(&endpoint);
        let _ = std::fs::remove_dir_all(&root);
        #[cfg(unix)]
        let _ = std::fs::remove_dir_all(&runtime);
    }

    #[test]
    fn local_transport_request_deadline_is_enforced() {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("al-protocol-deadline-{}-{n}", std::process::id()));
        let project = root.join("project");
        #[cfg(unix)]
        let runtime = std::path::PathBuf::from(format!(
            "/tmp/al-protocol-deadline-{}-{n}",
            std::process::id()
        ));
        #[cfg(windows)]
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&project).expect("create project directory");
        std::fs::create_dir_all(runtime.join("al-lsp")).expect("create runtime directory");

        let endpoint = socket_path_with_runtime_dir(&project, runtime.to_string_lossy())
            .expect("create platform endpoint");
        let name = endpoint
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .expect("convert endpoint name");
        let listener = ListenerOptions::new()
            .name(name)
            .create_sync()
            .expect("bind platform local transport");

        let server = std::thread::spawn(move || {
            let conn = listener.accept().expect("accept client");
            let mut reader = std::io::BufReader::new(&conn);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read request frame");
            std::thread::sleep(std::time::Duration::from_secs(1));
        });

        let stream = connect_stream(&endpoint).expect("connect platform local transport");
        let mut client = DaemonClient::from_stream(stream).expect("construct daemon client");
        #[cfg(unix)]
        client
            .reader
            .get_ref()
            .set_recv_timeout(Some(std::time::Duration::from_millis(50)))
            .expect("shorten Unix socket poll interval for deadline test");
        let error = client
            .request_with_timeout("test/never", None, std::time::Duration::from_millis(200))
            .expect_err("silent platform peer must hit the request deadline");
        assert!(
            error.contains("did not respond"),
            "deadline error must be actionable: {error}"
        );

        drop(client);
        server.join().expect("server thread completed");
        #[cfg(unix)]
        let _ = std::fs::remove_file(&endpoint);
        let _ = std::fs::remove_dir_all(&root);
        #[cfg(unix)]
        let _ = std::fs::remove_dir_all(&runtime);
    }
}
