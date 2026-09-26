//! Synchronous local IPC client for the al-lsp daemon.
//!
//! Connects to the daemon, auto-starts it if not running, and provides
//! JSON-RPC request/response with retry on "initializing" errors.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(windows)]
use interprocess::local_socket::GenericFilePath;
use interprocess::local_socket::{prelude::*, Stream};
use interprocess::TryClone;

use crate::identity::{self, BuildIdentity};
use crate::jsonrpc::{Request, Response};
use crate::socket::{socket_path, spawn_lock_path};

const INIT_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Default total time to keep retrying "Workspace is initializing"
/// responses. Cold daemon startup on a real project loads symbol
/// packages, which can take several seconds.
const INIT_WAIT_TOTAL: Duration = Duration::from_secs(60);
/// Default per-request response deadline. Individual commands override
/// this via [`DaemonClient::set_request_timeout`] for long operations
/// (symbol downloads, compiles, test runs), and `AL_REQUEST_TIMEOUT_MS`
/// overrides it for a whole process.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on progress-aware waiting. A request that waits past this gives up
/// even while the dependency source index is still advancing.
const MAX_INDEX_WAIT: Duration = Duration::from_secs(600);
/// Deadline for the `handshake` call that checks which build a daemon is.
/// It reads two constants, so anything slower than this is a wedged daemon,
/// and treating that as a mismatch replaces it.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Deadline for the `shutdown` that precedes a replacement.
const SHUTDOWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for a stopping daemon's endpoint to stop accepting.
/// The daemon drains in-flight connections for up to 10 s before it closes.
const ENDPOINT_CLOSE_WAIT: Duration = Duration::from_secs(20);
/// How often to retry the connect that proves the endpoint is closed.
const ENDPOINT_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// How long `<al-lsp> --version` gets to answer. It prints two constants, so
/// anything slower is not the binary this client is looking for.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the daemon's dependency source index is doing, as seen by a client
/// whose own request has reached its deadline.
struct IndexProgress {
    building: bool,
    packages_done: usize,
    packages_total: usize,
    files_done: usize,
    elapsed_ms: u64,
}

/// Whether an error came from the response deadline rather than a broken
/// connection. Matches the text [`DaemonClient::read_one_response`] produces.
fn is_timeout_message(error: &str) -> bool {
    error.starts_with("Daemon did not respond within")
}

/// The per-request deadline for this process: `AL_REQUEST_TIMEOUT_MS` when it
/// parses as a positive integer, otherwise [`DEFAULT_REQUEST_TIMEOUT`].
fn configured_request_timeout() -> Duration {
    std::env::var("AL_REQUEST_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT)
}
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
/// `read_bounded_line` in `al_lsp::server::daemon`.
///
/// `deadline`: socket-level read timeouts (`WouldBlock`/`TimedOut`) are
/// retried until this instant, preserving any partially-read line bytes.
/// `None` means a single socket timeout is fatal (legacy behaviour, used
/// by tests).
///
/// `carry` holds the bytes taken from the reader so far. It belongs to the
/// caller, not to this function, because the bytes are already consumed from
/// the `BufReader`: dropping them on a deadline that expires mid-frame (a
/// multi-megabyte `workspace/symbol` or `al_build` result still streaming)
/// leaves the rest of that JSON line in the socket, so the next request reads
/// a truncated fragment and reports a parse error for a frame that was well
/// formed. On a complete frame `carry` is left empty.
#[cfg(not(windows))]
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    carry: &mut Vec<u8>,
    max_bytes: usize,
    deadline: Option<std::time::Instant>,
) -> std::io::Result<Option<String>> {
    let buf = carry;
    loop {
        let available = match reader.fill_buf() {
            Ok(a) => a,
            // A signal arriving mid-read (SIGCHLD, SIGWINCH, a debugger
            // attaching) is not the daemon going away. `fill_buf` does not
            // retry it the way `read_line` does, so a resize of the terminal
            // failed a long request with "Interrupted system call".
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
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
                String::from_utf8(std::mem::take(buf))
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
            return String::from_utf8(std::mem::take(buf))
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
    carry: &mut Vec<u8>,
    max_bytes: usize,
    deadline: std::time::Instant,
) -> std::io::Result<Option<String>> {
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let buf = carry;
    loop {
        while reader.buffer().is_empty() {
            let Stream::NamedPipe(pipe) = reader.get_ref();
            let mut available = 0_u32;
            // SAFETY: the handle belongs to the pipe `reader` borrows for this
            // call. The null buffer, length 0 and null out pointers are the
            // documented "peek the byte count only" form, and `available` is a
            // live u32 for the call to write.
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
            return String::from_utf8(std::mem::take(buf))
                .map(Some)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
        let len = available.len();
        buf.extend_from_slice(available);
        reader.consume(len);
    }
}

/// A frame write that did not complete.
///
/// `wrote_any` distinguishes a frame the daemon never saw from one it saw the
/// start of. In the second case the connection carries half a JSON object and
/// the next request would append a second one to the same line, which the
/// daemon reads as one corrupt frame.
struct FrameWriteError {
    error: std::io::Error,
    wrote_any: bool,
}

/// Write a complete frame without allowing a non-reading daemon to block the
/// caller forever. Windows named pipes use nonblocking mode; Unix sockets use
/// their OS send timeout, with this deadline as a platform-independent guard.
fn write_all_bounded<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    timeout: Duration,
) -> Result<(), FrameWriteError> {
    let deadline = std::time::Instant::now() + timeout;
    let total = bytes.len();
    let mut bytes = bytes;
    let fail = move |error: std::io::Error, remaining: usize| FrameWriteError {
        error,
        wrote_any: remaining != total,
    };
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => {
                return Err(fail(
                    std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "failed to write complete daemon request",
                    ),
                    bytes.len(),
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
            Err(e) => {
                let remaining = bytes.len();
                return Err(fail(e, remaining));
            }
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
    /// Request ids whose response deadline expired while the daemon was still
    /// working. The daemon eventually writes those frames, so the next read
    /// must drain them instead of mistaking one for the current request's
    /// answer ("Response ID mismatch" on every subsequent call).
    abandoned_ids: std::collections::HashSet<u64>,
    /// Bytes of a response frame already taken from the socket but not yet
    /// terminated by a newline. Carried across `request` calls so a deadline
    /// that expires mid-frame does not truncate the frame for the next reader.
    partial_frame: Vec<u8>,
    /// Why this connection can no longer be used, once a half-written request
    /// frame has reached the daemon. Nothing can undo that, so every later
    /// request fails with this reason instead of a misleading parse error.
    desynced: Option<String>,
    /// The project this client is connected to, when it is known.
    ///
    /// A second connection to the same daemon is how a request that has hit
    /// its deadline asks whether the dependency source index is still
    /// building. Without it a timeout carries no reason and a retry walks into
    /// the next one.
    project_root: Option<PathBuf>,
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
        Self::from_stream(stream).map(|client| client.with_project_root(project_root))
    }

    /// Connect to the daemon for a project, auto-starting if needed.
    ///
    /// concurrent first-time callers are serialised via a per-socket
    /// `.lock` file so only one process spawns `al-lsp daemon`. Losers wait
    /// for the winner's socket to appear, then connect normally.
    ///
    /// A daemon that was already running is asked which build it came from. A
    /// daemon from another build is stopped and replaced, because it answers
    /// with the response shapes of whatever code it was started from.
    pub fn connect(project_root: &Path) -> Result<Self, String> {
        let endpoint = socket_path(project_root)
            .ok_or_else(|| "Cannot determine a local daemon endpoint: no per-user runtime directory is available".to_string())?;
        let lock_path = spawn_lock_path(project_root)
            .ok_or_else(|| "Cannot determine a daemon startup lock path".to_string())?;

        Self::connect_checked(
            project_root,
            &endpoint,
            &lock_path,
            expected_identity().as_ref(),
            &mut |project_root, endpoint| {
                Self::start_daemon(project_root)
                    .and_then(|mut child| Self::wait_for_daemon(endpoint, Some(&mut child)))
            },
        )
    }

    /// Connect, then replace the daemon if it came from a different build.
    ///
    /// `expected` is the identity a daemon must report to be used, or `None`
    /// when this client cannot identify its own build. `spawn` starts a daemon
    /// from the binary beside this executable and returns a connection to it.
    /// Both are parameters so the replace path can be tested against a fake
    /// daemon rather than two builds of al-lsp.
    fn connect_checked(
        project_root: &Path,
        endpoint: &Path,
        lock_path: &Path,
        expected: Option<&BuildIdentity>,
        spawn: &mut dyn FnMut(&Path, &Path) -> Result<Stream, String>,
    ) -> Result<Self, String> {
        let mut client = Self::connect_or_spawn(project_root, endpoint, lock_path, spawn)?;

        let Some(expected) = expected.cloned() else {
            return Ok(client);
        };
        let actual = client.daemon_identity();
        if actual.as_ref().is_ok_and(|actual| *actual == expected) {
            return Ok(client);
        }
        let reported = match &actual {
            Ok(actual) => actual.to_string(),
            Err(error) => format!("no identity ({error})"),
        };
        if identity::mismatch_allowed() {
            notify(&format!(
                "the running daemon was built from other code ({reported}, this client expects \
                 {expected}); using it anyway because {} is set",
                identity::ALLOW_MISMATCH_ENV
            ));
            return Ok(client);
        }

        notify(&format!(
            "the running daemon was built from other code ({reported}, this client expects \
             {expected}); stopping it and starting a matching one"
        ));
        client.request_daemon_shutdown();
        drop(client);
        if !wait_for_endpoint_closed(endpoint, ENDPOINT_CLOSE_WAIT) {
            return Err(format!(
                "The daemon for {} was built from other code ({reported}, this client expects \
                 {expected}) and did not stop within {}s. Stop it by hand (`al-explorer \
                 daemon-shutdown`, or kill the `al-lsp daemon` process for this project), or set \
                 {} to use it as it is.",
                project_root.display(),
                ENDPOINT_CLOSE_WAIT.as_secs(),
                identity::ALLOW_MISMATCH_ENV
            ));
        }

        let stream = spawn(project_root, endpoint)?;
        let mut client = Self::from_stream(stream)?.with_project_root(project_root);
        // One replacement, never a loop. A second mismatch means the binary
        // beside this executable is not the one answering on this endpoint,
        // and restarting again would not change that.
        match client.daemon_identity() {
            Ok(actual) if actual == expected => {}
            other => notify(&format!(
                "the replacement daemon still reports a different build ({other:?}, this client \
                 expects {expected}); continuing with it"
            )),
        }
        Ok(client)
    }

    /// Connect to a running daemon, or serialise with other callers and start
    /// one. No identity check: [`Self::connect_checked`] adds that.
    fn connect_or_spawn(
        project_root: &Path,
        endpoint: &Path,
        lock_path: &Path,
        spawn: &mut dyn FnMut(&Path, &Path) -> Result<Stream, String>,
    ) -> Result<Self, String> {
        if let Ok(stream) = connect_stream(endpoint) {
            return Self::from_stream(stream).map(|client| client.with_project_root(project_root));
        }

        let result = match try_acquire_spawn_lock(lock_path)
            .map_err(|e| format!("Cannot acquire daemon spawn lock: {}", e))?
        {
            SpawnLockResult::Acquired(lock_path) => {
                // Re-check inside the lock — a concurrent winner may have
                // just finished spawning while we were acquiring.
                let result = if let Ok(stream) = connect_stream(endpoint) {
                    Self::from_stream(stream)
                } else {
                    spawn(project_root, endpoint).and_then(Self::from_stream)
                };
                let _ = std::fs::remove_file(&lock_path);
                result
            }
            SpawnLockResult::Contended => {
                let stream = Self::wait_for_daemon(endpoint, None)?;
                Self::from_stream(stream)
            }
        };
        result.map(|client| client.with_project_root(project_root))
    }

    /// Ask the daemon which build it came from.
    ///
    /// A daemon too old to know `handshake` answers "Unknown method", which is
    /// itself the answer the caller needs: it predates this check.
    fn daemon_identity(&mut self) -> Result<BuildIdentity, String> {
        let challenge = identity::nonce();
        let params =
            (!challenge.is_empty()).then(|| serde_json::json!({ "nonce": challenge.clone() }));
        let value = self.request_with_timeout("handshake", params, HANDSHAKE_TIMEOUT)?;
        let identity: BuildIdentity = serde_json::from_value(value.clone())
            .map_err(|error| format!("handshake did not carry a build identity: {error}"))?;
        self.verify_handshake_proof(&challenge, &identity, &value)?;
        Ok(identity)
    }

    /// Check that the daemon knows the per-user secret, not just what the
    /// identity looks like.
    ///
    /// Everything the identity is made of is world-readable, so a process on
    /// the endpoint can answer with whatever the client expects. The proof is
    /// an HMAC over the nonce and the identity, keyed by a file only this user
    /// can read.
    ///
    /// No secret and no nonce means no challenge to make. A daemon too old to
    /// answer one reports no proof, and that is treated as a build mismatch
    /// rather than as an authentication failure: the replace path stops it and
    /// starts one from this binary, which is what a stale daemon needs anyway.
    fn verify_handshake_proof(
        &self,
        challenge: &str,
        identity: &BuildIdentity,
        answer: &serde_json::Value,
    ) -> Result<(), String> {
        if challenge.is_empty() {
            return Ok(());
        }
        let Some(secret) = handshake_secret() else {
            return Ok(());
        };
        let expected = identity::proof(&secret, challenge, identity);
        let actual = answer.get("proof").and_then(serde_json::Value::as_str);
        match actual {
            Some(actual) if identity::proofs_match(&expected, actual) => Ok(()),
            Some(_) => Err(
                "the handshake proof did not match: whatever is answering on this project's \
                 daemon endpoint cannot read this user's runtime directory"
                    .to_string(),
            ),
            None => Err("the daemon answered the handshake without a proof".to_string()),
        }
    }

    /// Best-effort stop, used before replacing a daemon. The wait for the
    /// endpoint to close is what decides whether it worked.
    fn request_daemon_shutdown(&mut self) {
        if let Err(error) = self.request_with_timeout("shutdown", None, SHUTDOWN_REQUEST_TIMEOUT) {
            tracing::debug!(%error, "daemon did not acknowledge the shutdown request");
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
            request_timeout: configured_request_timeout(),
            write_timeout: WRITE_TIMEOUT,
            init_wait_total: INIT_WAIT_TOTAL,
            init_retry_delay: INIT_RETRY_DELAY,
            abandoned_ids: std::collections::HashSet::new(),
            partial_frame: Vec::new(),
            desynced: None,
            project_root: None,
        })
    }

    /// Record which project this connection belongs to, so a request that
    /// reaches its deadline can ask the daemon whether it is still indexing.
    fn with_project_root(mut self, project_root: &Path) -> Self {
        self.project_root = Some(project_root.to_path_buf());
        self
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
    /// retries every `init_retry_delay` up to a total of
    /// `init_wait_total` — cold daemon startup on a real project
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
        if let Some(reason) = &self.desynced {
            return Err(format!(
                "Daemon connection is out of sync ({reason}); reconnect before sending more \
                 requests"
            ));
        }
        let init_deadline = std::time::Instant::now() + self.init_wait_total;
        let mut expected_id = self.send_request(method, &params)?;
        let started = std::time::Instant::now();
        let mut last_files_done = 0usize;

        loop {
            let response = match self.read_response(timeout) {
                Ok(response) => response,
                Err(error) if is_timeout_message(&error) => {
                    // The deadline is not evidence that the daemon is stuck.
                    // On a fresh project it is usually the dependency AL
                    // source index, which takes about a minute, and retrying
                    // into the next deadline was the whole first-minute
                    // experience. Ask a second connection what the daemon is
                    // doing and keep waiting while it makes progress.
                    match self.index_progress() {
                        Some(progress)
                            if progress.building
                                && started.elapsed() < MAX_INDEX_WAIT
                                && progress.files_done >= last_files_done =>
                        {
                            last_files_done = progress.files_done;
                            continue;
                        }
                        Some(progress) if progress.building => {
                            self.abandoned_ids.insert(expected_id);
                            return Err(format!(
                                "{method} is waiting on the dependency source index, which is \
                                 still building ({} of {} packages, {} files, {} s elapsed). \
                                 Call `status` to watch it, or raise AL_REQUEST_TIMEOUT_MS.",
                                progress.packages_done,
                                progress.packages_total,
                                progress.files_done,
                                progress.elapsed_ms / 1000
                            ));
                        }
                        _ => {
                            self.abandoned_ids.insert(expected_id);
                            return Err(error);
                        }
                    }
                }
                Err(error) => {
                    // The daemon may still be working and will eventually write
                    // this frame. Remember the id so the next request drains it
                    // instead of reading it as its own (skewed) answer.
                    self.abandoned_ids.insert(expected_id);
                    return Err(error);
                }
            };

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

    /// Ask the daemon, on a second connection, how far the dependency source
    /// index has got. `None` when there is no project root, no daemon to ask,
    /// or the answer does not carry `sourceIndex`.
    fn index_progress(&self) -> Option<IndexProgress> {
        let project_root = self.project_root.as_ref()?;
        let mut probe = Self::connect_existing(project_root).ok()?;
        // Short deadline: `status` touches no index and answers in milliseconds
        // even while a build holds the index write lock.
        probe.request_timeout = Duration::from_secs(5);
        let status = probe.request("status", None).ok()?;
        let source_index = status.get("sourceIndex")?;
        let number = |key: &str| {
            source_index
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        Some(IndexProgress {
            building: source_index.get("state").and_then(|v| v.as_str()) == Some("building"),
            packages_done: number("packagesDone") as usize,
            packages_total: number("packagesTotal") as usize,
            files_done: number("filesDone") as usize,
            elapsed_ms: number("elapsedMs"),
        })
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

        if let Err(failure) =
            write_all_bounded(&mut self.writer, json.as_bytes(), self.write_timeout)
        {
            if failure.wrote_any {
                self.desynced = Some(format!("a request frame was half sent: {}", failure.error));
            }
            return Err(format!("Failed to send request: {}", failure.error));
        }
        if let Err(error) = self.writer.flush() {
            self.desynced = Some(format!("a request frame may be half sent: {error}"));
            return Err(format!("Failed to flush: {}", error));
        }
        Ok(id)
    }

    /// Read the next response frame, discarding late answers to requests whose
    /// deadline already expired.
    ///
    /// Without this drain a single timed-out request permanently skews the
    /// connection: the abandoned response is still buffered, so the next
    /// `request` reads it and fails with "Response ID mismatch", and so does
    /// every request after it.
    fn read_response(&mut self, timeout: Duration) -> Result<Response, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let response = self.read_one_response(timeout, deadline)?;
            if self.abandoned_ids.remove(&response.id) {
                continue;
            }
            return Ok(response);
        }
    }

    fn read_one_response(
        &mut self,
        timeout: Duration,
        deadline: std::time::Instant,
    ) -> Result<Response, String> {
        #[cfg(not(windows))]
        let line = read_bounded_line(
            &mut self.reader,
            &mut self.partial_frame,
            MAX_RESPONSE_LINE,
            Some(deadline),
        )
        .map_err(|e| {
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) {
                format!(
                    "Daemon did not respond within {} — the operation may still be \
                         running. Raise AL_REQUEST_TIMEOUT_MS or pass --timeout-ms, or \
                         check the daemon log at ~/.local/share/al-lsp/logs/al-lsp.log",
                    describe_timeout(timeout)
                )
            } else {
                format!("Failed to read response: {}", e)
            }
        })?;
        #[cfg(windows)]
        let line = read_bounded_pipe_line(
            &mut self.reader,
            &mut self.partial_frame,
            MAX_RESPONSE_LINE,
            deadline,
        )
        .map_err(|e| {
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) {
                format!(
                    "Daemon did not respond within {} — the operation may still be \
                         running. Raise AL_REQUEST_TIMEOUT_MS or pass --timeout-ms, or \
                         check the daemon log at ~/.local/share/al-lsp/logs/al-lsp.log",
                    describe_timeout(timeout)
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
        // Windows hands every inheritable handle to a child, and this
        // process's own standard handles are inheritable when a caller
        // captures them through pipes. The daemon outlives this process, so it
        // held the caller's pipe open and `al-explorer diag` run with captured
        // output did not finish until the daemon exited, up to 30 minutes
        // later.
        #[cfg(windows)]
        let _std_handles = windows_std_handles::NotInherited::new();
        std::process::Command::new(&al_lsp)
            .arg("daemon")
            .arg("--project")
            .arg(project_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            // Keep the startup error. Discarding it left the caller with an
            // exit status and an instruction to read a log file, which an agent
            // cannot act on.
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start al-lsp daemon: {}", e))
    }

    /// The last non-empty line the daemon wrote to stderr before exiting.
    ///
    /// Reads at most `MAX_STARTUP_STDERR` bytes: the pipe is already closed
    /// when the child has exited, so this returns immediately.
    fn startup_stderr(child: &mut std::process::Child) -> Option<String> {
        const MAX_STARTUP_STDERR: u64 = 64 * 1024;
        use std::io::Read;
        let mut captured = String::new();
        child
            .stderr
            .take()?
            .take(MAX_STARTUP_STDERR)
            .read_to_string(&mut captured)
            .ok()?;
        captured
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(startup_error_message)
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
                    return Err(match Self::startup_stderr(child) {
                        Some(reason) => format!(
                            "al-lsp daemon exited before opening its endpoint ({status}): {reason}"
                        ),
                        None => format!(
                            "al-lsp daemon exited before opening its endpoint ({status}); \
                             inspect ~/.local/share/al-lsp/logs/al-lsp.log for the startup error"
                        ),
                    });
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

/// Tell the person running the command something about the daemon it is about
/// to use.
///
/// These notices explain a command that restarted a daemon or reached for a
/// binary the caller did not expect, so they have to be seen. al-explorer is
/// the only client of this module and installs no tracing subscriber, so the
/// `tracing` event alone would reach nobody; it stays for anything that does
/// install one. Stdout belongs to the command's own output.
fn notify(message: &str) {
    tracing::warn!("{message}");
    eprintln!("al-lsp: {message}");
}

/// The identity this client expects a daemon to report.
///
/// `None` when the build is identified by the `al-lsp` executable and that
/// executable cannot be found: there is then nothing to compare against, and
/// refusing to talk to a running daemon over that would be worse than using it.
fn expected_identity() -> Option<BuildIdentity> {
    if !identity::needs_binary() {
        return Some(identity::identity_for(Path::new("")));
    }
    match find_al_lsp_binary() {
        Ok(binary) => Some(identity::identity_for(&binary)),
        Err(error) => {
            tracing::debug!(%error, "cannot identify this build; skipping the daemon check");
            None
        }
    }
}

/// Wait until nothing answers on `endpoint`, or `timeout` elapses.
///
/// Returns whether the endpoint closed. A successful connect is the only
/// reliable proof that a daemon is still serving: on Unix the socket file
/// outlives a killed daemon, and it is removed a moment after the listener
/// stops in an orderly one. Callers that must know the old daemon is gone
/// before they start a new one use this rather than sleeping.
pub fn wait_for_endpoint_closed(endpoint: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if connect_stream(endpoint).is_err() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(ENDPOINT_POLL_INTERVAL);
    }
}

/// Locate the `al-lsp` this client should start.
///
/// The binary beside this executable wins: a release archive and a `target/`
/// directory both hold the pair, and taking the sibling keeps the client and
/// the daemon on one build. Falling back to PATH is what let a client start a
/// daemon weeks older than itself, silently, so a PATH binary is reported at
/// warn level and refused when its version differs from this build's.
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
    let on_path = std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(&binary_name))
            .find(|candidate| candidate.is_file())
    });
    let Some(candidate) = on_path else {
        return Err(format!(
            "Cannot find al-lsp. It is normally installed beside al-explorer; this executable \
             has no al-lsp next to it and there is none on PATH. Install the pair with \
             `cargo install --path crates/al-explorer` and `cargo install --path crates/al-lsp \
             --bin al-lsp --features semantic`, or unpack a release archive and put its \
             directory on PATH. This client is version {}.",
            identity::DAEMON_VERSION
        ));
    };

    let version = binary_version(&candidate);
    let reported = version.as_deref().unwrap_or("an unknown version");
    if version.as_deref() == Some(identity::DAEMON_VERSION) {
        notify(&format!(
            "no al-lsp beside this executable; starting {} ({reported}) from PATH",
            candidate.display()
        ));
        return Ok(candidate);
    }
    if identity::mismatch_allowed() {
        notify(&format!(
            "starting {} ({reported}) from PATH although this client is version {}, because {} \
             is set",
            candidate.display(),
            identity::DAEMON_VERSION,
            identity::ALLOW_MISMATCH_ENV
        ));
        return Ok(candidate);
    }
    Err(format!(
        "The only al-lsp available is {} ({reported}), and this client is version {}. It is on \
         PATH rather than beside this executable, so the two were installed separately and a \
         daemon started from it would answer with that version's behaviour. Install a matching \
         one with `cargo install --path crates/al-lsp --bin al-lsp --features semantic`, or \
         unpack the release archive for {} so al-lsp and al-explorer sit in one directory. Set \
         {} to use it as it is.",
        candidate.display(),
        identity::DAEMON_VERSION,
        identity::DAEMON_VERSION,
        identity::ALLOW_MISMATCH_ENV
    ))
}

/// The version `<binary> --version` reports, from output shaped
/// `al-lsp <version> (<build>)`.
///
/// `None` when the binary cannot be run, does not answer within
/// [`VERSION_PROBE_TIMEOUT`], or answers in some other shape — all of which
/// mean the same thing here: it is not a binary from this build.
fn binary_version(binary: &Path) -> Option<String> {
    let mut child = std::process::Command::new(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // An unknown binary gets a deadline rather than a blocking read: whatever
    // is named `al-lsp` on PATH need not be this program at all.
    let deadline = std::time::Instant::now() + VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }

    let output = child.wait_with_output().ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut fields = stdout.split_whitespace();
    if fields.next()? != "al-lsp" {
        return None;
    }
    fields.next().map(str::to_string)
}

/// The per-user handshake secret, created on first use.
///
/// `None` when there is no runtime directory to keep it in, or the directory
/// is one this user does not own. Both are reasons not to make a challenge
/// rather than reasons to refuse the daemon: the peer check already decided
/// who may answer.
pub fn handshake_secret() -> Option<Vec<u8>> {
    let dir = crate::socket::runtime_al_lsp_dir()?;
    #[cfg(unix)]
    crate::endpoint::ensure_private_dir(&dir).ok()?;
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir).ok()?;
    identity::shared_secret(&dir).ok()
}

/// Connect to the daemon endpoint, refusing anything that is not this user's.
///
/// The endpoint path is derived from the project path, so it is guessable, and
/// on Unix it can sit under a world-writable `/tmp`. Whoever binds it first
/// receives every request, and those requests carry Business Central
/// credentials. So the directory is checked the same way the daemon checks it
/// before creating the socket, the endpoint itself must be a socket and not a
/// symlink, and the peer's uid is read from the kernel before anything is sent.
///
/// See `crate::endpoint`.
#[cfg(unix)]
fn connect_stream(endpoint: &Path) -> std::io::Result<Stream> {
    crate::endpoint::check_before_connect(endpoint)?;
    let stream = std::os::unix::net::UnixStream::connect(endpoint)?;
    crate::endpoint::check_peer(&stream)?;
    Ok(interprocess::os::unix::uds_local_socket::Stream::from(stream).into())
}

/// Windows: the endpoint is a named pipe, and pipe names are a global
/// namespace where the first creator owns the name.
///
/// [UNVERIFIED] The server's own identity is not checked here. The equivalent
/// of the Unix peer check is `GetNamedPipeServerProcessId` plus a comparison of
/// that process's user SID, or creating the pipe with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` and a DACL and treating a pre-existing name
/// as hostile. Neither is written, and neither would be run on this machine.
/// See `Docs/current-limitations.md`.
#[cfg(windows)]
fn connect_stream(endpoint: &Path) -> std::io::Result<Stream> {
    let name = endpoint.to_fs_name::<GenericFilePath>()?;
    // `Stream::connect` uses an unbounded named-pipe wait on Windows. Under
    // concurrent CLI load every server instance can briefly be occupied, which
    // previously wedged callers before the request-level deadlines could apply.
    // Try once without waiting; daemon startup has its own bounded retry loop
    // in `wait_for_daemon`.
    interprocess::local_socket::ConnectOptions::new()
        .name(name)
        .wait_mode(interprocess::ConnectWaitMode::Timeout(Duration::ZERO))
        .connect_sync()
}

/// Clear the inherit flag on this process's standard handles for the length
/// of one spawn, and put back what was there afterwards.
#[cfg(windows)]
mod windows_std_handles {
    use windows_sys::Win32::Foundation::{
        GetHandleInformation, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    /// Held for the guard's lifetime: the flags are process-wide, and a second
    /// spawn that found them already cleared would record nothing to restore
    /// while the first one's restore put them back mid-spawn.
    static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(super) struct NotInherited {
        restore: Vec<HANDLE>,
        _serialized: std::sync::MutexGuard<'static, ()>,
    }

    impl NotInherited {
        pub(super) fn new() -> Self {
            let serialized = SPAWNING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut restore = Vec::new();
            for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                // Safety: these read and set flags on this process's own
                // standard handles, which stay open for its lifetime.
                unsafe {
                    let handle = GetStdHandle(which);
                    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                        continue;
                    }
                    let mut flags = 0u32;
                    if GetHandleInformation(handle, &mut flags) == 0
                        || flags & HANDLE_FLAG_INHERIT == 0
                    {
                        continue;
                    }
                    if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) != 0 {
                        restore.push(handle);
                    }
                }
            }
            Self {
                restore,
                _serialized: serialized,
            }
        }
    }

    impl Drop for NotInherited {
        fn drop(&mut self) {
            for handle in &self.restore {
                // Safety: see `new`; this restores the flag it cleared.
                unsafe {
                    SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
                }
            }
        }
    }
}

/// A request deadline for a message: `--timeout-ms 100` printed "within 0s".
fn describe_timeout(timeout: std::time::Duration) -> String {
    if timeout.as_millis() < 1000 {
        format!("{} ms", timeout.as_millis())
    } else if timeout.subsec_millis() == 0 {
        format!("{}s", timeout.as_secs())
    } else {
        format!("{:.1}s", timeout.as_secs_f64())
    }
}

/// The message in one line of the daemon's tracing output.
///
/// The daemon logs `<timestamp> <LEVEL> <message> error=<error>` with ANSI
/// colour codes. The client used to keep the text after the last `": "`, which
/// cut `Invalid app.json at <path>: missing field` down to `missing field` and
/// dropped the one thing the user needed: which file.
fn startup_error_message(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    if let Some((_, error)) = plain.split_once("error=") {
        return error.trim().to_string();
    }
    let mut rest = plain.trim();
    for _ in 0..2 {
        let Some((head, tail)) = rest.split_once(char::is_whitespace) else {
            break;
        };
        let is_prefix = head.starts_with(|c: char| c.is_ascii_digit())
            || matches!(head, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE");
        if !is_prefix {
            break;
        }
        rest = tail.trim_start();
    }
    rest.to_string()
}

#[cfg(all(test, unix))]
mod startup_error_tests {
    use super::startup_error_message;

    #[test]
    fn startup_error_keeps_the_path_in_a_structured_error() {
        let line = "\u{1b}[2m2026-09-24T05:23:43.774319Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m Daemon failed \u{1b}[3merror\u{1b}[0m\u{1b}[2m=\u{1b}[0mInvalid app.json at /work/schemas/app.json: missing field `id` at line 252 column 1";
        assert_eq!(
            startup_error_message(line),
            "Invalid app.json at /work/schemas/app.json: missing field `id` at line 252 column 1"
        );
    }

    #[test]
    fn startup_error_drops_timestamp_and_level_from_a_plain_message() {
        assert_eq!(
            startup_error_message("2026-09-24T05:23:43Z ERROR Daemon failed: no socket dir"),
            "Daemon failed: no socket dir"
        );
        assert_eq!(
            startup_error_message("thread 'main' panicked"),
            "thread 'main' panicked"
        );
    }

    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

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
                    Response::error(
                        req.dispatch_id(),
                        -32603,
                        "Workspace is initializing, try again",
                    )
                } else {
                    Response::ok(req.dispatch_id(), serde_json::json!({"status": "ok"}))
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

    /// The finding's scenario, as far as one uid can reach it: something else
    /// is sitting at the endpoint path when the client arrives. The client used
    /// to connect to whatever was there and send its request, credentials and
    /// all, so the check has to happen before the connect.
    #[test]
    fn a_planted_endpoint_is_refused_before_anything_is_sent() {
        let endpoint = unique_sock();
        std::fs::write(&endpoint, b"planted").expect("test");

        let error =
            connect_stream(&endpoint).expect_err("a planted regular file must not be connected to");

        let _ = std::fs::remove_file(&endpoint);
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("not a socket"), "{error}");
    }

    /// A symlink at the endpoint points the connection somewhere the path does
    /// not name, which is how a planted endpoint survives a check that reads
    /// the target rather than the entry.
    #[test]
    fn a_symlinked_endpoint_is_refused_before_anything_is_sent() {
        let real = unique_sock();
        let _listener = UnixListener::bind(&real).expect("test");
        let link = unique_sock();
        std::os::unix::fs::symlink(&real, &link).expect("test");

        let error = connect_stream(&link).expect_err("a symlinked endpoint must be refused");

        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&real);
        assert!(error.to_string().contains("symbolic link"), "{error}");
    }

    /// The daemon's own socket still connects, so the checks are not a blanket
    /// refusal.
    #[test]
    fn this_users_own_endpoint_still_connects() {
        let endpoint = unique_sock();
        let _listener = UnixListener::bind(&endpoint).expect("test");

        let result = connect_stream(&endpoint);

        let _ = std::fs::remove_file(&endpoint);
        assert!(result.is_ok(), "{:?}", result.err());
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

    /// A response whose request already timed out must be drained by id, not
    /// mistaken for the answer to the request that follows it. Without the
    /// drain the connection stays skewed forever ("Response ID mismatch").
    #[test]
    fn abandoned_response_is_drained_before_the_next_answer() {
        fn mock_stale_then_fresh(sock_path: &Path) -> (UnixListener, std::thread::JoinHandle<()>) {
            let listener = UnixListener::bind(sock_path).expect("test");
            let listener_clone = listener.try_clone().expect("test");
            let handle = std::thread::spawn(move || {
                let (stream, _) = listener_clone.accept().expect("test");
                let reader = std::io::BufReader::new(&stream);
                let mut writer = &stream;
                for line in reader.lines() {
                    let line = line.expect("test");
                    let req: Request = serde_json::from_str(&line).expect("parse");
                    // The late answer to the abandoned request arrives first.
                    for response in [
                        Response::ok(1, serde_json::json!({"stale": true})),
                        Response::ok(req.dispatch_id(), serde_json::json!({"fresh": true})),
                    ] {
                        let mut json = serde_json::to_string(&response).expect("test");
                        json.push('\n');
                        writer.write_all(json.as_bytes()).expect("test");
                        writer.flush().expect("test");
                    }
                }
            });
            (listener, handle)
        }

        let sock = unique_sock();
        let (_listener, _handle) = mock_stale_then_fresh(&sock);
        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
        // Simulate a previous request (id 1) whose deadline expired.
        client.next_id = 2;
        client.abandoned_ids.insert(1);

        let result = client
            .request("ping", None)
            .expect("the stale frame must be drained, not returned");
        assert_eq!(result["fresh"], serde_json::json!(true));
        assert!(
            client.abandoned_ids.is_empty(),
            "draining must clear the abandoned id"
        );
    }

    /// A timed-out request must record its id so the eventual answer can be
    /// drained instead of skewing the connection.
    #[test]
    fn timed_out_request_records_the_abandoned_id() {
        let sock = unique_sock();
        let listener = UnixListener::bind(&sock).expect("test");
        let _handle = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("test");
            std::thread::sleep(Duration::from_secs(4));
        });
        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
        let error = client
            .request_with_timeout("slow", None, Duration::from_millis(50))
            .expect_err("an unanswered request must time out");
        assert!(
            error.contains("did not respond"),
            "unexpected error: {error}"
        );
        assert!(
            client.abandoned_ids.contains(&1),
            "the timed-out request id must be remembered for draining"
        );
    }

    /// The deadline expiring mid-frame used to drop the bytes already taken
    /// from the `BufReader`, so the next request read a truncated fragment and
    /// reported a parse error for a frame that was well formed.
    #[test]
    fn a_frame_split_by_a_deadline_is_read_whole_by_the_next_request() {
        let sock = unique_sock();
        let listener = UnixListener::bind(&sock).expect("test");
        let _handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test");
            let mut discard = [0_u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut discard);
            // First half of the response frame, then a pause longer than the
            // socket poll interval (so the caller's deadline is observed
            // mid-frame), then the rest.
            stream
                .write_all(br#"{"jsonrpc":"2.0","id":1,"result":{"value":"#)
                .expect("test");
            stream.flush().expect("test");
            std::thread::sleep(READ_POLL_INTERVAL + Duration::from_millis(500));
            stream.write_all(b"42}}\n").expect("test");
            stream.flush().expect("test");
            std::thread::sleep(Duration::from_secs(2));
        });

        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
        let error = client
            .request_with_timeout("slow", None, Duration::from_millis(100))
            .expect_err("the first request must time out mid-frame");
        assert!(
            error.contains("did not respond"),
            "unexpected error: {error}"
        );
        assert!(
            !client.partial_frame.is_empty(),
            "the bytes already taken from the socket must be kept"
        );

        // The daemon finishes the frame. Draining it must recognise the whole
        // JSON object, not a fragment.
        let error = client
            .request_with_timeout("next", None, Duration::from_millis(1000))
            .expect_err("the drained frame belongs to the abandoned request");
        assert!(
            !error.to_lowercase().contains("parse"),
            "a well-formed frame must not surface as a parse error: {error}"
        );
    }

    #[test]
    fn a_half_written_request_poisons_the_connection() {
        let sock = unique_sock();
        let listener = UnixListener::bind(&sock).expect("test");
        let _handle = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("test");
            std::thread::sleep(Duration::from_secs(5));
        });

        let stream = UnixStream::connect(&sock).expect("test");
        let mut client = DaemonClient::from_stream(test_stream(stream)).expect("test");
        client.set_write_timeout(Duration::from_millis(200));

        let big = serde_json::json!({ "blob": "x".repeat(64 * 1024) });
        for _ in 0..2000 {
            if client
                .send_request("test/flood", &Some(big.clone()))
                .is_err()
            {
                break;
            }
        }
        assert!(
            client.desynced.is_some(),
            "a frame the daemon saw the start of must poison the connection"
        );
        let error = client
            .request("ping", None)
            .expect_err("a poisoned connection must refuse further requests");
        assert!(error.contains("out of sync"), "unexpected error: {error}");
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
        let result = read_bounded_line(&mut reader, &mut Vec::new(), 64, None)
            .expect("under-cap line should succeed");
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    /// A reader whose first read is interrupted by a signal, as a socket read
    /// is when SIGCHLD or SIGWINCH arrives.
    struct InterruptedOnce<'a> {
        interrupted: bool,
        rest: &'a [u8],
    }

    impl std::io::Read for InterruptedOnce<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            std::io::Read::read(&mut self.rest, buf)
        }
    }

    #[test]
    fn bounded_read_retries_a_read_interrupted_by_a_signal() {
        let mut reader = std::io::BufReader::new(InterruptedOnce {
            interrupted: false,
            rest: b"{\"id\":1}\n",
        });
        let result = read_bounded_line(&mut reader, &mut Vec::new(), 64, None)
            .expect("an interrupted read is retried, not reported");
        assert_eq!(result.as_deref(), Some("{\"id\":1}"));
    }

    #[test]
    fn bounded_read_rejects_line_exceeding_cap() {
        let payload = [b'X'; 100];
        let mut reader = std::io::BufReader::new(&payload[..]);
        let err = read_bounded_line(&mut reader, &mut Vec::new(), 5, None)
            .expect_err("must reject oversized line");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("5 byte limit"));
    }

    #[test]
    fn bounded_read_returns_none_on_empty_eof() {
        let payload: &[u8] = &[];
        let mut reader = std::io::BufReader::new(payload);
        let result =
            read_bounded_line(&mut reader, &mut Vec::new(), 64, None).expect("EOF must not error");
        assert!(result.is_none());
    }

    #[test]
    fn bounded_read_rejects_incomplete_utf8_at_eof() {
        // 0xC3 is a 2-byte-sequence lead byte; no continuation, no newline.
        let payload: &[u8] = &[b'o', b'k', 0xC3];
        let mut reader = std::io::BufReader::new(payload);
        let err = read_bounded_line(&mut reader, &mut Vec::new(), 64, None)
            .expect_err("incomplete UTF-8 at EOF must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn bounded_read_rejects_incomplete_utf8_before_newline() {
        // Lead byte 0xC3 followed immediately by the newline terminator.
        let payload: &[u8] = &[b'o', b'k', 0xC3, b'\n'];
        let mut reader = std::io::BufReader::new(payload);
        let err = read_bounded_line(&mut reader, &mut Vec::new(), 64, None)
            .expect_err("incomplete UTF-8 before newline must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn empty_line_yields_empty_string_then_graceful_parse_error() {
        let payload: &[u8] = b"\n";
        let mut reader = std::io::BufReader::new(payload);
        let result = read_bounded_line(&mut reader, &mut Vec::new(), 64, None)
            .expect("bare newline must not error");
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

    /// A fake daemon that answers `handshake` with the identity it was given,
    /// `shutdown` by closing its listener, and `ping` with "pong".
    ///
    /// It records whether it was asked to shut down, which is how the tests
    /// tell "replaced" from "reused" apart.
    struct FakeDaemon {
        shutdown_requested: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeDaemon {
        fn start(sock: &Path, identity: BuildIdentity) -> Self {
            Self::start_with(sock, Some(identity))
        }

        /// `None` models a daemon built before `handshake` existed: it answers
        /// the method it does not know with `METHOD_NOT_FOUND`.
        fn start_with(sock: &Path, identity: Option<BuildIdentity>) -> Self {
            let listener = UnixListener::bind(sock).expect("bind fake daemon");
            listener
                .set_nonblocking(true)
                .expect("poll the fake daemon's listener");
            let shutdown_requested = Arc::new(AtomicBool::new(false));
            let stop = Arc::new(AtomicBool::new(false));
            let sock = sock.to_path_buf();

            let requested = Arc::clone(&shutdown_requested);
            let stopping = Arc::clone(&stop);
            let handle = std::thread::spawn(move || {
                while !stopping.load(Ordering::SeqCst) {
                    let stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    stream.set_nonblocking(false).expect("blocking connection");
                    let reader = std::io::BufReader::new(&stream);
                    let mut writer = &stream;
                    for line in reader.lines() {
                        let Ok(line) = line else { break };
                        let Ok(request) = serde_json::from_str::<Request>(&line) else {
                            break;
                        };
                        let response = match (request.method.as_str(), &identity) {
                            ("handshake", Some(identity)) => {
                                // The real daemon answers the nonce with an
                                // HMAC keyed by the per-user secret. A fake
                                // that cannot read that file is exactly the
                                // planted daemon the client must refuse, so
                                // this one reads it too.
                                let mut answer =
                                    serde_json::to_value(identity).expect("serialize identity");
                                let nonce = request
                                    .params
                                    .as_ref()
                                    .and_then(|params| params.get("nonce"))
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default();
                                if !nonce.is_empty() {
                                    if let Some(secret) = handshake_secret() {
                                        answer["proof"] = serde_json::json!(
                                            super::identity::proof(&secret, nonce, identity)
                                        );
                                    }
                                }
                                Response::ok(request.dispatch_id(), answer)
                            }
                            ("handshake", None) => Response::error(
                                request.dispatch_id(),
                                -32601,
                                "Unknown method: handshake",
                            ),
                            ("shutdown", _) => {
                                requested.store(true, Ordering::SeqCst);
                                Response::ok(
                                    request.dispatch_id(),
                                    serde_json::json!({"shutdownRequested": true}),
                                )
                            }
                            _ => Response::ok(request.dispatch_id(), serde_json::json!("pong")),
                        };
                        let stopping_now = request.method == "shutdown";
                        let mut json =
                            serde_json::to_string(&response).expect("serialize response");
                        json.push('\n');
                        let _ = writer.write_all(json.as_bytes());
                        let _ = writer.flush();
                        if stopping_now {
                            stopping.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                }
                // A real daemon unlinks its endpoint as it stops. Do the same,
                // so the test exercises the wait the client actually performs.
                drop(listener);
                let _ = std::fs::remove_file(&sock);
            });

            Self {
                shutdown_requested,
                stop,
                handle: Some(handle),
            }
        }

        fn was_asked_to_shut_down(&self) -> bool {
            self.shutdown_requested.load(Ordering::SeqCst)
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn test_identity(build: &str) -> BuildIdentity {
        BuildIdentity {
            version: identity::DAEMON_VERSION.to_string(),
            build: build.to_string(),
        }
    }

    /// The identity `connect_checked` compares against, so a fake daemon can
    /// claim to be this build or a different one. Fixed rather than read from
    /// the machine, so these tests do not depend on what is installed on it.
    fn expected_for_test() -> BuildIdentity {
        test_identity("git:1111feedface")
    }

    /// The defect this whole path exists for: a daemon left over from an
    /// earlier build keeps answering, with that build's response shapes.
    #[test]
    fn a_daemon_from_another_build_is_replaced() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let sock = unique_sock();
        let lock = sock.with_extension("lock");
        let stale = FakeDaemon::start(&sock, test_identity("git:0000deadbeef"));

        let replacement: std::sync::Mutex<Option<FakeDaemon>> = std::sync::Mutex::new(None);
        let spawned = AtomicU32::new(0);
        let mut spawn = |_root: &Path, endpoint: &Path| {
            spawned.fetch_add(1, Ordering::SeqCst);
            *replacement.lock().expect("test") =
                Some(FakeDaemon::start(endpoint, expected_for_test()));
            let stream = UnixStream::connect(endpoint).map_err(|e| e.to_string())?;
            Ok(test_stream(stream))
        };

        let project = std::env::temp_dir();
        let mut client = DaemonClient::connect_checked(
            &project,
            &sock,
            &lock,
            Some(&expected_for_test()),
            &mut spawn,
        )
        .expect("connect");

        assert!(
            stale.was_asked_to_shut_down(),
            "a daemon from another build must be asked to stop"
        );
        assert_eq!(
            spawned.load(Ordering::SeqCst),
            1,
            "exactly one replacement daemon, never a loop"
        );
        assert_eq!(
            client
                .request("ping", None)
                .expect("the replacement answers"),
            serde_json::json!("pong")
        );

        drop(client);
        drop(replacement);
        let _ = std::fs::remove_file(&sock);
    }

    /// The other half: a daemon from this build is worth keeping, and
    /// restarting it would throw away a warm index on every command.
    /// The forgery the reviewer ran: a fake daemon answered
    /// `{"version":"0.4.0","build":"git:45341c128570"}` and the client accepted
    /// it and sent its next request. Every input is world-readable, so the
    /// right words are one `echo` away. The proof is not: it is an HMAC keyed
    /// by a file only this user can read.
    #[test]
    fn an_identity_without_the_proof_does_not_pass_as_this_build() {
        let secret = handshake_secret().expect("this user has a runtime directory");
        let identity = expected_for_test();
        let challenge = identity::nonce();
        assert!(!challenge.is_empty(), "the test needs a nonce");

        // What a forger can produce: the identity, and nothing else.
        let forged = serde_json::to_value(&identity).expect("serialize");
        let client =
            DaemonClient::from_stream(test_stream(UnixStream::pair().expect("socketpair").0))
                .expect("client");
        let error = client
            .verify_handshake_proof(&challenge, &identity, &forged)
            .expect_err("an identity with no proof must not pass");
        assert!(error.contains("without a proof"), "{error}");

        // Keyed by something else, which is what a process that cannot read
        // the secret would have to guess.
        let mut wrong = forged.clone();
        wrong["proof"] =
            serde_json::json!(identity::proof(b"not the secret", &challenge, &identity));
        let error = client
            .verify_handshake_proof(&challenge, &identity, &wrong)
            .expect_err("a proof under another key must not pass");
        assert!(error.contains("did not match"), "{error}");

        // The real answer.
        let mut real = forged;
        real["proof"] = serde_json::json!(identity::proof(&secret, &challenge, &identity));
        client
            .verify_handshake_proof(&challenge, &identity, &real)
            .expect("the daemon that can read the secret is accepted");
    }

    /// The proof covers the identity, so a daemon cannot answer one challenge
    /// and reuse it for a build it is not.
    #[test]
    fn the_proof_covers_the_build_it_claims() {
        let secret = b"a fixed test secret";
        let first = BuildIdentity {
            version: "0.4.0".to_string(),
            build: "git:aaaaaaaaaaaa".to_string(),
        };
        let second = BuildIdentity {
            version: "0.4.0".to_string(),
            build: "git:bbbbbbbbbbbb".to_string(),
        };
        assert_ne!(
            identity::proof(secret, "n", &first),
            identity::proof(secret, "n", &second)
        );
        assert_ne!(
            identity::proof(secret, "n1", &first),
            identity::proof(secret, "n2", &first)
        );
        assert_eq!(
            identity::proof(secret, "n", &first),
            identity::proof(secret, "n", &first)
        );
    }

    #[test]
    fn a_daemon_from_this_build_is_reused() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let sock = unique_sock();
        let lock = sock.with_extension("lock");
        let running = FakeDaemon::start(&sock, expected_for_test());

        let spawned = AtomicU32::new(0);
        let mut spawn = |_root: &Path, _endpoint: &Path| {
            spawned.fetch_add(1, Ordering::SeqCst);
            Err("connect must not start a second daemon".to_string())
        };

        let project = std::env::temp_dir();
        let mut client = DaemonClient::connect_checked(
            &project,
            &sock,
            &lock,
            Some(&expected_for_test()),
            &mut spawn,
        )
        .expect("connect");

        assert!(
            !running.was_asked_to_shut_down(),
            "a matching daemon must be left running"
        );
        assert_eq!(spawned.load(Ordering::SeqCst), 0, "nothing to spawn");
        assert_eq!(
            client.request("ping", None).expect("the daemon answers"),
            serde_json::json!("pong")
        );

        drop(client);
        drop(running);
        let _ = std::fs::remove_file(&sock);
    }

    /// A daemon built before this check existed answers "Unknown method", and
    /// that is exactly the daemon the check is for.
    #[test]
    fn a_daemon_without_a_handshake_is_replaced() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let sock = unique_sock();
        let lock = sock.with_extension("lock");
        let stale = FakeDaemon::start_with(&sock, None);

        let replacement: std::sync::Mutex<Option<FakeDaemon>> = std::sync::Mutex::new(None);
        let mut spawn = |_root: &Path, endpoint: &Path| {
            *replacement.lock().expect("test") =
                Some(FakeDaemon::start(endpoint, expected_for_test()));
            let stream = UnixStream::connect(endpoint).map_err(|e| e.to_string())?;
            Ok(test_stream(stream))
        };

        let project = std::env::temp_dir();
        let client = DaemonClient::connect_checked(
            &project,
            &sock,
            &lock,
            Some(&expected_for_test()),
            &mut spawn,
        )
        .expect("connect must succeed against the replacement");
        let replaced = replacement.lock().expect("test").is_some();

        drop(client);
        drop(replacement);
        let _ = std::fs::remove_file(&sock);
        assert!(
            stale.was_asked_to_shut_down(),
            "a daemon that cannot state its build must be asked to stop"
        );
        assert!(
            replaced,
            "a daemon that cannot state its build must be replaced"
        );
    }

    #[test]
    fn waiting_on_a_live_endpoint_times_out_and_a_closed_one_returns_at_once() {
        let sock = unique_sock();
        let daemon = FakeDaemon::start(&sock, expected_for_test());
        assert!(
            !wait_for_endpoint_closed(&sock, Duration::from_millis(200)),
            "an endpoint that still accepts is not closed"
        );
        drop(daemon);
        assert!(
            wait_for_endpoint_closed(&sock, Duration::from_secs(5)),
            "a stopped daemon's endpoint must be seen as closed"
        );
        let _ = std::fs::remove_file(&sock);
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

    // Serializes tests that mutate, or depend on, the process-wide PATH and
    // AL_ALLOW_MISMATCHED_DAEMON. Environment variables are per process, so a
    // test that sets one is visible to every other test thread until it clears
    // it: without this, setting the allow variable made the replacement tests
    // reuse a daemon they were meant to replace.
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

    /// Write an executable stand-in for al-lsp that answers `--version` the
    /// way the real one does.
    fn fake_al_lsp(dir: &Path, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("al-lsp");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\necho \"al-lsp {version} (git:testbuild)\"\n"),
        )
        .expect("write fake binary");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .expect("make the fake binary executable");
        bin
    }

    /// Run `find_al_lsp_binary` with `PATH` set to `dir` and nothing else.
    fn find_with_path(dir: &Path) -> Result<PathBuf, String> {
        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", dir);
        let result = find_al_lsp_binary();
        match saved {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        result
    }

    /// True when this test process has an `al-lsp` beside it, which wins over
    /// PATH and makes the PATH tests meaningless.
    fn has_sibling_al_lsp() -> bool {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("al-lsp").exists()))
            .unwrap_or(false)
    }

    #[test]
    fn find_al_lsp_binary_locates_a_matching_version_in_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if has_sibling_al_lsp() {
            return;
        }

        let bin_dir = unique_dir("haspath");
        fake_al_lsp(&bin_dir, identity::DAEMON_VERSION);
        let result = find_with_path(&bin_dir);
        let _ = std::fs::remove_dir_all(&bin_dir);

        let found = result.expect("an al-lsp on PATH of this version must be used");
        assert_eq!(
            found.file_name().and_then(|name| name.to_str()),
            Some("al-lsp"),
            "found path must end in al-lsp: {found:?}"
        );
    }

    /// The client used to fall back to whatever `al-lsp` was on PATH and start
    /// a daemon from it with no warning. One machine's was weeks older than
    /// the client that started it.
    #[test]
    fn a_path_al_lsp_of_another_version_is_refused() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if has_sibling_al_lsp() {
            return;
        }

        let bin_dir = unique_dir("oldpath");
        fake_al_lsp(&bin_dir, "0.0.1-ancient");
        let result = find_with_path(&bin_dir);
        let _ = std::fs::remove_dir_all(&bin_dir);

        let error = result.expect_err("a PATH al-lsp of another version must be refused");
        assert!(
            error.contains("0.0.1-ancient") && error.contains(identity::DAEMON_VERSION),
            "the refusal must name both versions: {error}"
        );
        assert!(
            error.contains("cargo install") && error.contains("release archive"),
            "the refusal must say how to install a matching one: {error}"
        );
        assert!(
            error.contains(identity::ALLOW_MISMATCH_ENV),
            "the refusal must name the way past it: {error}"
        );
    }

    #[test]
    fn a_path_al_lsp_of_another_version_is_allowed_by_the_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if has_sibling_al_lsp() {
            return;
        }

        let bin_dir = unique_dir("allowedpath");
        fake_al_lsp(&bin_dir, "0.0.1-ancient");
        std::env::set_var(identity::ALLOW_MISMATCH_ENV, "1");
        let result = find_with_path(&bin_dir);
        std::env::remove_var(identity::ALLOW_MISMATCH_ENV);
        let _ = std::fs::remove_dir_all(&bin_dir);

        assert!(
            result.is_ok(),
            "{} must allow an older PATH al-lsp: {result:?}",
            identity::ALLOW_MISMATCH_ENV
        );
    }

    /// Whatever is named `al-lsp` on PATH need not be this program, so the
    /// probe cannot wait on it forever.
    #[test]
    fn a_binary_that_never_answers_is_not_a_version() {
        let dir = unique_dir("hangs");
        let bin = dir.join("al-lsp");
        std::fs::write(&bin, "#!/bin/sh\nsleep 120\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let started = std::time::Instant::now();
        let version = binary_version(&bin);
        let waited = started.elapsed();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(version, None, "a binary that hangs reports no version");
        assert!(
            waited < VERSION_PROBE_TIMEOUT + Duration::from_secs(5),
            "the probe must give up near its deadline, waited {waited:?}"
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
        let result = find_with_path(&empty);
        let _ = std::fs::remove_dir_all(&empty);

        let err = result.expect_err("missing al-lsp must error");
        assert!(
            err.contains("Cannot find al-lsp"),
            "error must name the missing binary: {err}"
        );
        assert!(
            err.contains("cargo install"),
            "error must say how to install it: {err}"
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

        let result = find_with_path(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        let err = result.expect_err("a directory named al-lsp must not be accepted as the binary");
        assert!(err.contains("Cannot find al-lsp"), "got: {err}");
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
                let response = Response::ok(req.dispatch_id(), serde_json::json!({"slow": true}));
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

#[cfg(test)]
mod tests;
