//! Daemon mode — JSON-RPC server over Unix socket.
//!
//! `al-lsp daemon --project /path/to/project` starts a daemon that:
//! - Listens on a deterministic Unix socket path
//! - Initializes a Workspace for the given project
//! - Accepts JSON-RPC requests and routes them to core queries
//! - Auto-shuts down after 30 minutes of idle
//!
//! # Platform support
//!
//! The daemon transport is **Unix-only**: it binds an `AF_UNIX` socket
//! (`tokio::net::UnixListener`) and its client (`al_protocol::client`,
//! also `#[cfg(unix)]`) connects over `std::os::unix::net::UnixStream`.
//! Windows has no equivalent here, so the socket-bound transport
//! (`run_daemon`, `handle_connection`, the `SocketCleanup` guard) is
//! gated behind `#[cfg(unix)]`. On Windows, [`run_daemon`]
//! returns an explanatory error.
//!
//! Crucially, the request-dispatch logic (`dispatch_request` and the
//! `*_dispatch` submodules) and the framing helper (`read_bounded_line`)
//! are **platform-independent** and compile everywhere — this is what lets
//! the `al-lsp` binary build for `x86_64-pc-windows-msvc` so the Zed
//! extension can ship a Windows asset while only LSP (`--stdio`) and DAP
//! (`--dap`) modes are wired up there.

// The request-dispatch machinery below (these submodules, `dispatch_request`,
// and the `extract_*`/`require_*` helpers) is pure logic over `Workspace` and
// compiles on every platform. It is, however, only *reachable* through the
// Unix-only socket transport (`run_daemon` → `handle_connection`). On non-Unix
// targets that transport is unavailable, leaving this surface unreferenced, so we
// allow dead code there rather than fragmenting every helper with `#[cfg]`.
// `al-lsp` still ships on Windows for its portable LSP/DAP modes.
#![cfg_attr(not(unix), allow(dead_code))]

mod build_dispatch;
mod debug_dispatch;
mod insight_dispatch;
mod lsp_dispatch;

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::{Duration, Instant};

use al_protocol::jsonrpc::{error_codes, Request, Response, RpcError};
#[cfg(unix)]
use al_protocol::socket_path;
use al_workspace::Workspace;
use tokio::io::AsyncBufReadExt;
#[cfg(unix)]
use tokio::io::{AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::Notify;
#[cfg(unix)]
use tokio::sync::Semaphore;

/// Process-start `Instant` used as the epoch for `last_activity` millis.
///
/// `Instant` is not directly storable in an atomic, so we keep a captured
/// base and store millis-since-base in `AtomicU64`. The base is initialised
/// the first time `now_activity_ms` is called and lives for the process
/// lifetime (lazy `OnceLock`).
#[cfg(unix)]
static DAEMON_EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Millis since the daemon epoch — monotonic, atomic-storable.
#[cfg(unix)]
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

#[cfg(unix)]
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;
#[cfg(unix)]
const MAX_CONNECTIONS: usize = 64;
#[cfg(unix)]
const ACCEPT_BACKOFF_START: Duration = Duration::from_millis(10);
#[cfg(unix)]
const ACCEPT_BACKOFF_CAP: Duration = Duration::from_secs(5);

/// Return an unsupported-platform error for daemon mode on Windows.
///
/// The daemon's IPC transport is an `AF_UNIX` socket, which has no Windows
/// equivalent here, and its only client (`al-explorer` via
/// `al_protocol::client`) is itself `#[cfg(unix)]`. Rather than fail to
/// compile, `al-lsp` builds on Windows while its portable LSP (`--stdio`) and
/// DAP (`--dap`) modes remain available.
#[cfg(not(unix))]
pub async fn run_daemon(_project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "Daemon mode is not supported on this platform: it requires a Unix \
         domain socket (AF_UNIX), which al-lsp only wires up on Unix targets. \
         Use LSP mode (--stdio) or DAP mode (--dap) instead."
            .into(),
    )
}

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

#[cfg(unix)]
pub async fn run_daemon(project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let sock_path = socket_path(&project_root).ok_or(
        "Cannot determine Unix socket path: XDG_RUNTIME_DIR is not set and no secure runtime directory is available"
    )?;

    // Ensure parent directory exists, owner-only (0o700). The socket file is
    // already 0o600 below, but the containing dir defaulted to the process
    // umask (often 0o755) — world-readable, leaking the socket filename (a hash
    // of the project path) to other users on a shared host. See ensure_private_dir.
    if let Some(parent) = sock_path.parent() {
        ensure_private_dir(parent)?;
    }

    let _ = tokio::fs::remove_file(&sock_path).await;

    let listener = UnixListener::bind(&sock_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;
    }
    tracing::info!(path = %sock_path.display(), project = %project_root.display(), "daemon: listening");

    let _ = SOCKET_PATH.set(sock_path.clone());
    let _cleanup = SocketCleanup;

    let workspace = Arc::new(Workspace::new());

    let _ = workspace.notify_sink.set(std::sync::Arc::new(|msg: &str| {
        tracing::warn!("daemon: {msg}");
    }));

    initialize_daemon_workspace(&workspace, &project_root).await;

    // stored as millis-since-`DAEMON_EPOCH` in an AtomicU64 so
    // the hot per-connection-accept + per-dispatch update is lock-free.
    // Previous `Arc<Mutex<Instant>>` serialised every connection at the lock.
    let last_activity = Arc::new(AtomicU64::new(now_activity_ms()));
    let shutdown_signal = Arc::new(Notify::new());

    let activity_clone = Arc::clone(&last_activity);
    let ws_clone = Arc::clone(&workspace);
    let shutdown_idle = Arc::clone(&shutdown_signal);
    let idle_timeout_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let elapsed = Duration::from_millis(
                now_activity_ms().saturating_sub(activity_clone.load(Ordering::Relaxed)),
            );
            if elapsed >= IDLE_TIMEOUT {
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
    #[cfg(unix)]
    let mut sigint = {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::interrupt()).ok()
    };

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
        #[cfg(unix)]
        let sigint_fut = async {
            match sigint.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        #[cfg(not(unix))]
        let sigterm_fut = std::future::pending::<()>();
        #[cfg(not(unix))]
        let sigint_fut = std::future::pending::<()>();

        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
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
                                tracing::warn!("daemon: connection limit ({MAX_CONNECTIONS}) reached, dropping new connection");
                                drop(stream);
                                continue;
                            }
                        };

                        let ws = Arc::clone(&workspace);
                        let activity = Arc::clone(&last_activity);
                        let shutdown_conn = Arc::clone(&shutdown_signal);
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(e) = handle_connection(stream, ws, activity, shutdown_conn).await {
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
            _ = sigint_fut => {
                tracing::info!("daemon: received SIGINT — shutting down gracefully");
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

/// Read a single newline-delimited line, enforcing a byte limit during reading.
/// Returns `Ok(None)` on EOF, `Err` if the line exceeds `max_bytes`.
async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
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

#[cfg(unix)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    workspace: Arc<Workspace>,
    last_activity: Arc<AtomicU64>,
    shutdown: Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, mut writer) = stream.into_split();
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

        // (idle-timer update is deferred until after dispatch_request returns
        // so a long-running query keeps the daemon alive — otherwise the timer
        // updates only at request-arrival, and a 35s build can be killed by
        // the 30s idle reaper.)

        // JSON-RPC 2.0 §5: on parse error the response id MUST be null because
        // the request id is unknown. The typed Response struct uses u64, so we
        // write the parse-error case directly as raw JSON.
        let req = match serde_json::from_str::<Request>(line) {
            Ok(r) => r,
            Err(e) => {
                // Record the parse-error context here. If the write below fails
                // (broken pipe / client gone), `?` would propagate a bare I/O
                // error to `handle_connection` and the original malformed-JSON
                // reason would be lost — only a generic "connection error" would
                // surface. Logging first preserves the diagnostic either way.
                tracing::warn!(error = %e, "daemon: malformed JSON-RPC request");
                let error_obj = serde_json::json!({
                    "id": null,
                    "error": {
                        "code": error_codes::PARSE_ERROR,
                        "message": format!("Invalid JSON-RPC: {}", e),
                    }
                });
                let mut raw = serde_json::to_string(&error_obj).unwrap_or_else(|_| {
                    // Extremely unlikely: the json! macro produces valid JSON.
                    // Fall back to a minimal static error string.
                    format!(
                        r#"{{"id":null,"error":{{"code":{},"message":"Parse error"}}}}"#,
                        error_codes::PARSE_ERROR
                    )
                });
                raw.push('\n');
                // Handle write failure explicitly rather than via `?` so a dead
                // client connection ends this loop cleanly without masking the
                // parse-error context already logged above.
                if let Err(io_err) = writer.write_all(raw.as_bytes()).await {
                    tracing::warn!(error = %io_err, "daemon: failed to send parse-error response");
                    break;
                }
                if let Err(io_err) = writer.flush().await {
                    tracing::warn!(error = %io_err, "daemon: failed to flush parse-error response");
                    break;
                }
                continue;
            }
        };
        let response = {
            let method = req.method.clone();
            let req_id = req.id;
            let start = Instant::now();
            let resp = dispatch_request(&workspace, req, &shutdown).await;
            let elapsed = start.elapsed();
            tracing::debug!(method = %method, id = req_id, elapsed_us = elapsed.as_micros() as u64, "daemon: request");
            // Mark activity AFTER dispatch returns so the idle reaper can't
            // kill the daemon mid-request — a long-running build / download
            // keeps the timer fresh until completion.
            last_activity.store(now_activity_ms(), Ordering::Relaxed);
            resp
        };

        let mut json = serde_json::to_string(&response)?;
        json.push('\n');
        writer.write_all(json.as_bytes()).await?;
        writer.flush().await?;
    }

    Ok(())
}

pub(crate) async fn dispatch_request(
    workspace: &std::sync::Arc<Workspace>,
    req: Request,
    shutdown: &Notify,
) -> Response {
    let id = req.id;
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
        "lint" => build_dispatch::dispatch_lint(workspace, id, &params),
        "format" => build_dispatch::dispatch_format(workspace, id, &params),
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
        "trace" => insight_dispatch::dispatch_trace(workspace, id, &params),
        "entrypoints" => insight_dispatch::dispatch_entrypoints(workspace, id),
        "graphExport" => insight_dispatch::dispatch_graph_export(workspace, id, &params),
        "insightStats" => insight_dispatch::dispatch_insight_stats(workspace, id),
        "deadCode" => insight_dispatch::dispatch_dead_code(workspace, id),
        "nativeCheck" => insight_dispatch::dispatch_native_check(workspace, id),
        "impact" => insight_dispatch::dispatch_impact(workspace, id, &params),
        "tableImpact" => insight_dispatch::dispatch_table_impact(workspace, id, &params),
        "suggestEvent" => insight_dispatch::dispatch_suggest_event(workspace, id, &params),
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
        "tests.snapshot_record" => {
            build_dispatch::dispatch_tests_snapshot_record(workspace, id, &params).await
        }
        "tests.snapshot_replay" => {
            build_dispatch::dispatch_tests_snapshot_replay(id, &params).await
        }
        "tests.snapshot_diff" => build_dispatch::dispatch_tests_snapshot_diff(id, &params).await,
        "tests.mutate" => build_dispatch::dispatch_tests_mutate(workspace, id, &params).await,
        "generate" => build_dispatch::dispatch_generate(workspace, id, &params),
        "obsolete" => build_dispatch::dispatch_obsolete(workspace, id),
        "audit.dataClassification" => {
            build_dispatch::dispatch_audit_data_classification(workspace, id)
        }
        "permissions.audit" => build_dispatch::dispatch_permission_set_audit(workspace, id),
        "deps.graph" => build_dispatch::dispatch_deps_graph(workspace, id, &params),
        "breaking" => build_dispatch::dispatch_breaking_changes(workspace, id, &params),
        "arch.lint" => build_dispatch::dispatch_arch_lint(workspace, id),
        "duplicates" => build_dispatch::dispatch_find_duplicates(workspace, id, &params),
        "upgrade" => build_dispatch::dispatch_upgrade_report(workspace, id, &params),
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
                result: Some(serde_json::json!("ok")),
                error: None,
                ..Default::default()
            }
        }
        "status" => {
            let cache_stats = workspace.semantic_cache.read().ok().map(|c| {
                let (hits, misses) = c.stats();
                serde_json::json!({ "types": c.len(), "hits": hits, "misses": misses, "version": c.version() })
            });
            let status = serde_json::json!({
                "pid": std::process::id(),
                "indexedSymbols": workspace.symbols.len(),
                "workspaceFiles": workspace.file_index.len(),
                "workspaceObjects": workspace.file_index.object_count(),
                "builtinTypes": workspace.builtins.read().ok().map(|g| g.len()).unwrap_or(0),
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
    let cmd = params
        .get("cmd")
        .and_then(|v| v.as_str())
        .unwrap_or("summary");
    match cmd {
        "summary" => {
            let stats = workspace.memory_stats();
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

// Err is a ready-to-send JSON-RPC `Response` by design (callers just return it
// on a cold error path); boxing it would scatter `*` derefs across every
// dispatcher for no real benefit.
#[allow(clippy::result_large_err)]
pub(crate) fn require_project_root(workspace: &Workspace, id: u64) -> Result<PathBuf, Response> {
    workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .ok_or_else(|| rpc_error(id, error_codes::INTERNAL_ERROR, "No project loaded"))
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

/// Get document text, loading from disk if needed. Returns the text or a file-not-found Response.
// Err is a ready-to-send JSON-RPC `Response` (cold path); see require_project_root.
#[allow(clippy::result_large_err)]
pub(crate) fn require_document_text(
    workspace: &Workspace,
    uri: &url::Url,
    id: u64,
) -> Result<String, Response> {
    ensure_document(workspace, uri);
    workspace
        .documents
        .get_text(uri)
        .ok_or_else(|| file_not_found(id))
}

/// Ensure a file is loaded in the document store. If not found, read from disk.
pub(crate) fn ensure_document(workspace: &Workspace, uri: &url::Url) -> Option<()> {
    if workspace.documents.contains(uri) {
        return Some(());
    }
    // Try to read from disk (block_in_place avoids blocking the tokio runtime)
    let path = uri.to_file_path().ok()?;
    let content = tokio::task::block_in_place(|| std::fs::read_to_string(&path)).ok()?;
    workspace.documents.open(uri.clone(), content);
    Some(())
}

pub(crate) fn file_uri_from_params(params: &serde_json::Value) -> Option<url::Url> {
    // Accept either "uri" (file:// URL) or "file" (path string)
    if let Some(uri) = extract_uri(params) {
        return Some(uri);
    }
    if let Some(file_str) = params.get("file").and_then(|v| v.as_str()) {
        let path = std::path::Path::new(file_str);
        let abs_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(path)
        };
        let canon = abs_path.canonicalize().unwrap_or(abs_path);
        return url::Url::from_file_path(canon).ok();
    }
    None
}

pub(crate) fn lint_diag_to_json(d: &al_syntax::LintDiagnostic) -> serde_json::Value {
    serde_json::json!({
        "code": d.code,
        "message": d.message,
        "severity": d.severity.to_string(),
        "line": d.range.start_point.row + 1,
        "column": d.range.start_point.column + 1,
        "endLine": d.range.end_point.row + 1,
        "endColumn": d.range.end_point.column + 1,
    })
}

pub(crate) async fn initialize_daemon_workspace(workspace: &Workspace, project_root: &Path) {
    let result = al_workspace::initialize_core_workspace(workspace, project_root).await;

    tracing::info!(
        files = result.file_count,
        packages = result.package_count,
        symbols = result.total_symbols,
        has_toolchain = result.has_toolchain,
        "daemon: workspace initialization complete"
    );

    // Daemon-specific: open all scanned files in DocumentStore for query access.
    for entry in workspace.file_index.files.iter() {
        if let Ok(uri) = url::Url::from_file_path(entry.key()) {
            workspace.documents.open(uri, entry.value().clone());
        }
    }
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
    use tokio::sync::Notify;

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
    fn file_uri_prefers_explicit_uri_field() {
        let params = serde_json::json!({
            "uri": "file:///some/where.al",
            "file": "/other/path.al",
        });
        let uri = file_uri_from_params(&params).expect("uri must parse");
        assert_eq!(uri.as_str(), "file:///some/where.al");
    }

    #[test]
    fn file_uri_canonicalizes_absolute_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.al");
        std::fs::write(&file, b"x").unwrap();
        let params = serde_json::json!({ "file": file.to_str().unwrap() });
        let uri = file_uri_from_params(&params).expect("absolute file path must produce a uri");
        let canon = file.canonicalize().unwrap();
        assert_eq!(uri.to_file_path().unwrap(), canon);
    }

    #[test]
    fn file_uri_resolves_relative_path_against_cwd() {
        let params = serde_json::json!({ "file": "relative/file.al" });
        let uri = file_uri_from_params(&params).expect("relative path must produce a uri");
        let path = uri.to_file_path().unwrap();
        assert!(
            path.is_absolute(),
            "resolved path must be absolute: {path:?}"
        );
        assert!(path.ends_with("relative/file.al"));
    }

    #[test]
    fn file_uri_falls_back_when_canonicalize_fails() {
        let params = serde_json::json!({
            "file": "/definitely/not/existing/al-test-xyz.al"
        });
        let uri = file_uri_from_params(&params).expect("nonexistent path must still produce a uri");
        assert_eq!(
            uri.to_file_path().unwrap(),
            std::path::Path::new("/definitely/not/existing/al-test-xyz.al")
        );
    }

    #[test]
    fn file_uri_returns_none_without_uri_or_file() {
        let params = serde_json::json!({ "something": "else" });
        assert!(file_uri_from_params(&params).is_none());
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
        assert_eq!(resp.result, Some(serde_json::json!("ok")));

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
        let text = rt.block_on(async { require_document_text(&ws, &uri, 1) });
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
        let err = rt.block_on(async { require_document_text(&ws, &uri, 6).unwrap_err() });
        assert_eq!(err.id, 6);
        let rpc = err.error.expect("must carry an RpcError");
        assert_eq!(rpc.code, error_codes::FILE_NOT_FOUND);
    }

    #[test]
    fn ensure_document_returns_none_for_non_file_uri() {
        // A non-file URI has no filesystem path; ensure_document must return
        // None rather than panicking.
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let uri = url::Url::parse("https://example.com/x.al").unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().build().unwrap();
        let got = rt.block_on(async { ensure_document(&ws, &uri) });
        assert!(got.is_none());
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
    /// with no error. (The catalogue itself is currently empty because all
    /// diagnostics come from the .NET bridge, but the routing + JSON shape
    /// are what we pin here.)
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

    #[test]
    fn lint_diag_to_json_uses_one_based_positions() {
        use al_syntax::{LintDiagnostic, LintSeverity};
        use tree_sitter::{Point, Range};
        let diag = LintDiagnostic {
            code: "AL0001".to_string(),
            message: "bad thing".to_string(),
            severity: LintSeverity::Warning,
            range: Range {
                start_byte: 0,
                end_byte: 5,
                start_point: Point { row: 4, column: 2 },
                end_point: Point { row: 4, column: 7 },
            },
        };
        let json = super::lint_diag_to_json(&diag);
        assert_eq!(json.get("code").and_then(|v| v.as_str()), Some("AL0001"));
        assert_eq!(
            json.get("message").and_then(|v| v.as_str()),
            Some("bad thing")
        );
        // tree-sitter rows/columns are 0-based; the wire format is 1-based.
        assert_eq!(json.get("line").and_then(|v| v.as_u64()), Some(5));
        assert_eq!(json.get("column").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(json.get("endLine").and_then(|v| v.as_u64()), Some(5));
        assert_eq!(json.get("endColumn").and_then(|v| v.as_u64()), Some(8));
    }
}
