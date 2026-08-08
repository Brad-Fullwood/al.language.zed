//! Daemon mode — JSON-RPC server over local IPC.
//!
//! `al-lsp daemon --project /path/to/project` starts a daemon that:
//! - Listens on a deterministic local endpoint
//! - Initializes a Workspace for the given project
//! - Accepts JSON-RPC requests and routes them to core queries
//! - Auto-shuts down after 30 minutes of idle
//!
//! # Platform support
//!
//! The transport uses Unix-domain sockets on Linux/macOS and Windows named
//! pipes on Windows through the same `interprocess::local_socket` API. The
//! newline-delimited JSON-RPC framing and request dispatch are identical on
//! every platform.

mod build_dispatch;
mod debug_dispatch;
mod insight_dispatch;
mod lsp_dispatch;

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

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;
const ACCEPT_BACKOFF_START: Duration = Duration::from_millis(10);
const ACCEPT_BACKOFF_CAP: Duration = Duration::from_secs(5);

/// Create `dir` (and parents) restricted to the owner (0o700).
///
/// `DirBuilder::mode` applies the mode only to directories this call creates, so
/// a dir left at a laxer mode by an earlier run (created before this hardening,
/// or under a different umask) would keep its old permissions. We therefore
/// re-assert 0o700 after creation, making the result independent of prior state.
#[cfg(unix)]
fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

pub async fn run_daemon(project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
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
    *workspace.config.write().await = al_project::config::AlConfig::load_effective(&project_root)?;

    let _ = workspace.notify_sink.set(std::sync::Arc::new(|msg: &str| {
        tracing::warn!("daemon: {msg}");
    }));

    initialize_daemon_workspace(&workspace, &project_root).await?;

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
    let idle_timeout_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let elapsed = Duration::from_millis(
                now_activity_ms().saturating_sub(activity_clone.load(Ordering::Relaxed)),
            );
            if elapsed >= IDLE_TIMEOUT {
                // Don't shut down while a request is still being served. The
                // activity timestamp is bumped when a request starts and again
                // when it finishes, but a single operation can legitimately run
                // longer than the whole idle window (a large symbol download, a
                // live-BC snapshot with a long `timeoutMs`), and reaping it
                // mid-flight cut the operation off after only the 10 s drain.
                if in_flight_reaper.load(Ordering::Acquire) > 0 {
                    tracing::info!("daemon: idle timeout skipped (requests in flight)");
                    continue;
                }
                // Don't shut down if a debug session is active.
                //
                // the `try_lock` here is intentional — if the
                // `debug_session` mutex is currently held by another task
                // (mid-RPC) we treat that as "session active" via the
                // `unwrap_or(true)` fallback. The invariant: the only way
                // this mutex is held for >60 ms is during an in-flight
                // debug-session RPC, which by definition means a session
                // exists. If a future contributor ever changes this mutex
                // to be held for long stretches outside debug RPCs, the
                // daemon will never time out — flag it as a deliberate
                // trade-off rather than a bug.
                let has_debug_session = ws_clone
                    .debug_session
                    .try_lock()
                    .map(|g| g.is_some())
                    .unwrap_or(true);
                if has_debug_session {
                    tracing::info!("daemon: idle timeout skipped (debug session active)");
                    continue;
                }
                tracing::info!(
                    idle_secs = elapsed.as_secs(),
                    "daemon: idle timeout, shutting down"
                );
                shutdown_idle.notify_one();
                return;
            }
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
    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::new(reader);

    // previously a 50 ms ring-buffer dedup over hover / completions /
    // signatureHelp / inlayHints replied to repeat requests with `null` /
    // `[]`. Editors that legitimately re-issue these (debounce flush, retry
    // after typing, parallel daemon clients) saw missing-info flicker.
    // Removed entirely — real coalescing requires keeping the request IDs
    // around to replay the completed result, and the workload here is small
    // enough that running the dispatch twice is cheaper than the
    // correctness debt.

    while let Some(line) = read_bounded_line(&mut reader, MAX_MESSAGE_SIZE).await? {
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
                    &mut writer,
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
                    &mut writer,
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
            // JSON-RPC 2.0 §4.1: a notification is processed but MUST NOT be
            // answered.
            continue;
        }

        let frame = match request_id {
            // The common case — an id that round-trips through the dispatcher's
            // `u64` — serializes the typed response directly. Anything else
            // (string, null, negative, or fractional) is echoed verbatim.
            Some(ref id) if id.as_u64().is_some() => serde_json::to_value(&response)?,
            Some(ref id) => response.to_json_with_id(id),
            None => serde_json::to_value(&response)?,
        };
        if !write_frame(&mut writer, &frame).await {
            break;
        }
    }

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

pub(crate) async fn dispatch_request(
    workspace: &std::sync::Arc<Workspace>,
    req: Request,
    shutdown: &Notify,
) -> Response {
    // Dispatch is keyed on u64; string/null ids dispatch under 0 and the
    // connection loop restores the original id on the wire.
    let id = req.dispatch_id();
    let params = req.params.unwrap_or(serde_json::Value::Null);

    match req.method.as_str() {
        "hover" => lsp_dispatch::dispatch_hover(workspace, id, &params).await,
        "definition" => lsp_dispatch::dispatch_definition(workspace, id, &params),
        "references" => lsp_dispatch::dispatch_references(workspace, id, &params),
        "implementations" => lsp_dispatch::dispatch_implementations(workspace, id, &params),
        "completions" => lsp_dispatch::dispatch_completions(workspace, id, &params).await,
        "signatureHelp" => lsp_dispatch::dispatch_signature_help(workspace, id, &params),
        "rename" => lsp_dispatch::dispatch_rename(workspace, id, &params),
        "documentSymbols" => lsp_dispatch::dispatch_document_symbols(workspace, id, &params),
        "foldingRanges" => lsp_dispatch::dispatch_folding_ranges(workspace, id, &params),
        "semanticTokens" => lsp_dispatch::dispatch_semantic_tokens(workspace, id, &params),
        "inlayHints" => lsp_dispatch::dispatch_inlay_hints(workspace, id, &params),
        "codeActions" => lsp_dispatch::dispatch_code_actions(workspace, id, &params),
        "search" => lsp_dispatch::dispatch_search(workspace, id, &params),
        "object" => lsp_dispatch::dispatch_object(workspace, id, &params),
        "byId" => lsp_dispatch::dispatch_by_id(workspace, id, &params),
        "events" => lsp_dispatch::dispatch_events(workspace, id, &params),
        "subscribers" => lsp_dispatch::dispatch_subscribers(workspace, id, &params),
        "composed" => lsp_dispatch::dispatch_composed(workspace, id, &params),
        "packages" => lsp_dispatch::dispatch_packages(workspace, id),
        "deps" => lsp_dispatch::dispatch_deps(workspace, id),
        "lint" => build_dispatch::dispatch_lint(workspace, id, &params).await,
        "format" => build_dispatch::dispatch_format(workspace, id, &params).await,
        "fix" => build_dispatch::dispatch_fix(workspace, id, &params),
        "fix.applicationArea" => {
            build_dispatch::dispatch_fix_application_area(workspace, id, &params)
        }
        "fix.tooltips" => build_dispatch::dispatch_fix_tooltips(workspace, id, &params),
        "fix.dataClassification" => {
            build_dispatch::dispatch_fix_data_classification(workspace, id, &params)
        }
        "rules" => build_dispatch::dispatch_rules(id),
        "parse" => build_dispatch::dispatch_parse(workspace, id, &params),
        "metrics" => build_dispatch::dispatch_metrics(workspace, id, &params),
        "sqlPatterns" => build_dispatch::dispatch_sql_patterns(workspace, id, &params),
        "sortMembers" => build_dispatch::dispatch_sort_members(workspace, id, &params),
        "organizeFiles" => build_dispatch::dispatch_organize_files(workspace, id, &params),
        "source" => build_dispatch::dispatch_source(workspace, id, &params),
        "eventSource" => build_dispatch::dispatch_event_source(workspace, id, &params),
        "location" => build_dispatch::dispatch_location(workspace, id, &params),
        "trace" => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "trace", move || {
                insight_dispatch::dispatch_trace(&ws, id, &args)
            })
            .await
        }
        "entrypoints" => {
            let ws = Arc::clone(workspace);
            offload(id, "entrypoints", move || {
                insight_dispatch::dispatch_entrypoints(&ws, id)
            })
            .await
        }
        "graphExport" => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "graphExport", move || {
                insight_dispatch::dispatch_graph_export(&ws, id, &args)
            })
            .await
        }
        "insightStats" => insight_dispatch::dispatch_insight_stats(workspace, id),
        "deadCode" => {
            let ws = Arc::clone(workspace);
            offload(id, "deadCode", move || {
                insight_dispatch::dispatch_dead_code(&ws, id)
            })
            .await
        }
        "nativeCheck" => insight_dispatch::dispatch_native_check(workspace, id).await,
        "impact" => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "impact", move || {
                insight_dispatch::dispatch_impact(&ws, id, &args)
            })
            .await
        }
        "tableImpact" => insight_dispatch::dispatch_table_impact(workspace, id, &params),
        "suggestEvent" => {
            let (ws, args) = (Arc::clone(workspace), params.clone());
            offload(id, "suggestEvent", move || {
                insight_dispatch::dispatch_suggest_event(&ws, id, &args)
            })
            .await
        }
        "traceChain" => insight_dispatch::dispatch_trace_chain(workspace, id, &params),
        "eventMap" => insight_dispatch::dispatch_event_map(workspace, id),
        "permissions" => build_dispatch::dispatch_permissions(workspace, id, &params),
        "compile" => build_dispatch::dispatch_compile(workspace, id).await,
        "package" => build_dispatch::dispatch_package(workspace, id).await,
        "newProject" => build_dispatch::dispatch_new_project(id, &params),
        "errorCodes" => build_dispatch::dispatch_error_codes(workspace, id).await,
        "builtinTypes" => build_dispatch::dispatch_builtin_types(workspace, id).await,
        "setup" => build_dispatch::dispatch_setup(workspace, id),
        "clearCache" => build_dispatch::dispatch_clear_cache(id).await,
        "authenticate" => build_dispatch::dispatch_authenticate(workspace, id, &params).await,
        "downloadSymbols" => {
            build_dispatch::dispatch_download_symbols(workspace, id, &params).await
        }
        "debug" => debug_dispatch::dispatch_debug(workspace, id, &params).await,
        "snapshot" => build_dispatch::dispatch_snapshot(id, &params).await,
        "profiling" => build_dispatch::dispatch_profiling(id, &params).await,
        "xlf.generate" => build_dispatch::dispatch_xlf_generate(workspace, id, &params).await,
        "xlf.refresh" => build_dispatch::dispatch_xlf_refresh(workspace, id, &params).await,
        "xlf.untranslated" => build_dispatch::dispatch_xlf_untranslated(id, &params),
        "xlf.suggest" => build_dispatch::dispatch_xlf_suggest(workspace, id, &params).await,
        "tests.discover" => build_dispatch::dispatch_tests_discover(workspace, id),
        "tests.run" => build_dispatch::dispatch_tests_run(workspace, id, &params).await,
        "tests.coverage" => build_dispatch::dispatch_tests_coverage(workspace, id),
        "tests.run_batch" => build_dispatch::dispatch_tests_run_batch(workspace, id, &params).await,
        "tests.run_auto" => build_dispatch::dispatch_tests_run_auto(workspace, id, &params).await,
        "tests.last_results" => {
            build_dispatch::dispatch_tests_last_results(workspace, id, &params).await
        }
        "tests.affected" => build_dispatch::dispatch_tests_affected(workspace, id, &params),
        "tests.classify" => build_dispatch::dispatch_tests_classify(workspace, id),
        "tests.snapshot_validate" => {
            build_dispatch::dispatch_tests_snapshot_validate(workspace, id, &params).await
        }
        "tests.snapshot_capture" => {
            build_dispatch::dispatch_tests_snapshot_capture(workspace, id, &params).await
        }
        "tests.snapshot_replay" => {
            build_dispatch::dispatch_tests_snapshot_replay(workspace, id, &params).await
        }
        "tests.snapshot_diff" => {
            build_dispatch::dispatch_tests_snapshot_diff(workspace, id, &params).await
        }
        "tests.mutate" => build_dispatch::dispatch_tests_mutate(workspace, id, &params).await,
        "generate" => build_dispatch::dispatch_generate(workspace, id, &params),
        "obsolete" => build_dispatch::dispatch_obsolete(workspace, id),
        "audit.dataClassification" => {
            build_dispatch::dispatch_audit_data_classification(workspace, id)
        }
        "permissions.audit" => build_dispatch::dispatch_permission_set_audit(workspace, id),
        "deps.graph" => build_dispatch::dispatch_deps_graph(workspace, id, &params).await,
        "breaking" => build_dispatch::dispatch_breaking_changes(workspace, id, &params).await,
        "arch.lint" => build_dispatch::dispatch_arch_lint(workspace, id).await,
        "duplicates" => build_dispatch::dispatch_find_duplicates(workspace, id, &params),
        "upgrade" => build_dispatch::dispatch_upgrade_report(workspace, id, &params).await,
        "profiler.hints" => build_dispatch::dispatch_profiler_hints(workspace, id, &params),
        "diag" => dispatch_diag(workspace, id, &params),
        "ping" => Response {
            id,
            result: Some(serde_json::json!("pong")),
            error: None,
            ..Default::default()
        },
        "shutdown" => {
            tracing::info!("daemon: shutdown requested");
            shutdown.notify_one();
            Response {
                id,
                result: Some(serde_json::json!({"shutdownRequested": true})),
                error: None,
                ..Default::default()
            }
        }
        "status" => {
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
            });
            Response {
                id,
                result: Some(status),
                error: None,
                ..Default::default()
            }
        }
        _ => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("Unknown method: {}", req.method),
            }),
            ..Default::default()
        },
    }
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
                Ok(value) => Response {
                    id,
                    result: Some(value),
                    error: None,
                    ..Default::default()
                },
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
        Err(error) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("Failed to serialize {method} response: {error}"),
        ),
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

/// Ensure a file is loaded in the document store. If not found, read it through
/// the same bounded, regular-file-only ingestion path used by workspace scans.
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
    let read_result = match tokio::runtime::Handle::try_current() {
        Ok(handle)
            if matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(|| al_source::file_index::read_source_file(&path))
        }
        // Synchronous/unit-test callers and current-thread runtimes cannot use
        // block_in_place. The dispatcher API is synchronous, so perform the
        // bounded read directly rather than panicking.
        _ => al_source::file_index::read_source_file(&path),
    };
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

pub(crate) fn file_uri_from_params(params: &serde_json::Value) -> Result<Option<url::Url>, String> {
    // Daemon file operations accept exactly one existing local regular file.
    // Failing canonicalisation used to fall back to the unresolved path, which
    // made missing files, inaccessible parents, and symlink failures look like
    // a valid request until a later and often unrelated operation failed.
    let uri_value = params.get("uri");
    let file_value = params.get("file");
    if uri_value.is_some() && file_value.is_some() {
        return Err("'uri' and 'file' are mutually exclusive".to_string());
    }

    let path = match (uri_value, file_value) {
        (Some(value), None) => {
            let raw = value
                .as_str()
                .ok_or_else(|| "'uri' must be a string when supplied".to_string())?;
            if raw.trim().is_empty() {
                return Err("'uri' must not be empty".to_string());
            }
            let uri = url::Url::parse(raw).map_err(|error| format!("invalid 'uri': {error}"))?;
            uri.to_file_path()
                .map_err(|()| "'uri' must identify a local file".to_string())?
        }
        (None, Some(value)) => {
            let raw = value
                .as_str()
                .ok_or_else(|| "'file' must be a string when supplied".to_string())?;
            if raw.trim().is_empty() {
                return Err("'file' must not be empty".to_string());
            }
            let path = std::path::Path::new(raw);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|error| format!("resolve current directory failed: {error}"))?
                    .join(path)
            }
        }
        (None, None) => return Ok(None),
        (Some(_), Some(_)) => unreachable!("mutual exclusion checked above"),
    };

    let canonical = path
        .canonicalize()
        .map_err(|error| format!("resolve input file '{}' failed: {error}", path.display()))?;
    if !canonical.is_file() {
        return Err(format!(
            "input path '{}' is not a regular file",
            canonical.display()
        ));
    }
    let uri = url::Url::from_file_path(&canonical).map_err(|()| {
        format!(
            "input file cannot be represented as a file URI: {}",
            canonical.display()
        )
    })?;
    Ok(Some(uri))
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

#[cfg(test)]
mod tests {
    use super::{
        dispatch_diag, dispatch_request, ensure_document, extract_i32, extract_position,
        extract_uri, file_not_found, file_uri_from_params, invalid_params, parse_object_kind,
        read_bounded_line, require_document_text, require_project_root, rpc_error,
    };
    use al_protocol::jsonrpc::{error_codes, Request};
    use futures::FutureExt;
    use std::collections::BTreeSet;
    use tokio::sync::Notify;

    fn dispatched_method_literals(source: &str) -> BTreeSet<String> {
        let dispatch = source
            .split_once("match req.method.as_str() {")
            .expect("dispatch_request method match")
            .1
            .split_once("\n        _ => Response {")
            .expect("dispatch_request unknown-method arm")
            .0;
        dispatch
            .lines()
            .filter_map(|line| line.strip_prefix("        \""))
            .filter_map(|line| line.split_once('"').map(|(method, _)| method))
            .map(str::to_string)
            .collect()
    }

    /// The daemon reference and MCP's generic `al_call` promise the complete
    /// dispatcher, not a hand-picked subset. Keep the human reference pinned
    /// directly to the executable method match so newly registered methods
    /// cannot become undocumented agent-only knowledge.
    #[test]
    fn daemon_reference_names_every_dispatched_method() {
        let source = include_str!("mod.rs");
        let methods = dispatched_method_literals(source);
        assert!(
            methods.len() >= 80,
            "dispatcher extraction unexpectedly found only {} methods",
            methods.len()
        );
        let reference = include_str!("../../../../../Docs/reference/daemon-methods.md");
        let missing = methods
            .iter()
            .filter(|method| !reference.contains(&format!("`{method}`")))
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            missing.is_empty(),
            "Docs/reference/daemon-methods.md omits dispatched methods: {missing:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_creates_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("al-lsp");
        super::ensure_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "newly created dir must be 0o700");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_tightens_preexisting_lax_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("al-lsp");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::ensure_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "pre-existing 0o755 dir must be tightened to 0o700"
        );
    }

    #[test]
    fn extract_i32_accepts_in_range() {
        let params = serde_json::json!({ "id": 50_100 });
        assert_eq!(extract_i32(&params, "id"), Some(50_100));
        let params = serde_json::json!({ "id": -1 });
        assert_eq!(extract_i32(&params, "id"), Some(-1));
        let params = serde_json::json!({ "id": i32::MAX });
        assert_eq!(extract_i32(&params, "id"), Some(i32::MAX));
        let params = serde_json::json!({ "id": i32::MIN });
        assert_eq!(extract_i32(&params, "id"), Some(i32::MIN));
    }

    #[test]
    fn extract_i32_rejects_overflow() {
        let params = serde_json::json!({ "id": (i32::MAX as i64) + 1 });
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": (i32::MIN as i64) - 1 });
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": u64::MAX });
        assert_eq!(extract_i32(&params, "id"), None);
    }

    #[test]
    fn extract_i32_rejects_missing_or_wrong_type() {
        let params = serde_json::json!({});
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": "fifty" });
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": 3.5 });
        assert_eq!(extract_i32(&params, "id"), None);
    }

    #[test]
    fn extract_position_rejects_overflow() {
        let params = serde_json::json!({ "line": (u32::MAX as u64) + 1, "character": 0 });
        assert!(extract_position(&params).is_none());
    }

    #[test]
    fn socket_path_is_deterministic() {
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp");
        let p = std::path::Path::new("/tmp");
        let path1 = al_protocol::socket_path(p)
            .expect("socket_path returned None with XDG_RUNTIME_DIR set");
        let path2 = al_protocol::socket_path(p)
            .expect("socket_path returned None with XDG_RUNTIME_DIR set");
        assert_eq!(path1, path2);
        assert!(path1.to_str().unwrap().ends_with(".sock"));
        let filename = path1.file_name().unwrap().to_str().unwrap();
        let hash_part = filename.strip_suffix(".sock").unwrap();
        assert_eq!(hash_part.len(), 16);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn bounded_read_accepts_line_within_limit() {
        let input = b"hello world\n";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, Some("hello world".to_string()));
    }

    #[tokio::test]
    async fn bounded_read_rejects_line_exceeding_limit() {
        let input = b"0123456789";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let err = read_bounded_line(&mut reader, 5).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn bounded_read_returns_none_on_empty_eof() {
        let input: &[u8] = b"";
        let mut reader = tokio::io::BufReader::new(input);
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn bounded_read_rejects_line_with_newline_exceeding_limit() {
        let input = b"0123456789\nmore data";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let err = read_bounded_line(&mut reader, 5).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn bounded_read_returns_partial_line_on_eof() {
        let input = b"no newline here";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, Some("no newline here".to_string()));
    }

    #[test]
    fn file_uri_accepts_existing_local_uri() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.al");
        std::fs::write(&file, b"x").unwrap();
        let params = serde_json::json!({
            "uri": url::Url::from_file_path(&file).unwrap(),
        });
        let uri = file_uri_from_params(&params)
            .expect("uri must be valid")
            .expect("uri must be present");
        assert_eq!(uri.to_file_path().unwrap(), file.canonicalize().unwrap());
    }

    #[test]
    fn file_uri_canonicalizes_absolute_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.al");
        std::fs::write(&file, b"x").unwrap();
        let params = serde_json::json!({ "file": file.to_str().unwrap() });
        let uri = file_uri_from_params(&params)
            .expect("absolute file path must be valid")
            .expect("absolute file path must produce a uri");
        let canon = file.canonicalize().unwrap();
        assert_eq!(uri.to_file_path().unwrap(), canon);
    }

    #[test]
    fn file_uri_resolves_existing_relative_path_against_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&cwd).unwrap();
        let file = dir.path().join("relative.al");
        std::fs::write(&file, b"x").unwrap();
        let relative = file.strip_prefix(&cwd).unwrap();
        let params = serde_json::json!({ "file": relative });
        let uri = file_uri_from_params(&params)
            .expect("relative path must be valid")
            .expect("relative path must produce a uri");
        let path = uri.to_file_path().unwrap();
        assert_eq!(path, file.canonicalize().unwrap());
    }

    #[test]
    fn file_uri_rejects_missing_path_instead_of_falling_back() {
        let params = serde_json::json!({
            "file": "/definitely/not/existing/al-test-xyz.al"
        });
        let error = file_uri_from_params(&params).expect_err("nonexistent path must be rejected");
        assert!(
            error.contains("resolve input file"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn file_uri_returns_absent_without_uri_or_file() {
        let params = serde_json::json!({ "something": "else" });
        assert_eq!(file_uri_from_params(&params).unwrap(), None);
    }

    #[test]
    fn file_uri_rejects_ambiguous_or_malformed_inputs() {
        for params in [
            serde_json::json!({"uri": "file:///tmp/x.al", "file": "/tmp/x.al"}),
            serde_json::json!({"uri": 7}),
            serde_json::json!({"uri": "not a url"}),
            serde_json::json!({"uri": "https://example.com/Test.al"}),
            serde_json::json!({"file": false}),
            serde_json::json!({"file": "  "}),
        ] {
            assert!(
                file_uri_from_params(&params).is_err(),
                "malformed input must be rejected: {params}"
            );
        }
    }

    #[tokio::test]
    async fn dispatch_ping_returns_pong() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let req = Request::new(7, "ping", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 7);
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!("pong")));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_is_method_not_found() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let req = Request::new(11, "definitelyNotAMethod", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 11);
        assert!(resp.result.is_none());
        let err = resp.error.expect("unknown method must yield an error");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert!(
            err.message.contains("definitelyNotAMethod"),
            "message should name the method: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn dispatch_status_reports_pid() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let req = Request::new(3, "status", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 3);
        assert!(resp.error.is_none());
        let result = resp.result.expect("status must return a result");
        assert_eq!(
            result.get("pid").and_then(|v| v.as_u64()),
            Some(u64::from(std::process::id()))
        );
        assert_eq!(
            result.get("indexedSymbols").and_then(|v| v.as_u64()),
            Some(0)
        );
        assert!(
            result
                .get("semanticCache")
                .is_some_and(serde_json::Value::is_object),
            "a healthy cache must report concrete statistics"
        );
    }

    #[tokio::test]
    async fn dispatch_status_rejects_poisoned_inventory_locks() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let poison_target = std::sync::Arc::clone(&ws);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target
                .semantic_cache
                .write()
                .expect("lock starts healthy");
            panic!("poison semantic-cache lock for status regression");
        })
        .join();

        let shutdown = Notify::new();
        let response = dispatch_request(&ws, Request::new(4, "status", None), &shutdown).await;
        let error = response
            .error
            .expect("poisoned status inventory must not be reported as empty");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert!(error.message.contains("poisoned"), "got: {}", error.message);
    }

    #[tokio::test]
    async fn dispatch_shutdown_signals_notify_and_acks() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let notified = shutdown.notified();
        tokio::pin!(notified);
        assert!(
            notified.as_mut().now_or_never().is_none(),
            "notify should not be pre-signalled"
        );

        let req = Request::new(99, "shutdown", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 99);
        assert!(resp.error.is_none());
        assert_eq!(
            resp.result,
            Some(serde_json::json!({"shutdownRequested": true}))
        );

        assert!(
            notified.as_mut().now_or_never().is_some(),
            "shutdown must have signalled the Notify"
        );
    }

    #[tokio::test]
    async fn dispatch_diag_defaults_to_summary_when_params_absent() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let req = Request::new(5, "diag", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 5);
        assert!(
            resp.error.is_none(),
            "diag summary must succeed: {:?}",
            resp.error
        );
        assert!(resp.result.is_some());
    }

    #[test]
    fn dispatch_diag_summary_serializes_memory_stats() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let resp = dispatch_diag(&ws, 1, &serde_json::json!({ "cmd": "summary" }));
        assert_eq!(resp.id, 1);
        assert!(resp.error.is_none());
        let result = resp.result.expect("summary must return memory stats");
        assert!(
            result.is_object(),
            "memory stats serialize to a JSON object"
        );
    }

    #[test]
    fn dispatch_diag_rejects_unknown_subcommand() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let resp = dispatch_diag(&ws, 2, &serde_json::json!({ "cmd": "bogus" }));
        assert_eq!(resp.id, 2);
        assert!(resp.result.is_none());
        let err = resp.error.expect("unknown subcommand must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("bogus"));
    }

    #[test]
    fn dispatch_diag_rejects_non_string_subcommand() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let resp = dispatch_diag(&ws, 3, &serde_json::json!({ "cmd": false }));
        let err = resp.error.expect("wrong-type subcommand must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("'cmd'"));
    }

    #[test]
    fn parse_object_kind_rejects_garbage_with_invalid_params() {
        // A non-AL kind string must map to an INVALID_PARAMS error Response
        // that names the bad input — never panic, never default silently.
        let err = parse_object_kind(8, "notakind").expect_err("garbage kind must be rejected");
        assert_eq!(err.id, 8);
        let rpc = err.error.expect("must carry an RpcError");
        assert_eq!(rpc.code, error_codes::INVALID_PARAMS);
        assert!(rpc.message.contains("notakind"));
    }

    #[test]
    fn require_project_root_errors_when_no_project_loaded() {
        // A fresh workspace has no project; require_project_root must return
        // an INTERNAL_ERROR Response rather than a path.
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let err = require_project_root(&ws, 4).expect_err("no project => Err");
        assert_eq!(err.id, 4);
        let rpc = err.error.expect("must carry an RpcError");
        assert_eq!(rpc.code, error_codes::INTERNAL_ERROR);
    }

    /// JSON-RPC 2.0 ids may be strings; the daemon must dispatch them and echo
    /// the id back unchanged instead of answering `-32700`.
    #[tokio::test]
    async fn string_and_null_request_ids_are_dispatched_and_echoed() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();

        let request: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"call-7","method":"ping"}"#)
                .expect("a string id must deserialize");
        let id = request.id.clone().expect("id present");
        let response = dispatch_request(&ws, request, &shutdown).await;
        let frame = response.to_json_with_id(&id);
        assert_eq!(frame["id"], serde_json::json!("call-7"));
        assert_eq!(frame["result"], serde_json::json!("pong"));

        // A negative id is legal JSON-RPC but does not fit the dispatcher's
        // u64, so it must be echoed verbatim rather than answered with 0.
        let request: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":-3,"method":"ping"}"#)
            .expect("a negative id must deserialize");
        let id = request.id.clone().expect("id present");
        assert!(id.as_u64().is_none());
        let response = dispatch_request(&ws, request, &shutdown).await;
        assert_eq!(response.to_json_with_id(&id)["id"], serde_json::json!(-3));

        let request: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#)
                .expect("a null id must deserialize");
        assert!(
            !request.is_notification(),
            "an explicit null id is a request, not a notification"
        );
        let id = request.id.clone().expect("id present");
        let response = dispatch_request(&ws, request, &shutdown).await;
        assert!(response.to_json_with_id(&id)["id"].is_null());
    }

    #[test]
    fn in_flight_guard_tracks_dispatch_and_restores_on_drop() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let _first = super::InFlightGuard::new(&counter);
            assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 1);
            {
                let _second = super::InFlightGuard::new(&counter);
                assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 2);
            }
            assert_eq!(
                counter.load(std::sync::atomic::Ordering::Acquire),
                1,
                "a finished request must release its in-flight slot"
            );
        }
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Acquire),
            0,
            "the idle reaper must see zero once every request completes"
        );
    }

    #[test]
    fn a_message_without_an_id_is_a_notification() {
        let notification: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"ping"}"#).expect("valid request");
        assert!(notification.is_notification());
    }

    /// Valid JSON that is not a valid request object is `-32600`, not `-32700`.
    #[test]
    fn malformed_request_objects_are_invalid_request_not_parse_error() {
        // Valid JSON, but `method` is missing.
        let value: serde_json::Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":4}"#).expect("valid JSON");
        assert!(serde_json::from_value::<Request>(value).is_err());
        // Whereas this is not JSON at all.
        assert!(serde_json::from_str::<serde_json::Value>("{not json").is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn project_root_wait_reports_busy_instead_of_no_project() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        // Hold the project write lock for longer than the wait window.
        let holder = std::sync::Arc::clone(&ws);
        let guard = holder.project.write().await;
        let ws_for_task = std::sync::Arc::clone(&ws);
        let probe =
            tokio::task::spawn_blocking(move || super::project_root_with_wait(&ws_for_task));
        let error = probe.await.expect("probe joins").expect_err("lock held");
        assert!(error.contains("busy"), "unexpected error: {error}");
        drop(guard);

        // Once released, the same call reports "no project loaded" (Ok(None)),
        // which is a different condition from "busy".
        let ws_for_task = std::sync::Arc::clone(&ws);
        let resolved =
            tokio::task::spawn_blocking(move || super::project_root_with_wait(&ws_for_task))
                .await
                .expect("probe joins")
                .expect("lock is free");
        assert!(resolved.is_none());
    }

    // al-workspace's core initializer uses `block_in_place`, so this needs the
    // multi-threaded flavor (the daemon itself always runs multi-threaded).
    #[tokio::test(flavor = "multi_thread")]
    async fn oversized_workspace_files_are_skipped_instead_of_aborting_startup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-0000000000aa",
                "name": "Skip test",
                "publisher": "Tests",
                "version": "1.0.0.0",
                "dependencies": [],
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Small.Codeunit.al"),
            "codeunit 50100 Small\n{\n}\n",
        )
        .unwrap();
        let big = "codeunit 50101 Big\n{\n}\n".to_string() + &" ".repeat(4096);
        std::fs::write(dir.path().join("Big.Codeunit.al"), &big).unwrap();

        let ws = al_workspace::Workspace::new();
        ws.config.write().await.max_document_size_bytes = Some(128);
        super::initialize_daemon_workspace(&ws, dir.path())
            .await
            .expect("one oversized file must not abort daemon startup");

        let small = url::Url::from_file_path(dir.path().join("Small.Codeunit.al")).unwrap();
        assert!(
            ws.documents.contains(&small),
            "the rest of the workspace must still be queryable"
        );
        let oversized = url::Url::from_file_path(dir.path().join("Big.Codeunit.al")).unwrap();
        assert!(
            !ws.documents.contains(&oversized),
            "the oversized file must be skipped, not opened"
        );
    }

    #[test]
    fn ensure_document_loads_file_from_disk_then_serves_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("On.al");
        std::fs::write(&file, b"codeunit 50000 Foo {}").unwrap();
        let uri = url::Url::from_file_path(&file).unwrap();

        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        assert!(ws.documents.get_text(&uri).is_none());

        // ensure_document uses block_in_place, which requires a multi-thread
        // runtime context.
        let rt = tokio::runtime::Builder::new_multi_thread().build().unwrap();
        let text = rt.block_on(async { require_document_text(&ws, &uri, 1).await });
        assert_eq!(text.unwrap(), "codeunit 50000 Foo {}");
        assert_eq!(
            ws.documents.get_text(&uri).as_deref(),
            Some("codeunit 50000 Foo {}")
        );
    }

    #[test]
    fn require_document_text_returns_file_not_found_for_missing_file() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let uri = url::Url::parse("file:///no/such/al-file-xyz.al").unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().build().unwrap();
        let err = rt.block_on(async { require_document_text(&ws, &uri, 6).await.unwrap_err() });
        assert_eq!(err.id, 6);
        let rpc = err.error.expect("must carry an RpcError");
        assert_eq!(rpc.code, error_codes::FILE_NOT_FOUND);
    }

    #[test]
    fn ensure_document_returns_invalid_params_for_non_file_uri() {
        // A non-file URI has no filesystem path; ensure_document must return
        // a concrete protocol error rather than silently failing.
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let uri = url::Url::parse("https://example.com/x.al").unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().build().unwrap();
        let response = rt
            .block_on(async { ensure_document(&ws, &uri, 7) })
            .expect_err("non-file URI must be rejected");
        assert_eq!(response.id, 7);
        assert_eq!(
            response.error.expect("RPC error").code,
            error_codes::INVALID_PARAMS
        );
    }

    #[test]
    fn extract_uri_parses_valid_file_url() {
        let params = serde_json::json!({ "uri": "file:///a/b.al" });
        let uri = extract_uri(&params).expect("a valid file URL must parse");
        assert_eq!(uri.as_str(), "file:///a/b.al");
        assert_eq!(uri.scheme(), "file");
    }

    #[test]
    fn extract_uri_returns_none_when_uri_missing_or_unparseable() {
        assert!(extract_uri(&serde_json::json!({})).is_none());
        assert!(extract_uri(&serde_json::json!({ "uri": 42 })).is_none());
        // Present string but not a parseable URL (no scheme → relative-ref error).
        assert!(extract_uri(&serde_json::json!({ "uri": "not a url" })).is_none());
    }

    #[test]
    fn extract_position_parses_valid_line_and_character() {
        let params = serde_json::json!({ "line": 12, "character": 34 });
        let pos = extract_position(&params).expect("valid coords must parse");
        assert_eq!(pos.line, 12);
        assert_eq!(pos.character, 34);
    }

    #[test]
    fn extract_position_returns_none_when_a_field_is_missing() {
        assert!(extract_position(&serde_json::json!({ "line": 1 })).is_none());
        assert!(extract_position(&serde_json::json!({ "character": 1 })).is_none());
        assert!(extract_position(&serde_json::json!({})).is_none());
    }

    #[test]
    fn extract_position_rejects_character_overflow() {
        // line in range, character out of u32 range — must reject the whole
        // position rather than truncate the character.
        let params = serde_json::json!({ "line": 0, "character": (u32::MAX as u64) + 1 });
        assert!(extract_position(&params).is_none());
    }

    #[test]
    fn invalid_params_carries_invalid_params_code_and_id() {
        let resp = invalid_params(42);
        assert_eq!(resp.id, 42);
        assert!(resp.result.is_none());
        let err = resp.error.expect("invalid_params must carry an error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(!err.message.is_empty());
    }

    #[test]
    fn file_not_found_carries_file_not_found_code_and_id() {
        let resp = file_not_found(13);
        assert_eq!(resp.id, 13);
        assert!(resp.result.is_none());
        let err = resp.error.expect("file_not_found must carry an error");
        assert_eq!(err.code, error_codes::FILE_NOT_FOUND);
    }

    #[test]
    fn rpc_error_preserves_code_message_and_id() {
        let resp = rpc_error(77, error_codes::INTERNAL_ERROR, "boom");
        assert_eq!(resp.id, 77);
        assert!(resp.result.is_none());
        let err = resp.error.expect("rpc_error must carry an error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert_eq!(err.message, "boom");
    }

    /// `rules` is a static query (lint rule catalogue) needing no project; it
    /// must route through `dispatch_request` and return a JSON array result
    /// with no error. The catalogue combines file-local, transaction,
    /// native-check, and workspace-native rule registries; this test pins the
    /// transport shape while the registry-specific test pins its contents.
    #[tokio::test]
    async fn dispatch_rules_returns_array_result() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(21, "rules", None), &shutdown).await;
        assert_eq!(resp.id, 21);
        assert!(resp.error.is_none(), "rules must succeed: {:?}", resp.error);
        assert!(
            resp.result.expect("rules must return a result").is_array(),
            "rules result must be a JSON array"
        );
    }

    #[tokio::test]
    async fn dispatch_packages_returns_empty_array_on_fresh_workspace() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(22, "packages", None), &shutdown).await;
        assert_eq!(resp.id, 22);
        assert!(resp.error.is_none());
        let arr = resp.result.expect("packages result").as_array().cloned();
        assert_eq!(arr, Some(vec![]));
    }

    /// `entrypoints` builds the insight graph lazily; on a fresh (empty)
    /// workspace it must still succeed and return a JSON array.
    #[tokio::test]
    async fn dispatch_entrypoints_succeeds_on_empty_workspace() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(23, "entrypoints", None), &shutdown).await;
        assert_eq!(resp.id, 23);
        assert!(resp.error.is_none());
        assert!(resp.result.expect("entrypoints result").is_array());
    }

    /// `hover` requires uri + position; with null params (the default when the
    /// wire omits `params`) it must route through and surface INVALID_PARAMS,
    /// proving both the routing entry and the shared `invalid_params` helper.
    #[tokio::test]
    async fn dispatch_hover_without_params_is_invalid_params() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(31, "hover", None), &shutdown).await;
        assert_eq!(resp.id, 31);
        assert!(resp.result.is_none());
        let err = resp.error.expect("missing hover params must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    /// Routing for synchronous LSP methods that also validate params. Each must
    /// be reachable via `dispatch_request` and return INVALID_PARAMS for the
    /// empty-params case — this covers a swath of the routing table at once.
    #[tokio::test]
    async fn dispatch_param_validating_methods_route_and_reject_empty_params() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        for method in [
            "definition",
            "references",
            "implementations",
            "signatureHelp",
            "rename",
            "documentSymbols",
            "foldingRanges",
            "semanticTokens",
            "lint",
            "format",
            "trace",
        ] {
            let req = Request::new(40, method, None);
            let resp = dispatch_request(&ws, req, &shutdown).await;
            assert_eq!(resp.id, 40, "{method}: id must be preserved");
            assert!(
                resp.result.is_none(),
                "{method}: empty params must not yield a result"
            );
            let err = resp
                .error
                .unwrap_or_else(|| panic!("{method}: empty params must error"));
            assert_eq!(
                err.code,
                error_codes::INVALID_PARAMS,
                "{method}: expected INVALID_PARAMS, got code {}",
                err.code
            );
        }
    }

    /// `object` requires a `kind` param; an unknown kind string must route
    /// through `parse_object_kind` and surface INVALID_PARAMS naming the input.
    #[tokio::test]
    async fn dispatch_object_with_bad_kind_is_invalid_params() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let params = serde_json::json!({ "kind": "notakind", "name": "X" });
        let req = Request::new(45, "object", Some(params));
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 45);
        let err = resp.error.expect("bad kind must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("notakind"));
    }

    /// The request id must be threaded through to the response for the
    /// not-found path too — a regression here would mismatch client futures.
    #[tokio::test]
    async fn dispatch_preserves_request_id_on_unknown_method() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(9_999, "nope.nope", None), &shutdown).await;
        assert_eq!(resp.id, 9_999);
        assert_eq!(
            resp.error.expect("unknown must error").code,
            error_codes::METHOD_NOT_FOUND
        );
    }
}
