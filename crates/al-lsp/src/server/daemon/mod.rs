//! Daemon mode — JSON-RPC server over local IPC.
//!
//! `al-lsp daemon --project /path/to/project` starts a daemon that:
//! - Listens on a deterministic local endpoint
//! - Initializes a Workspace for the given project
//! - Accepts JSON-RPC requests and routes them to core queries
//! - Exits after 30 minutes with no connections and no running work, or as
//!   soon as its project root stops existing
//!
//! # Platform support
//!
//! The transport uses Unix-domain sockets on Linux/macOS and Windows named
//! pipes on Windows through the same `interprocess::local_socket` API. The
//! newline-delimited JSON-RPC framing and request dispatch are identical on
//! every platform.

mod build_dispatch;
mod containment;
mod debug_dispatch;
mod insight_dispatch;
mod lsp_dispatch;
mod process_memory;
mod projection;
mod scope;

pub(crate) use debug_dispatch::authorize_live_test_target;
pub(crate) use projection::list_target;
pub(crate) use scope::accepts_scope;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use al_protocol::jsonrpc::{error_codes, Request, Response, RpcError};
use al_protocol::socket_path;
use al_workspace::Workspace;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalSocketStream},
    GenericFilePath, ListenerOptions, ToFsName,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Notify;
use tokio::sync::Semaphore;

/// Process-start `Instant` used as the epoch for `last_activity` millis.
///
/// `Instant` is not directly storable in an atomic, so we keep a captured
/// base and store millis-since-base in `AtomicU64`. The base is initialised
/// the first time `now_activity_ms` is called and lives for the process
/// lifetime (lazy `OnceLock`).
static DAEMON_EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Millis since the daemon epoch — monotonic, atomic-storable.
fn now_activity_ms() -> u64 {
    let epoch = DAEMON_EPOCH.get_or_init(Instant::now);
    Instant::now().duration_since(*epoch).as_millis() as u64
}

#[cfg(unix)]
static SOCKET_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The directory this daemon was started for, which the per-request refresh
/// scans when the workspace has no app.json project.
static SCAN_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(unix)]
pub(crate) fn cleanup_socket() {
    if let Some(path) = SOCKET_PATH.get() {
        let _ = std::fs::remove_file(path);
        tracing::info!("daemon: socket cleaned up");
    }
}

#[cfg(unix)]
struct SocketCleanup;

#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        cleanup_socket();
    }
}

/// How long a daemon with no connections and no running work stays alive.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Overrides [`DEFAULT_IDLE_TIMEOUT`]. `0` keeps the daemon alive until it is
/// stopped.
const IDLE_TIMEOUT_ENV: &str = "AL_DAEMON_IDLE_SECS";
/// How often the lifecycle task looks at the idle clock and the project root.
/// Both checks are two atomic loads and one `stat`, so a one-second cadence
/// costs nothing and lets a deleted project root be noticed while the tooling
/// that deleted it is still running.
const LIFECYCLE_POLL: Duration = Duration::from_secs(1);
/// How often the lifecycle task repeats a reason for not exiting.
const SKIP_LOG_INTERVAL: Duration = Duration::from_secs(60);
/// Consecutive polls that must find the project root missing before the daemon
/// stops. Two polls keep a network filesystem's momentary failure from
/// stopping a daemon whose project is still there.
const MISSING_ROOT_POLLS: u32 = 2;
const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;
/// How many requests one connection may have running at once.
///
/// The connection loop used to await each dispatch before reading the next
/// line, so a `ping` pipelined behind a `tests.run` or a `downloadSymbols`
/// waited for the long call. Requests now run as tasks; the cap keeps one
/// client from filling the blocking pool, and reading stops until a permit
/// frees up, which is the backpressure the sequential loop gave for free.
const MAX_IN_FLIGHT_PER_CONNECTION: usize = 8;
const ACCEPT_BACKOFF_START: Duration = Duration::from_millis(10);
const ACCEPT_BACKOFF_CAP: Duration = Duration::from_secs(5);

// The daemon runtime directory check lives beside the client's endpoint
// check, so the two cannot drift: the daemon runs it before it creates the
// socket and the client runs it before it connects. See
// `al_protocol::endpoint`.
#[cfg(unix)]
use al_protocol::endpoint::ensure_private_dir;

/// How long this daemon stays alive with nothing to do.
///
/// `explicit` is the `--idle-timeout-secs` argument. Without it the
/// [`IDLE_TIMEOUT_ENV`] environment variable decides, and without that
/// [`DEFAULT_IDLE_TIMEOUT`] does. `Some(0)` from either source means the
/// daemon never exits on its own.
fn resolve_idle_timeout(explicit: Option<Duration>) -> Option<Duration> {
    let configured = match explicit {
        Some(timeout) => timeout,
        None => match std::env::var(IDLE_TIMEOUT_ENV) {
            Err(_) => DEFAULT_IDLE_TIMEOUT,
            Ok(raw) => match raw.trim().parse::<u64>() {
                Ok(secs) => Duration::from_secs(secs),
                Err(_) => {
                    tracing::warn!(
                        value = %raw,
                        "daemon: {IDLE_TIMEOUT_ENV} is not a number of seconds, using the default"
                    );
                    DEFAULT_IDLE_TIMEOUT
                }
            },
        },
    };
    (!configured.is_zero()).then_some(configured)
}

pub async fn run_daemon(
    project_root: PathBuf,
    idle_timeout: Option<Duration>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Capture the build identity before anything can overwrite the executable
    // on disk, so a rebuild cannot make this process claim the new build.
    let identity = al_protocol::identity::current_identity();
    tracing::info!(build = %identity, "daemon: build identity");

    let endpoint = socket_path(&project_root).ok_or(
        "Cannot determine a local daemon endpoint: no per-user runtime directory is available",
    )?;

    #[cfg(unix)]
    // Ensure parent directory exists, owner-only (0o700). The socket file is
    // already 0o600 below, but the containing dir defaulted to the process
    // umask (often 0o755) — world-readable, leaking the socket filename (a hash
    // of the project path) to other users on a shared host. See ensure_private_dir.
    if let Some(parent) = endpoint.parent() {
        ensure_private_dir(parent)?;
    }

    #[cfg(unix)]
    let _ = tokio::fs::remove_file(&endpoint).await;

    let name = endpoint.as_path().to_fs_name::<GenericFilePath>()?;
    let listener = ListenerOptions::new().name(name).create_tokio()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))?;
    }
    tracing::info!(endpoint = %endpoint.display(), project = %project_root.display(), "daemon: listening");

    #[cfg(unix)]
    let _ = SOCKET_PATH.set(endpoint.clone());
    #[cfg(unix)]
    let _cleanup = SocketCleanup;

    let workspace = Arc::new(Workspace::new());

    // CLI/TUI daemon clients do not send LSP initializationOptions. Merge the
    // persisted config with project-local VS Code/Zed settings so compiler
    // backend and symbol-package paths match the editor.
    // Recorded before the read, so a store or settings file written while this
    // evaluation runs is seen as a change by the next request rather than
    // missed.
    TRUST_INPUTS.store(
        al_project::trust::inputs_fingerprint(&project_root),
        std::sync::atomic::Ordering::Relaxed,
    );
    let evaluated = al_project::trust::evaluate(&project_root)?;
    *workspace.config.write().await = evaluated.config;
    if let Some(advisory) = evaluated.decision.advisory() {
        tracing::warn!("daemon: {advisory}");
    }
    if let Some(advisory) = al_project::trust::enforce_dotnet_path(&project_root) {
        tracing::warn!("daemon: {advisory}");
    }
    let _ = workspace.trust_advisory.set(evaluated.decision.advisory());

    let _ = workspace.notify_sink.set(std::sync::Arc::new(|msg: &str| {
        tracing::warn!("daemon: {msg}");
    }));

    initialize_daemon_workspace(&workspace, &project_root).await?;
    let _ = SCAN_ROOT.set(project_root.clone());

    // Warm the dependency AL source index and the graphs built on it now,
    // rather than inside whichever query needs them first. The build takes
    // about a minute on Base Application; paid here it overlaps with the
    // agent's first few symbol queries, and `status` can report its progress
    // from the start instead of only once something is already blocked on it.
    let warm_workspace = Arc::clone(&workspace);
    tokio::task::spawn_blocking(move || match warm_workspace.get_or_build_call_graph() {
        Ok(_) => tracing::info!("daemon: dependency source index and call graph warm"),
        Err(error) => tracing::warn!(%error, "daemon: background index warm-up failed"),
    });

    // stored as millis-since-`DAEMON_EPOCH` in an AtomicU64 so
    // the hot per-connection-accept + per-dispatch update is lock-free.
    // Previous `Arc<Mutex<Instant>>` serialised every connection at the lock.
    let last_activity = Arc::new(AtomicU64::new(now_activity_ms()));
    // Requests currently being dispatched. The idle reaper never fires while
    // this is non-zero.
    let in_flight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let shutdown_signal = Arc::new(Notify::new());

    let activity_clone = Arc::clone(&last_activity);
    let in_flight_reaper = Arc::clone(&in_flight);
    let ws_clone = Arc::clone(&workspace);
    let shutdown_idle = Arc::clone(&shutdown_signal);
    let watched_root = project_root.clone();
    let idle_timeout = resolve_idle_timeout(idle_timeout);
    match idle_timeout {
        Some(timeout) => tracing::info!(
            idle_secs = timeout.as_secs(),
            "daemon: will exit after this much idle time"
        ),
        None => tracing::info!("daemon: idle exit disabled ({IDLE_TIMEOUT_ENV}=0)"),
    }
    let idle_timeout_handle = tokio::spawn(async move {
        let mut missing_root_polls = 0_u32;
        let mut last_skip_log: Option<Instant> = None;
        loop {
            tokio::time::sleep(LIFECYCLE_POLL).await;

            // A project root that no longer exists cannot be served, and the
            // work in flight for it cannot mean anything either. This is the
            // case that left daemons for deleted git worktrees resident: the
            // idle clock is not the thing that notices.
            if watched_root.exists() {
                missing_root_polls = 0;
            } else {
                missing_root_polls += 1;
                if missing_root_polls >= MISSING_ROOT_POLLS {
                    tracing::info!(
                        project = %watched_root.display(),
                        "daemon: project root is gone, shutting down"
                    );
                    shutdown_idle.notify_one();
                    return;
                }
                continue;
            }

            let Some(idle_timeout) = idle_timeout else {
                continue;
            };
            let elapsed = Duration::from_millis(
                now_activity_ms().saturating_sub(activity_clone.load(Ordering::Relaxed)),
            );
            if elapsed < idle_timeout {
                continue;
            }

            // Don't shut down while a request is still being served. The
            // activity timestamp is bumped when a request starts and again
            // when it finishes, but a single operation can legitimately run
            // longer than the whole idle window (a large symbol download, a
            // live-BC snapshot with a long `timeoutMs`), and reaping it
            // mid-flight cut the operation off after only the 10 s drain.
            let running = in_flight_reaper.load(Ordering::Acquire);
            // A debug session keeps the daemon alive too. The `try_lock` is
            // deliberate: a held `debug_session` mutex counts as a live
            // session, because the only thing that holds it for longer than a
            // few milliseconds is a debug-session RPC, which means a session
            // exists. Holding it anywhere else would keep the daemon resident
            // for good, so this reports which guard fired.
            let has_debug_session = running == 0
                && ws_clone
                    .debug_session
                    .try_lock()
                    .map(|session| session.is_some())
                    .unwrap_or(true);
            if running > 0 || has_debug_session {
                // At warn, because past the idle window these are the two
                // reasons a daemon outlives the session that started it, and
                // the log is the only place that says which one it was.
                let due = last_skip_log.is_none_or(|at| at.elapsed() >= SKIP_LOG_INTERVAL);
                if due {
                    last_skip_log = Some(Instant::now());
                    tracing::warn!(
                        idle_secs = elapsed.as_secs(),
                        in_flight = running,
                        debug_session = has_debug_session,
                        "daemon: past its idle window but still holding work"
                    );
                }
                continue;
            }

            tracing::info!(
                idle_secs = elapsed.as_secs(),
                "daemon: idle timeout, shutting down"
            );
            shutdown_idle.notify_one();
            return;
        }
    });

    let connection_limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    #[cfg(unix)]
    let mut sigterm = {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::terminate()).ok()
    };
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let mut accept_backoff = ACCEPT_BACKOFF_START;
    loop {
        #[cfg(unix)]
        let sigterm_fut = async {
            match sigterm.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        #[cfg(not(unix))]
        let sigterm_fut = std::future::pending::<()>();

        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok(stream) => {
                        accept_backoff = ACCEPT_BACKOFF_START;

                        // Bump the idle timer *before* spawning the connection
                        // handler. Without this there's a small race window
                        // between accept and the spawned task's first
                        // dispatch where the 60s-poll idle-timeout checker
                        // could fire on a daemon that just received a fresh
                        // connection. The spawned task still updates the
                        // timer per-request; this just closes the accept->
                        // first-request gap.
                        last_activity.store(now_activity_ms(), Ordering::Relaxed);

                        let permit = match connection_limit.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                tracing::warn!("daemon: connection limit ({MAX_CONNECTIONS}) reached, rejecting new connection");
                                // Send an actionable JSON-RPC error before
                                // hanging up. Dropping the stream silently left
                                // the client blocked until its own timeout with
                                // no indication of why.
                                tokio::spawn(async move {
                                    reject_connection_over_limit(stream).await;
                                });
                                continue;
                            }
                        };

                        let ws = Arc::clone(&workspace);
                        let activity = Arc::clone(&last_activity);
                        let in_flight_conn = Arc::clone(&in_flight);
                        let shutdown_conn = Arc::clone(&shutdown_signal);
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(e) = handle_connection(stream, ws, activity, in_flight_conn, shutdown_conn).await {
                                tracing::warn!(error = %e, "daemon: connection error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "daemon: accept error");
                        tokio::time::sleep(accept_backoff).await;
                        accept_backoff = (accept_backoff * 2).min(ACCEPT_BACKOFF_CAP);
                    }
                }
            }
            _ = shutdown_signal.notified() => {
                tracing::info!("daemon: idle timeout — shutting down");
                break;
            }
            _ = sigterm_fut => {
                tracing::info!("daemon: received SIGTERM — shutting down gracefully");
                break;
            }
            _ = &mut ctrl_c => {
                tracing::info!("daemon: received Ctrl-C — shutting down gracefully");
                break;
            }
        }
    }

    idle_timeout_handle.abort();

    // graceful drain. The accept loop has broken; new connections
    // are no longer accepted. In-flight connection tasks still hold a
    // semaphore permit each, so when ALL permits are available again every
    // connection has finished cleanly. Wait for that (with a timeout) so a
    // build / download in progress completes rather than being cut off
    // mid-write. If the grace period elapses, the tasks are dropped on
    // tokio runtime shutdown — same as before.
    const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
    let acquire_all = connection_limit.acquire_many(MAX_CONNECTIONS as u32);
    match tokio::time::timeout(SHUTDOWN_GRACE, acquire_all).await {
        Ok(Ok(_permits)) => {
            tracing::info!("daemon: all in-flight connections drained");
        }
        Ok(Err(_)) => {
            // Semaphore closed — should not happen; we don't close it.
            tracing::warn!("daemon: connection semaphore closed during drain");
        }
        Err(_) => {
            tracing::warn!(
                grace_secs = SHUTDOWN_GRACE.as_secs(),
                "daemon: graceful drain timed out, dropping in-flight tasks"
            );
        }
    }

    Ok(())
}

/// Tell a client that the daemon is at its connection limit, then hang up.
///
/// The frame uses `id: null` because no request has been read yet; JSON-RPC 2.0
/// §5 requires a null id when the request id is unknown.
async fn reject_connection_over_limit(stream: LocalSocketStream) {
    let (_reader, mut writer) = stream.split();
    let frame = format!(
        "{}\n",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {
                "code": error_codes::INTERNAL_ERROR,
                "message": format!(
                    "al-lsp daemon is busy: the {MAX_CONNECTIONS}-connection limit is \
                     reached. Retry shortly, or stop unused al-explorer/editor clients."
                ),
            }
        })
    );
    if let Err(error) = writer.write_all(frame.as_bytes()).await {
        tracing::debug!(%error, "daemon: could not send connection-limit rejection");
        return;
    }
    let _ = writer.flush().await;
}

/// Read a single newline-delimited line, enforcing a byte limit during reading.
/// Returns `Ok(None)` on EOF, `Err` if the line exceeds `max_bytes`.
pub(crate) async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<String>, std::io::Error> {
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
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
        // limit is enforced before the allocation, not after. Previously the
        // two post-extend checks (newline branch + no-newline branch) made
        // the invariant non-obvious and left a window where buf could
        // momentarily exceed max_bytes.
        let prospective_take = if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            pos // bytes up to (but excluding) the newline
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

/// Increments an in-flight counter for as long as it is alive.
///
/// A guard (rather than a manual decrement) so the count is restored even if
/// the connection task is dropped mid-dispatch.
struct InFlightGuard(Arc<std::sync::atomic::AtomicUsize>);

impl InFlightGuard {
    fn new(counter: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(counter))
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn handle_connection(
    stream: LocalSocketStream,
    workspace: Arc<Workspace>,
    last_activity: Arc<AtomicU64>,
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    shutdown: Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, writer) = stream.split();
    let mut reader = BufReader::new(reader);
    // One writer shared by every in-flight request on this connection, so two
    // responses can never interleave on the wire. Mirrors `run_mcp`'s
    // `write_mcp_frame`.
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_PER_CONNECTION));
    let mut in_flight_tasks = tokio::task::JoinSet::new();
    // Set when a write fails, so the read loop stops instead of queueing more
    // work for a client that is gone.
    let client_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // previously a 50 ms ring-buffer dedup over hover / completions /
    // signatureHelp / inlayHints replied to repeat requests with `null` /
    // `[]`. Editors that legitimately re-issue these (debounce flush, retry
    // after typing, parallel daemon clients) saw missing-info flicker.
    // Removed entirely — real coalescing requires keeping the request IDs
    // around to replay the completed result, and the workload here is small
    // enough that running the dispatch twice is cheaper than the
    // correctness debt.

    while let Some(line) = read_bounded_line(&mut reader, MAX_MESSAGE_SIZE).await? {
        if client_gone.load(Ordering::Relaxed) {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // JSON-RPC 2.0 §5 separates the two failure modes: text that is not
        // valid JSON is a PARSE_ERROR, while valid JSON that is not a valid
        // request object is an INVALID_REQUEST. Both answer with a null id when
        // the id cannot be recovered.
        let raw_message = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(value) => value,
            Err(e) => {
                // Record the parse-error context here. If the write below fails
                // (broken pipe / client gone), `?` would propagate a bare I/O
                // error to `handle_connection` and the original malformed-JSON
                // reason would be lost — only a generic "connection error" would
                // surface. Logging first preserves the diagnostic either way.
                tracing::warn!(error = %e, "daemon: malformed JSON-RPC request");
                if !write_frame(
                    &mut *writer.lock().await,
                    &error_frame(
                        serde_json::Value::Null,
                        error_codes::PARSE_ERROR,
                        &format!("Invalid JSON: {e}"),
                    ),
                )
                .await
                {
                    break;
                }
                continue;
            }
        };
        // Echo whatever id the client sent, even a string or null one.
        let echo_id = raw_message
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let req = match serde_json::from_value::<Request>(raw_message) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "daemon: invalid JSON-RPC request object");
                if !write_frame(
                    &mut *writer.lock().await,
                    &error_frame(
                        echo_id,
                        error_codes::INVALID_REQUEST,
                        &format!("Invalid JSON-RPC request: {e}"),
                    ),
                )
                .await
                {
                    break;
                }
                continue;
            }
        };

        // Mark activity when the request STARTS, and again when it finishes.
        // The start bump is what keeps the idle reaper from firing mid-request
        // (a 40-minute symbol download or live-BC capture would otherwise look
        // idle for its whole duration); the completion bump keeps the daemon
        // alive for the idle window measured from the end of the work.
        last_activity.store(now_activity_ms(), Ordering::Relaxed);

        // Reading stops here while the connection is already at its in-flight
        // cap, which is the backpressure the sequential loop provided.
        let permit = match Arc::clone(&permits).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };

        let workspace = Arc::clone(&workspace);
        let shutdown = Arc::clone(&shutdown);
        let writer = Arc::clone(&writer);
        let last_activity = Arc::clone(&last_activity);
        let client_gone = Arc::clone(&client_gone);
        let in_flight = Arc::clone(&in_flight);
        in_flight_tasks.spawn(async move {
            let _permit = permit;
            let is_notification = req.is_notification();
            let request_id = req.id.clone();
            let method = req.method.clone();
            let start = Instant::now();
            let response = {
                // Held for the whole dispatch so the idle reaper cannot fire
                // mid-request, however long the operation takes.
                let _in_flight = InFlightGuard::new(&in_flight);
                dispatch_request(&workspace, req, &shutdown).await
            };
            let elapsed = start.elapsed();
            tracing::debug!(method = %method, id = ?request_id, elapsed_us = elapsed.as_micros() as u64, "daemon: request");
            last_activity.store(now_activity_ms(), Ordering::Relaxed);

            if is_notification {
                // JSON-RPC 2.0 §4.1: a notification is processed but MUST NOT
                // be answered.
                return;
            }

            let frame = match request_id {
                // The common case — an id that round-trips through the
                // dispatcher's `u64` — serializes the typed response directly.
                // Anything else (string, null, negative, or fractional) is
                // echoed verbatim.
                Some(ref id) if id.as_u64().is_some() => serde_json::to_value(&response),
                Some(ref id) => Ok(response.to_json_with_id(id)),
                None => serde_json::to_value(&response),
            };
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) => {
                    tracing::error!(method = %method, %error, "daemon: response is not serializable");
                    error_frame(
                        request_id
                            .as_ref()
                            .map(|id| id.to_json())
                            .unwrap_or(serde_json::Value::Null),
                        error_codes::INTERNAL_ERROR,
                        &format!("response for {method} is not serializable: {error}"),
                    )
                }
            };
            if !write_frame(&mut *writer.lock().await, &frame).await {
                client_gone.store(true, Ordering::Relaxed);
            }
        });

        // Reap finished tasks so the set does not grow for the connection's
        // lifetime. `try_join_next` never blocks the read loop.
        while in_flight_tasks.try_join_next().is_some() {}
    }

    // Requests already accepted must finish and answer before the connection
    // closes, so a client that pipelined and then stopped reading still gets
    // every response it was owed.
    while in_flight_tasks.join_next().await.is_some() {}

    Ok(())
}

fn error_frame(id: serde_json::Value, code: i32, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// Write one newline-delimited JSON frame. Returns `false` when the client
/// connection is gone, so callers can end the loop cleanly instead of masking
/// the reason already logged.
async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &serde_json::Value,
) -> bool {
    let mut raw = frame.to_string();
    raw.push('\n');
    if let Err(error) = writer.write_all(raw.as_bytes()).await {
        tracing::warn!(%error, "daemon: failed to send response");
        return false;
    }
    if let Err(error) = writer.flush().await {
        tracing::warn!(%error, "daemon: failed to flush response");
        return false;
    }
    true
}

/// Run a synchronous, workspace-scale dispatcher on the blocking pool.
///
/// Insight queries walk the whole workspace-enriched call graph. Running them
/// inline on the async connection task stalls the tokio worker that drives I/O
/// for *every* connection, so they get the same treatment `deadCode` already
/// had.
async fn offload<F>(id: u64, method: &'static str, work: F) -> Response
where
    F: FnOnce() -> Response + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(response) => response,
        Err(error) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("{method} query worker failed: {error}"),
        ),
    }
}

/// The fingerprint of the trust inputs the daemon last evaluated.
///
/// Process-wide rather than per workspace: a daemon serves one project.
static TRUST_INPUTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Re-evaluate trust when anything it reads has changed.
///
/// The daemon evaluated once at startup and kept that configuration until it
/// exited, which is up to `AL_DAEMON_IDLE_SECS` after the last request, or
/// never while an editor keeps it busy. So `al-explorer trust --revoke` left
/// the privileged settings in effect in the process that was applying them.
///
/// Four `stat` calls per request decide whether to read the files again, so
/// the common case costs nothing and a revoke takes effect on the next
/// request.
async fn refresh_trust(workspace: &Workspace) {
    use std::sync::atomic::Ordering;

    let Some(project_root) = workspace
        .project
        .try_read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|project| project.root.clone()))
    else {
        return;
    };

    let fingerprint = al_project::trust::inputs_fingerprint(&project_root);
    if TRUST_INPUTS.swap(fingerprint, Ordering::Relaxed) == fingerprint {
        return;
    }

    match al_project::trust::evaluate(&project_root) {
        Ok(evaluated) => {
            if let Some(advisory) = evaluated.decision.advisory() {
                tracing::warn!("daemon: {advisory}");
            }
            // A `dotnet` host in the tree is part of the record, so a replaced
            // one makes the project stale and is dropped here, not only at
            // startup.
            if let Some(advisory) = al_project::trust::enforce_dotnet_path(&project_root) {
                tracing::warn!("daemon: {advisory}");
            }
            *workspace.config.write().await = evaluated.config;
        }
        // A settings file that stopped parsing is not a reason to keep serving
        // the configuration it used to hold.
        Err(error) => {
            tracing::warn!(%error, "daemon: trust re-evaluation failed, denying privileged settings");
            al_project::trust::deny_privileged(&mut *workspace.config.write().await);
        }
    }
}

/// Pick up `.al` files written, edited or deleted since the last request.
///
/// A metadata walk of the project per request; files whose size and mtime are
/// unchanged are not read. Refreshes run one at a time, so a burst of requests
/// after an edit re-reads the file once and every one of them sees it.
async fn refresh_workspace_files(workspace: &std::sync::Arc<Workspace>) {
    static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    // The root the startup scan indexed: the project, or the directory the
    // daemon was started for when it has no app.json.
    let project_root = workspace
        .project
        .try_read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|project| project.root.clone()));
    let Some(scan_root) = project_root.or_else(|| SCAN_ROOT.get().cloned()) else {
        return;
    };
    let _serialized = REFRESH.lock().await;
    let scan_workspace = std::sync::Arc::clone(workspace);
    let scanned = tokio::task::spawn_blocking(move || {
        let delta = al_workspace::refresh_workspace_files(&scan_workspace, &scan_root)?;
        sync_disk_documents(&scan_workspace, &delta);
        Ok::<_, al_source::file_index::ScanError>(delta)
    })
    .await;
    match scanned {
        Ok(Ok(delta)) if !delta.is_empty() => tracing::info!(
            changed = delta.changed.len(),
            removed = delta.removed.len(),
            topology_changed = delta.topology_changed,
            "daemon: workspace files changed on disk"
        ),
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "daemon: could not refresh workspace files from disk")
        }
        Err(error) => tracing::warn!(%error, "daemon: workspace refresh worker failed"),
    }
}

/// Carry a disk refresh into the daemon's document store.
///
/// The daemon opens every scanned file as a document at startup, and the
/// per-file queries (`symbols`, `hover`, `lint`) read the document. Refreshing
/// only the file index left them answering from the text the file had when
/// the daemon started.
fn sync_disk_documents(workspace: &Workspace, delta: &al_source::file_index::ScanDelta) {
    for path in &delta.changed {
        let Ok(uri) = url::Url::from_file_path(path) else {
            continue;
        };
        let Some(text) = workspace
            .file_index
            .files
            .get(path)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        if let Err(error) = workspace.documents.replace_or_open(uri, text) {
            tracing::warn!(path = %path.display(), %error, "daemon: changed file rejected by the document store");
        }
    }
    for path in &delta.removed {
        if let Ok(uri) = url::Url::from_file_path(path) {
            workspace.documents.close(&uri);
        }
    }
}

pub(crate) async fn dispatch_request(
    workspace: &std::sync::Arc<Workspace>,
    req: Request,
    shutdown: &Notify,
) -> Response {
    refresh_trust(workspace).await;
    refresh_workspace_files(workspace).await;
    let method = req.method.clone();
    let params = req.params.clone().unwrap_or(serde_json::Value::Null);
    let declared = DISPATCHERS
        .iter()
        .find(|dispatcher| dispatcher.method == method);
    if declared.is_some_and(|dispatcher| dispatcher.credential == CredentialUse::Authorized) {
        // The methods that can put a Business Central credential on the wire,
        // named in the log before they do it.
        tracing::info!(%method, "daemon: request may spend a Business Central credential");
    }
    let response = dispatch_method(workspace, req, shutdown).await;
    let response = path_refusal_advice(declared, response);
    // `scope` first, so a `limit` counts the rows that survive it rather than
    // the rows it was about to drop. Both are applied once, here, for every
    // method that takes them. See `scope` and `projection`.
    let response = scope::apply(workspace, &method, &params, response);
    projection::apply(&method, &params, response)
}

/// Tell a caller whose path was refused what it can do instead, which depends
/// on whether the method would have written the file.
///
/// `al-explorer` acts on the code alone, but the same refusal reaches an agent
/// through MCP's `al_call`, where the message is all there is.
fn path_refusal_advice(declared: Option<&Dispatcher>, mut response: Response) -> Response {
    let Some(dispatcher) = declared else {
        return response;
    };
    if let Some(error) = response.error.as_mut() {
        if error.code == error_codes::PATH_NOT_AUTHORIZED {
            match dispatcher.path {
                PathUse::Read => error.message.push_str(
                    "; send the file's 'text' with the request to have that content analysed \
                     without the daemon opening the path",
                ),
                PathUse::Write => error.message.push_str(
                    "; this method rewrites the file it names, so it takes a path inside the \
                     project and nothing else",
                ),
                PathUse::Named | PathUse::None => {}
            }
        }
    }
    response
}

/// What a method does with a path its caller names.
///
/// Declared beside the arm that routes to it, because the arm is generated
/// from the declaration: [`dispatch_table!`] builds [`DISPATCHERS`] and the
/// dispatch match from the same list, so a method cannot be dispatched without
/// stating what it reaches, and the tests below drive every entry. `rename`
/// took a `uri` straight to the filesystem for a release because nothing tied
/// the arm to the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathUse {
    /// Opens no path the caller names. A `file` that only identifies an
    /// already-indexed object, the way `debug breakpoint` uses it, is none of
    /// it: nothing is opened.
    None,
    /// Reads the one file its `uri`/`file` names, through
    /// [`read_document_from_params`], and accepts `text` in place of a path
    /// the daemon may not open.
    Read,
    /// Rewrites the file its `uri`/`file` names, through
    /// [`file_uri_from_params`], which takes no `text`.
    Write,
    /// Reads or writes a path named by another parameter (`xlf`, `generated`,
    /// `from`, `to`, `dir`), each resolved through
    /// `containment::resolve_within_project`. The XLIFF methods took any
    /// absolute path for a release because the registry had no way to say
    /// they took one at all.
    Named,
}

impl PathUse {
    /// Fold the capabilities one arm declares into a single value.
    const fn or(self, other: Self) -> Self {
        match self {
            Self::None => other,
            declared => declared,
        }
    }
}

/// Which Business Central credential a method can spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialUse {
    /// Only what the request or the user's own environment carries, against a
    /// target the caller chose.
    Caller,
    /// A credential the daemon holds, or the user's own sent to a server named
    /// by a file the repository carries. Both go through
    /// `al_project::trust::authorize_cached_credential`.
    Authorized,
}

impl CredentialUse {
    const fn or(self, other: Self) -> Self {
        match self {
            Self::Caller => other,
            declared => declared,
        }
    }
}

/// One dispatched method and what it reaches.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dispatcher {
    pub(crate) method: &'static str,
    pub(crate) path: PathUse,
    pub(crate) credential: CredentialUse,
}

macro_rules! declared_path {
    (read) => {
        PathUse::Read
    };
    (write) => {
        PathUse::Write
    };
    (named) => {
        PathUse::Named
    };
    (authorized) => {
        PathUse::None
    };
}

macro_rules! declared_credential {
    (read) => {
        CredentialUse::Caller
    };
    (write) => {
        CredentialUse::Caller
    };
    (named) => {
        CredentialUse::Caller
    };
    (authorized) => {
        CredentialUse::Authorized
    };
}

/// Build [`DISPATCHERS`] and the method match from one list of arms.
///
/// The capabilities in brackets are the ones [`PathUse`] and [`CredentialUse`]
/// define: `read`, `write`, `named`, `authorized`. An arm that declares none reaches
/// neither a caller-named path nor a credential.
macro_rules! dispatch_table {
    (
        ($workspace:ident, $method:ident, $id:ident, $params:ident, $shutdown:ident)
        $( $name:literal [$($cap:ident),*] => $body:expr, )*
    ) => {
        /// Every method the daemon dispatches, with what each one reaches.
        pub(crate) const DISPATCHERS: &[Dispatcher] = &[
            $(Dispatcher {
                method: $name,
                path: PathUse::None $(.or(declared_path!($cap)))*,
                credential: CredentialUse::Caller $(.or(declared_credential!($cap)))*,
            },)*
        ];

        async fn dispatch_known_method(
            $workspace: &std::sync::Arc<Workspace>,
            $method: &str,
            $id: u64,
            $params: serde_json::Value,
            $shutdown: &Notify,
        ) -> Response {
            match $method {
                $($name => $body,)*
                unknown => unknown_method($id, unknown),
            }
        }
    };
}

/// The answer for a method the daemon does not dispatch.
///
/// A method that differs only in case is named, because the spelling is the
/// whole of that mistake and an agent calling `al_call` has no completion to
/// correct it with.
fn unknown_method(id: u64, method: &str) -> Response {
    let suggestion = DISPATCHERS
        .iter()
        .find(|dispatcher| dispatcher.method.eq_ignore_ascii_case(method))
        .map(|dispatcher| format!(". Did you mean '{}'?", dispatcher.method))
        .unwrap_or_default();
    rpc_error(
        id,
        error_codes::METHOD_NOT_FOUND,
        &format!("Unknown method: {method}{suggestion}"),
    )
}

async fn dispatch_method(
    workspace: &std::sync::Arc<Workspace>,
    req: Request,
    shutdown: &Notify,
) -> Response {
    // Dispatch is keyed on u64; string/null ids dispatch under 0 and the
    // connection loop restores the original id on the wire.
    let id = req.dispatch_id();
    let params = req.params.unwrap_or(serde_json::Value::Null);
    dispatch_known_method(workspace, &req.method, id, params, shutdown).await
}

dispatch_table! {
    (workspace, method, id, params, shutdown)
        "hover" [read] => lsp_dispatch::dispatch_hover(workspace, id, &params).await,
        "definition" [read] => lsp_dispatch::dispatch_definition(workspace, id, &params),
        "references" [read] => lsp_dispatch::dispatch_references(workspace, id, &params),
        "implementations" [read] => lsp_dispatch::dispatch_implementations(workspace, id, &params),
        "completions" [read] => lsp_dispatch::dispatch_completions(workspace, id, &params).await,
        "signatureHelp" [read] => lsp_dispatch::dispatch_signature_help(workspace, id, &params),
        "rename" [read] => lsp_dispatch::dispatch_rename(workspace, id, &params),
        "documentSymbols" [read] => lsp_dispatch::dispatch_document_symbols(workspace, id, &params),
        "foldingRanges" [read] => lsp_dispatch::dispatch_folding_ranges(workspace, id, &params),
        "semanticTokens" [read] => lsp_dispatch::dispatch_semantic_tokens(workspace, id, &params),
        "inlayHints" [read] => lsp_dispatch::dispatch_inlay_hints(workspace, id, &params),
        "codeActions" [read] => lsp_dispatch::dispatch_code_actions(workspace, id, &params),
        "search" [] => lsp_dispatch::dispatch_search(workspace, id, &params),
        "object" [] => lsp_dispatch::dispatch_object(workspace, id, &params),
        "byId" [] => lsp_dispatch::dispatch_by_id(workspace, id, &params),
        "events" [] => lsp_dispatch::dispatch_events(workspace, id, &params),
        "subscribers" [] => lsp_dispatch::dispatch_subscribers(workspace, id, &params),
        "composed" [] => lsp_dispatch::dispatch_composed(workspace, id, &params),
        "packages" [] => lsp_dispatch::dispatch_packages(workspace, id),
        "deps" [] => lsp_dispatch::dispatch_deps(workspace, id),
        "lint" [read] => build_dispatch::dispatch_lint(workspace, id, &params).await,
        "format" [write] => build_dispatch::dispatch_format(workspace, id, &params).await,
        "fix" [write] => build_dispatch::dispatch_fix(workspace, id, &params),
        "fix.applicationArea" [] => {
            build_dispatch::dispatch_fix_application_area(workspace, id, &params)
        },
        "fix.tooltips" [] => build_dispatch::dispatch_fix_tooltips(workspace, id, &params),
        "fix.dataClassification" [] => {
            build_dispatch::dispatch_fix_data_classification(workspace, id, &params)
        },
        "rules" [] => build_dispatch::dispatch_rules(id),
        "parse" [read] => build_dispatch::dispatch_parse(workspace, id, &params),
        "metrics" [read] => build_dispatch::dispatch_metrics(workspace, id, &params),
        "sqlPatterns" [] => build_dispatch::dispatch_sql_patterns(workspace, id, &params),
        "sortMembers" [write] => build_dispatch::dispatch_sort_members(workspace, id, &params),
        "organizeFiles" [] => build_dispatch::dispatch_organize_files(workspace, id, &params),
        "source" [] => build_dispatch::dispatch_source(workspace, id, &params),
        "eventSource" [] => build_dispatch::dispatch_event_source(workspace, id, &params),
        "location" [] => build_dispatch::dispatch_location(workspace, id, &params),
        "trace" [] => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "trace", move || {
                insight_dispatch::dispatch_trace(&ws, id, &args)
            })
            .await
        },
        "entrypoints" [] => {
            let ws = Arc::clone(workspace);
            offload(id, "entrypoints", move || {
                insight_dispatch::dispatch_entrypoints(&ws, id)
            })
            .await
        },
        "graphExport" [] => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "graphExport", move || {
                insight_dispatch::dispatch_graph_export(&ws, id, &args)
            })
            .await
        },
        "insightStats" [] => {
            let ws = Arc::clone(workspace);
            offload(id, "insightStats", move || {
                insight_dispatch::dispatch_insight_stats(&ws, id)
            })
            .await
        },
        "deadCode" [] => {
            let ws = Arc::clone(workspace);
            offload(id, "deadCode", move || {
                insight_dispatch::dispatch_dead_code(&ws, id)
            })
            .await
        },
        "nativeCheck" [] => insight_dispatch::dispatch_native_check(workspace, id).await,
        "impact" [] => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "impact", move || {
                insight_dispatch::dispatch_impact(&ws, id, &args)
            })
            .await
        },
        "tableImpact" [] => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "tableImpact", move || {
                insight_dispatch::dispatch_table_impact(&ws, id, &args)
            })
            .await
        },
        "suggestEvent" [] => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "suggestEvent", move || {
                insight_dispatch::dispatch_suggest_event(&ws, id, &args)
            })
            .await
        },
        "traceChain" [] => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "traceChain", move || {
                insight_dispatch::dispatch_trace_chain(&ws, id, &args)
            })
            .await
        },
        "eventMap" [] => {
            let ws = Arc::clone(workspace);
            offload(id, "eventMap", move || {
                insight_dispatch::dispatch_event_map(&ws, id)
            })
            .await
        },
        "freeIds" [] => build_dispatch::dispatch_free_ids(workspace, id, &params).await,
        "permissions" [] => build_dispatch::dispatch_permissions(workspace, id, &params),
        "compile" [] => build_dispatch::dispatch_compile(workspace, id).await,
        "package" [] => build_dispatch::dispatch_package(workspace, id).await,
        "publish" [authorized] => build_dispatch::dispatch_publish(workspace, id, &params).await,
        "newProject" [named] => build_dispatch::dispatch_new_project(workspace, id, &params),
        "errorCodes" [] => build_dispatch::dispatch_error_codes(workspace, id).await,
        "builtinTypes" [] => build_dispatch::dispatch_builtin_types(workspace, id).await,
        "setup" [] => build_dispatch::dispatch_setup(workspace, id),
        "clearCache" [] => build_dispatch::dispatch_clear_cache(id).await,
        "authenticate" [] => build_dispatch::dispatch_authenticate(workspace, id, &params).await,
        "downloadSymbols" [authorized] => {
            build_dispatch::dispatch_download_symbols(workspace, id, &params).await
        },
        "debug" [authorized] => debug_dispatch::dispatch_debug(workspace, id, &params).await,
        "snapshot" [authorized] => build_dispatch::dispatch_snapshot(workspace, id, &params).await,
        "profiling" [authorized] => build_dispatch::dispatch_profiling(workspace, id, &params).await,
        "xlf.generate" [] => build_dispatch::dispatch_xlf_generate(workspace, id, &params).await,
        "xlf.refresh" [named] => build_dispatch::dispatch_xlf_refresh(workspace, id, &params).await,
        "xlf.untranslated" [named] => build_dispatch::dispatch_xlf_untranslated(workspace, id, &params),
        "xlf.suggest" [named] => build_dispatch::dispatch_xlf_suggest(workspace, id, &params).await,
        "tests.discover" [] => build_dispatch::dispatch_tests_discover(workspace, id),
        "tests.run" [authorized] => build_dispatch::dispatch_tests_run(workspace, id, &params).await,
        "tests.coverage" [] => build_dispatch::dispatch_tests_coverage(workspace, id),
        "tests.run_batch" [authorized] => build_dispatch::dispatch_tests_run_batch(workspace, id, &params).await,
        "tests.run_auto" [authorized] => build_dispatch::dispatch_tests_run_auto(workspace, id, &params).await,
        "tests.last_results" [] => {
            build_dispatch::dispatch_tests_last_results(workspace, id, &params).await
        },
        "tests.affected" [] => build_dispatch::dispatch_tests_affected(workspace, id, &params),
        "tests.classify" [] => build_dispatch::dispatch_tests_classify(workspace, id),
        "tests.snapshot_validate" [] => {
            build_dispatch::dispatch_tests_snapshot_validate(workspace, id, &params).await
        },
        "tests.snapshot_capture" [authorized] => {
            build_dispatch::dispatch_tests_snapshot_capture(workspace, id, &params).await
        },
        "tests.snapshot_replay" [authorized] => {
            build_dispatch::dispatch_tests_snapshot_replay(workspace, id, &params).await
        },
        "tests.snapshot_diff" [] => {
            build_dispatch::dispatch_tests_snapshot_diff(workspace, id, &params).await
        },
        "tests.mutate" [] => build_dispatch::dispatch_tests_mutate(workspace, id, &params).await,
        "generate" [] => build_dispatch::dispatch_generate(workspace, id, &params),
        "obsolete" [] => build_dispatch::dispatch_obsolete(workspace, id),
        "obsoleteUsages" [] => build_dispatch::dispatch_obsolete_usages(workspace, id),
        "packageDiff" [named] => build_dispatch::dispatch_package_diff(workspace, id, &params),
        "audit.dataClassification" [] => {
            build_dispatch::dispatch_audit_data_classification(workspace, id)
        },
        "permissions.audit" [] => build_dispatch::dispatch_permission_set_audit(workspace, id),
        "deps.graph" [] => build_dispatch::dispatch_deps_graph(workspace, id, &params).await,
        "breaking" [] => build_dispatch::dispatch_breaking_changes(workspace, id, &params).await,
        "arch.lint" [] => build_dispatch::dispatch_arch_lint(workspace, id).await,
        "duplicates" [] => build_dispatch::dispatch_find_duplicates(workspace, id, &params),
        "upgrade" [] => build_dispatch::dispatch_upgrade_report(workspace, id, &params).await,
        "profiler.hints" [] => build_dispatch::dispatch_profiler_hints(workspace, id, &params),
        "diag" [] => dispatch_diag(workspace, id, &params),
        "ping" [] => Response {
            id,
            result: Some(serde_json::json!("pong")),
            error: None,
            ..Default::default()
        },
        // Which build this daemon came from. A client compares it with its own
        // before it uses a daemon it did not start, and replaces a daemon that
        // answers with a different one. See `al_protocol::identity`.
        "handshake" [] => {
            let identity = al_protocol::identity::current_identity();
            let mut result = serde_json::json!({
                "version": identity.version,
                "build": identity.build,
                "pid": std::process::id(),
            });
            // Everything the identity is made of is world-readable, so a
            // process answering on this endpoint could say the same words. The
            // proof is an HMAC over the client's nonce and the identity, keyed
            // by a file only this user can read, so the answer is something a
            // planted daemon cannot produce.
            let nonce = params
                .get("nonce")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if !nonce.is_empty() {
                if let Some(secret) = al_protocol::client::handshake_secret() {
                    result["proof"] =
                        serde_json::json!(al_protocol::identity::proof(&secret, nonce, &identity));
                }
            }
            Response {
                id,
                result: Some(result),
                error: None,
                ..Default::default()
            }
        },
        "shutdown" [] => {
            tracing::info!("daemon: shutdown requested");
            shutdown.notify_one();
            Response {
                id,
                result: Some(serde_json::json!({"shutdownRequested": true})),
                error: None,
                ..Default::default()
            }
        },
        "status" [] => {
            // Read the project before taking the std RwLock guards below: a
            // guard held across an await makes this dispatch future non-Send.
            let launch_config_error = workspace
                .project
                .read()
                .await
                .as_ref()
                .and_then(|project| project.launch_config_error.clone());
            let semantic_cache = match workspace.semantic_cache.read() {
                Ok(cache) => cache,
                Err(_) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        "semantic-cache lock is poisoned; workspace status is unavailable",
                    );
                }
            };
            let builtins = match workspace.builtins.read() {
                Ok(builtins) => builtins,
                Err(_) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        "built-in symbol lock is poisoned; workspace status is unavailable",
                    );
                }
            };
            let cache_stats = {
                let c = &*semantic_cache;
                let (hits, misses) = c.stats();
                serde_json::json!({ "types": c.len(), "hits": hits, "misses": misses, "version": c.version() })
            };
            let status = serde_json::json!({
                "pid": std::process::id(),
                "indexedSymbols": workspace.symbols.len(),
                "workspaceFiles": workspace.file_index.len(),
                "workspaceObjects": workspace.file_index.object_count(),
                "builtinTypes": builtins.len(),
                "semanticCache": cache_stats,
                // Present only when the project's debug configuration file
                // could not be read. Symbol queries are unaffected; the BC
                // connection commands are the ones that need it.
                "launchConfigError": launch_config_error,
                // `subscribers`, `composed`, `events`, `lint`, `trace`,
                // `impact` and `entrypoints` all wait for this. A client that
                // sees `building` should keep waiting rather than retry.
                "sourceIndex": workspace.dependency_source_progress(),
                // The call graph those methods wait on builds after the
                // source index is ready, so `ready` above is not the end of
                // the wait.
                "callGraph": workspace.call_graph_progress(),
                // What the process costs the machine, which the per-structure
                // totals in `diag` do not show.
                "memory": process_memory::ResidentMemory::read().to_json(),
            });
            Response {
                id,
                result: Some(status),
                error: None,
                ..Default::default()
            }
        },
}

fn dispatch_diag(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let cmd = match params.get("cmd") {
        None => "summary",
        Some(value) => match value.as_str() {
            Some(cmd) => cmd,
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'cmd' must be a string when supplied",
                );
            }
        },
    };
    match cmd {
        "summary" => {
            let stats = match workspace.memory_stats() {
                Ok(stats) => stats,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("diag/summary could not inspect workspace state: {error}"),
                    );
                }
            };
            match serde_json::to_value(&stats) {
                Ok(mut value) => {
                    if let Some(object) = value.as_object_mut() {
                        object.insert(
                            "process".into(),
                            process_memory::ResidentMemory::read().to_json(),
                        );
                        match serde_json::to_value(workspace.dependency_source_progress()) {
                            Ok(progress) => {
                                object.insert("sourceIndex".into(), progress);
                            }
                            Err(error) => {
                                return rpc_error(
                                    id,
                                    error_codes::INTERNAL_ERROR,
                                    &format!("diag/summary source-index progress: {error}"),
                                );
                            }
                        }
                    }
                    Response {
                        id,
                        result: Some(value),
                        error: None,
                        ..Default::default()
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("diag/summary serialization failed: {e}"),
                    }),
                    ..Default::default()
                },
            }
        }
        _ => rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            &format!("Unknown diag subcommand: {cmd}. Available: summary"),
        ),
    }
}

pub(crate) fn extract_uri(params: &serde_json::Value) -> Option<url::Url> {
    let uri_str = params.get("uri")?.as_str()?;
    url::Url::parse(uri_str).ok()
}

pub(crate) fn extract_position(
    params: &serde_json::Value,
) -> Option<al_analysis::queries::Position> {
    // Cap to u32::MAX to prevent silent truncation of attacker-controlled values.
    let line = u32::try_from(params.get("line")?.as_u64()?).ok()?;
    let character = u32::try_from(params.get("character")?.as_u64()?).ok()?;
    Some(al_analysis::queries::Position { line, character })
}

/// Extract a JSON-RPC integer parameter as `i32` without silent truncation.
///
/// AL object IDs are i32 in the BC metadata; the JSON-RPC wire form is i64.
/// `params.get(key).as_i64()? as i32` would silently wrap on values >2³¹-1
/// (or < -2³¹). Use `try_from` so an out-of-range integer returns `None`
/// and the caller can return `INVALID_PARAMS` instead of corrupting the
/// query.
pub(crate) fn extract_i32(params: &serde_json::Value, key: &str) -> Option<i32> {
    i32::try_from(params.get(key)?.as_i64()?).ok()
}

pub(crate) fn optional_bool_param(
    params: &serde_json::Value,
    key: &str,
    default: bool,
) -> Result<bool, String> {
    match params.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("'{key}' must be a boolean when supplied")),
    }
}

pub(crate) fn optional_bounded_usize_param(
    params: &serde_json::Value,
    key: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, String> {
    match params.get(key) {
        None => Ok(default),
        Some(value) => {
            let raw = value
                .as_u64()
                .ok_or_else(|| format!("'{key}' must be a non-negative integer when supplied"))?;
            let parsed = usize::try_from(raw)
                .map_err(|_| format!("'{key}' is too large for this platform"))?;
            if parsed > maximum {
                return Err(format!(
                    "'{key}' must be no greater than {maximum}; received {parsed}"
                ));
            }
            Ok(parsed)
        }
    }
}

pub(crate) fn invalid_params(id: u64) -> Response {
    Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INVALID_PARAMS,
            message: "Missing or invalid parameters".to_string(),
        }),
        ..Default::default()
    }
}

pub(crate) fn file_not_found(id: u64) -> Response {
    Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::FILE_NOT_FOUND,
            message: "File not found".to_string(),
        }),
        ..Default::default()
    }
}

pub(crate) fn rpc_error(id: u64, code: i32, message: &str) -> Response {
    Response {
        id,
        result: None,
        error: Some(al_protocol::jsonrpc::RpcError {
            code,
            message: message.to_string(),
        }),
        ..Default::default()
    }
}

pub(crate) fn serialized_response<T: serde::Serialize>(
    id: u64,
    value: &T,
    method: &str,
) -> Response {
    match serde_json::to_value(value) {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        },
        Err(error) => {
            tracing::error!(method, %error, "daemon: serializing a result failed");
            rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("serialization failed for {method}: {error}"),
            )
        }
    }
}

// Err is a ready-to-send JSON-RPC `Response` by design (callers just return it
// on a cold error path); boxing it would scatter `*` derefs across every
// dispatcher for no real benefit.
/// How long a lock-taking helper waits for a transiently held workspace lock
/// before reporting the state as unavailable. Project-state writers
/// (`did_change_configuration`, reindex publication) hold the lock for a
/// handful of milliseconds; a `try_read` raced against one of those turned a
/// perfectly healthy workspace into "No project loaded".
pub(crate) const LOCK_WAIT: Duration = Duration::from_millis(500);

// Err is a ready-to-send JSON-RPC `Response` (cold path). Boxing it would only
// scatter `*` derefs across every caller.
#[allow(clippy::result_large_err)]
pub(crate) fn require_project_root(workspace: &Workspace, id: u64) -> Result<PathBuf, Response> {
    match project_root_with_wait(workspace) {
        Ok(Some(root)) => Ok(root),
        Ok(None) => Err(rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            "No project loaded",
        )),
        Err(error) => Err(rpc_error(id, error_codes::INTERNAL_ERROR, &error)),
    }
}

/// Read the loaded project root, briefly awaiting a transiently held lock.
///
/// Returns `Ok(None)` when no project is loaded and `Err` only when the lock
/// stayed held for the whole [`LOCK_WAIT`] window — the two cases the caller
/// must distinguish, and which `try_read` collapsed into one.
pub(crate) fn project_root_with_wait(workspace: &Workspace) -> Result<Option<PathBuf>, String> {
    project_state_with_wait(workspace, |project| project.map(|p| p.root.clone()))
}

/// Await the project lock briefly and project the guarded state with `map`.
pub(crate) fn project_state_with_wait<T>(
    workspace: &Workspace,
    map: impl FnOnce(Option<&al_project::project::AlProject>) -> T,
) -> Result<T, String> {
    const BUSY: &str =
        "Project state is busy (configuration reload or reindex in progress); retry the request";
    match tokio::runtime::Handle::try_current() {
        Ok(handle)
            if matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(|| {
                handle.block_on(async {
                    match tokio::time::timeout(LOCK_WAIT, workspace.project.read()).await {
                        Ok(guard) => Ok(map(guard.as_ref())),
                        Err(_) => Err(BUSY.to_string()),
                    }
                })
            })
        }
        // Synchronous callers and current-thread runtimes cannot block on the
        // runtime from inside it; fall back to the non-blocking attempt.
        _ => match workspace.project.try_read() {
            Ok(guard) => Ok(map(guard.as_ref())),
            Err(_) => Err(BUSY.to_string()),
        },
    }
}

/// Get document text without blocking the async runtime, loading it from disk
/// when the document store does not already contain the file.
// Err is a ready-to-send JSON-RPC `Response` (cold path); see require_project_root.
// Clippy flags this one only with the semantic feature on.
#[allow(clippy::result_large_err)]
pub(crate) async fn require_document_text(
    workspace: &Workspace,
    uri: &url::Url,
    id: u64,
) -> Result<String, Response> {
    if let Some(text) = workspace.documents.get_text(uri) {
        return Ok(text);
    }
    ensure_document(workspace, uri, id)?;
    workspace.documents.get_text(uri).ok_or_else(|| {
        rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            "Document loader completed without publishing the document",
        )
    })
}

// Err is a ready-to-send JSON-RPC `Response` (cold path); see require_project_root.
#[allow(clippy::result_large_err)]
pub(crate) fn parse_object_kind(
    id: u64,
    kind_str: &str,
) -> Result<al_symbols::ObjectKind, Response> {
    kind_str.parse::<al_symbols::ObjectKind>().map_err(|_| {
        rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            &format!("Unknown object kind: {kind_str}"),
        )
    })
}

/// Run a blocking step off the async executor when the runtime supports it.
///
/// `tokio::task::block_in_place` panics outright on a current-thread runtime,
/// which is what a plain `#[tokio::test]` gives and what an embedder may drive
/// the dispatcher from. Every blocking step in the daemon goes through here so
/// none of them can abort the process.
pub(crate) fn blocking<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle)
            if matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Ensure a file is loaded in the document store. If not found, read it through
/// the same bounded, regular-file-only ingestion path used by workspace scans.
// Err is a ready-to-send JSON-RPC `Response` (cold path); see require_project_root.
#[allow(clippy::result_large_err)]
pub(crate) fn ensure_document(
    workspace: &Workspace,
    uri: &url::Url,
    id: u64,
) -> Result<(), Response> {
    if workspace.documents.contains(uri) {
        return Ok(());
    }
    let path = uri.to_file_path().map_err(|()| {
        rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "Document URI is not a local file",
        )
    })?;
    let read_result = blocking(|| al_source::file_index::read_source_file(&path));
    let content = read_result
        .map_err(|error| {
            rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("Failed to load {}: {error}", path.display()),
            )
        })?
        .ok_or_else(|| file_not_found(id))?;
    workspace
        .documents
        .open(uri.clone(), content)
        .map_err(|error| {
            rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("Document was rejected: {error}"),
            )
        })
}

/// A path parameter the daemon refused, carrying the JSON-RPC code the client
/// needs to tell "the daemon will not touch this path" from any other bad
/// parameter.
///
/// `al-explorer` resends a read-only request with the file's text when it sees
/// [`error_codes::PATH_NOT_AUTHORIZED`], so the distinction has to survive the
/// round trip as a code rather than as prose.
#[derive(Debug)]
pub(crate) struct PathRejection {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl std::fmt::Display for PathRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl PathRejection {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: error_codes::INVALID_PARAMS,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            code: error_codes::PATH_NOT_AUTHORIZED,
            message: message.into(),
        }
    }

    pub(crate) fn into_response(self, id: u64) -> Response {
        rpc_error(id, self.code, &self.message)
    }
}

/// The path a request names, before any containment or filesystem check.
///
/// `Ok(None)` means the request named no file at all, which several
/// dispatchers treat as a whole-project request.
fn path_from_params(params: &serde_json::Value) -> Result<Option<PathBuf>, PathRejection> {
    let uri_value = params.get("uri");
    let file_value = params.get("file");
    if uri_value.is_some() && file_value.is_some() {
        return Err(PathRejection::invalid(
            "'uri' and 'file' are mutually exclusive",
        ));
    }

    match (uri_value, file_value) {
        (Some(value), None) => {
            let raw = value
                .as_str()
                .ok_or_else(|| PathRejection::invalid("'uri' must be a string when supplied"))?;
            if raw.trim().is_empty() {
                return Err(PathRejection::invalid("'uri' must not be empty"));
            }
            let uri = url::Url::parse(raw)
                .map_err(|error| PathRejection::invalid(format!("invalid 'uri': {error}")))?;
            let path = uri
                .to_file_path()
                .map_err(|()| PathRejection::invalid("'uri' must identify a local file"))?;
            Ok(Some(path))
        }
        (None, Some(value)) => {
            let raw = value
                .as_str()
                .ok_or_else(|| PathRejection::invalid("'file' must be a string when supplied"))?;
            if raw.trim().is_empty() {
                return Err(PathRejection::invalid("'file' must not be empty"));
            }
            Ok(Some(PathBuf::from(raw)))
        }
        (None, None) => Ok(None),
        (Some(_), Some(_)) => unreachable!("mutual exclusion checked above"),
    }
}

/// The `text` a caller supplied in place of letting the daemon open the path.
fn text_from_params(params: &serde_json::Value) -> Result<Option<String>, PathRejection> {
    match params.get("text") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| PathRejection::invalid("'text' must be a string when supplied")),
    }
}

/// The single existing local regular file a request names, resolved inside the
/// loaded project's boundary.
///
/// Every dispatcher that takes `uri` or `file` goes through here, so the
/// containment check in [`containment::resolve_within_project`] applies to all
/// of them at once. Relative paths resolve against the project root, not the
/// daemon's working directory: the daemon outlives the shell that started it,
/// so its cwd is not a meaningful base for a client's path.
///
/// This is the path a method that *writes* the file takes, so it never accepts
/// `text`: the content a caller supplies can be analysed, never written back
/// over a file the caller was not allowed to name.
pub(crate) fn file_uri_from_params(
    workspace: &Workspace,
    params: &serde_json::Value,
) -> Result<Option<url::Url>, PathRejection> {
    // Daemon file operations accept exactly one existing local regular file.
    // Failing canonicalisation used to fall back to the unresolved path, which
    // made missing files, inaccessible parents, and symlink failures look like
    // a valid request until a later and often unrelated operation failed.
    if text_from_params(params)?.is_some() {
        return Err(PathRejection::invalid(
            "'text' is not accepted here: this method rewrites the file it names, and only a \
             path inside the project can be written",
        ));
    }
    let Some(path) = path_from_params(params)? else {
        return Ok(None);
    };

    let canonical = containment::resolve_within_project(workspace, &path)
        .map_err(PathRejection::unauthorized)?;
    if !canonical.is_file() {
        return Err(PathRejection::invalid(format!(
            "input path '{}' is not a regular file",
            canonical.display()
        )));
    }
    let uri = url::Url::from_file_path(&canonical).map_err(|()| {
        PathRejection::invalid(format!(
            "input file cannot be represented as a file URI: {}",
            canonical.display()
        ))
    })?;
    Ok(Some(uri))
}

/// A document the caller supplied the text for, removed from the store when
/// the request that needed it is answered.
///
/// The text stands in for a file the daemon is not allowed to open, so it must
/// not outlive the one request: it is not part of the project, and leaving it
/// behind would put a file the daemon never read into workspace-wide answers.
pub(crate) struct SuppliedDocument<'a> {
    workspace: &'a Workspace,
    uri: url::Url,
}

impl Drop for SuppliedDocument<'_> {
    fn drop(&mut self) {
        self.workspace.documents.close(&self.uri);
    }
}

/// Whether a method names its subject document through
/// `read_document_from_params`, and so accepts `uri` or `file`, plus `text`
/// for a path the daemon may not open itself.
///
/// The MCP bridge derives the document arguments of a named tool's schema from
/// this, the way it derives `limit`/`offset`/`fields` from
/// `projection::list_target` and `scope` from `scope::accepts_scope`. A schema
/// written out by hand drifted once already: `al_getdiagnostics` rejected
/// `text` that `lint` accepts.
pub(crate) fn reads_document(method: &str) -> bool {
    matches!(
        method,
        "hover"
            | "definition"
            | "references"
            | "implementations"
            | "completions"
            | "signatureHelp"
            | "documentSymbols"
            | "foldingRanges"
            | "semanticTokens"
            | "inlayHints"
            | "codeActions"
            | "lint"
            | "format"
            | "metrics"
    )
}

/// The document a read-only single-file request works on, and the guard that
/// removes it again when the caller supplied its text.
///
/// Three inputs, tried in order:
///
/// 1. `text` from the caller. The daemon answers from that and never opens the
///    path, which is how `al-explorer` serves a file outside the project: the
///    user running the CLI can read their own files, the daemon must not read
///    them on anyone's behalf. Refused for a path *inside* the project, so no
///    caller can substitute its own content for a project file the daemon
///    holds and have a later write flush it to disk.
/// 2. a document already open in the store, which the editor put there. No
///    filesystem access, so containment has nothing to guard.
/// 3. the path itself, contained in the project and read from disk.
// Err is a ready-to-send JSON-RPC `Response` (cold path); see require_project_root.
#[allow(clippy::result_large_err)]
pub(crate) fn read_document_from_params<'a>(
    workspace: &'a Workspace,
    params: &serde_json::Value,
    id: u64,
) -> Result<(url::Url, Option<SuppliedDocument<'a>>), Response> {
    let supplied = text_from_params(params).map_err(|rejection| rejection.into_response(id))?;
    if let Some(text) = supplied {
        let path = path_from_params(params)
            .map_err(|rejection| rejection.into_response(id))?
            .ok_or_else(|| {
                rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'text' needs the 'uri' or 'file' it stands for",
                )
            })?;
        if let Ok(inside) = containment::resolve_within_project(workspace, &path) {
            return Err(rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!(
                    "'{}' is inside the project, so the daemon reads it itself; 'text' is only \
                     for a path the daemon may not open",
                    inside.display()
                ),
            ));
        }
        let absolute = if path.is_absolute() {
            path
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        };
        let uri = url::Url::from_file_path(&absolute).map_err(|()| {
            rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!(
                    "supplied path cannot be represented as a file URI: {}",
                    absolute.display()
                ),
            )
        })?;
        workspace
            .documents
            .open(uri.clone(), text)
            .map_err(|error| {
                rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("supplied document was rejected: {error}"),
                )
            })?;
        let guard = SuppliedDocument {
            workspace,
            uri: uri.clone(),
        };
        return Ok((uri, Some(guard)));
    }

    // The raw URI, before containment: a document the editor already opened is
    // answered from the store, exactly as `ensure_document` has always done,
    // and reading it touches no filesystem for containment to guard.
    if let Some(uri) = extract_uri(params) {
        if workspace.documents.contains(&uri) {
            return Ok((uri, None));
        }
    }

    let uri = match file_uri_from_params(workspace, params) {
        Ok(Some(uri)) => uri,
        Ok(None) => return Err(invalid_params(id)),
        Err(rejection) => return Err(rejection.into_response(id)),
    };
    ensure_document(workspace, &uri, id)?;
    Ok((uri, None))
}

/// Load a minimal project rooted at `root` so a test workspace has a
/// containment boundary. `try_write` rather than `write().await` so the same
/// helper serves synchronous and `#[tokio::test]` callers.
#[cfg(test)]
pub(crate) fn set_test_project_root(workspace: &Workspace, root: &Path) {
    *workspace
        .project
        .try_write()
        .expect("test workspace project lock is uncontended") =
        Some(al_project::project::AlProject {
            root: root.to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: "test".to_string(),
                name: "Test".to_string(),
                publisher: "Test".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: root.join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
}

/// Only failures that leave the daemon without a usable workspace abort
/// startup. Per-file ingestion problems (oversized, unreadable, no file URI)
/// are reported as warnings and skipped — see `initialize_daemon_workspace`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DaemonWorkspaceInitError {
    #[error(transparent)]
    Core(#[from] al_workspace::CoreInitError),
}

pub(crate) async fn initialize_daemon_workspace(
    workspace: &Workspace,
    project_root: &Path,
) -> Result<(), DaemonWorkspaceInitError> {
    workspace
        .documents
        .set_max_doc_bytes(workspace.config.read().await.max_document_size_bytes);
    let result = al_workspace::initialize_core_workspace(workspace, project_root).await?;

    tracing::info!(
        files = result.file_count,
        packages = result.package_count,
        symbols = result.total_symbols,
        has_toolchain = result.has_toolchain,
        "daemon: workspace initialization complete"
    );

    // Daemon-specific: open all scanned files in DocumentStore for query access.
    //
    // A single rejected file (most commonly one above `maxDocumentSizeBytes`)
    // used to abort daemon *and* MCP startup entirely. Degrade per file
    // instead: skip it with a warning and keep the rest of the workspace
    // queryable. The file stays in the file index, so syntax-level queries that
    // read from there are unaffected.
    let mut skipped = Vec::new();
    for entry in workspace.file_index.files.iter() {
        let path = entry.key().clone();
        let Ok(uri) = url::Url::from_file_path(&path) else {
            tracing::warn!(
                path = %path.display(),
                "daemon: skipping workspace file that has no file URI"
            );
            skipped.push(path.display().to_string());
            continue;
        };
        if let Err(error) = workspace.documents.open(uri, entry.value().clone()) {
            tracing::warn!(
                path = %path.display(),
                %error,
                "daemon: skipping workspace file rejected by the document store"
            );
            skipped.push(format!("{}: {error}", path.display()));
        }
    }
    if !skipped.is_empty() {
        let sample = skipped
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        let message = format!(
            "{} workspace file(s) were skipped during daemon startup: {sample}{}",
            skipped.len(),
            if skipped.len() > 5 { " …" } else { "" }
        );
        tracing::warn!("{message}");
        if let Some(sink) = workspace.notify_sink.get() {
            sink(&message);
        }
    }
    Ok(())
}

#[cfg(unix)]
#[cfg(test)]
mod tests;
