//! Daemon mode — JSON-RPC server over Unix socket.
//!
//! `al-lsp daemon --project /path/to/project` starts a daemon that:
//! - Listens on a deterministic Unix socket path
//! - Initializes a Workspace for the given project
//! - Accepts JSON-RPC requests, routes to al-core queries
//! - Auto-shuts down after 30 minutes of idle
//!
//! # Platform support
//!
//! The daemon transport is **Unix-only**: it binds an `AF_UNIX` socket
//! (`tokio::net::UnixListener`) and its client (`al_protocol::client`,
//! also `#[cfg(unix)]`) connects over `std::os::unix::net::UnixStream`.
//! Windows has no equivalent here, so the socket-bound transport
//! (`run_daemon`, `handle_connection`, the `SocketCleanup` guard) is
//! gated behind `#[cfg(unix)]`. On Windows, [`run_daemon`] is a stub that
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
// targets that transport is a stub, leaving this surface unreferenced, so we
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

use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Request, Response, RpcError};
#[cfg(unix)]
use al_protocol::socket_path;
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

/// Global socket path for cleanup on exit.
#[cfg(unix)]
static SOCKET_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Clean up the socket file (called from signal handlers or shutdown).
#[cfg(unix)]
pub(crate) fn cleanup_socket() {
    if let Some(path) = SOCKET_PATH.get() {
        let _ = std::fs::remove_file(path);
        tracing::info!("daemon: socket cleaned up");
    }
}

/// RAII guard that cleans up the socket on drop.
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
const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024; // 64 MB
#[cfg(unix)]
const MAX_CONNECTIONS: usize = 64;
#[cfg(unix)]
const ACCEPT_BACKOFF_START: Duration = Duration::from_millis(10);
#[cfg(unix)]
const ACCEPT_BACKOFF_CAP: Duration = Duration::from_secs(5);

/// Run the daemon server for a project (Windows stub).
///
/// The daemon's IPC transport is an `AF_UNIX` socket, which has no Windows
/// equivalent here, and its only client (`al-explorer` via
/// `al_protocol::client`) is itself `#[cfg(unix)]`. Rather than fail to
/// compile, `al-lsp` builds on Windows with this stub so its portable LSP
/// (`--stdio`) and DAP (`--dap`) modes work; daemon mode returns a clear
/// error if invoked. See the module docs and `ecosystem-roadmap.md` item 8.
#[cfg(not(unix))]
pub async fn run_daemon(_project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "Daemon mode is not supported on this platform: it requires a Unix \
         domain socket (AF_UNIX), which al-lsp only wires up on Unix targets. \
         Use LSP mode (--stdio) or DAP mode (--dap) instead."
            .into(),
    )
}

/// Run the daemon server for a project.
#[cfg(unix)]
pub async fn run_daemon(project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let sock_path = socket_path(&project_root).ok_or(
        "Cannot determine Unix socket path: XDG_RUNTIME_DIR is not set and no secure runtime directory is available"
    )?;

    // Ensure parent directory exists
    if let Some(parent) = sock_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Remove stale socket file if it exists
    let _ = tokio::fs::remove_file(&sock_path).await;

    let listener = UnixListener::bind(&sock_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;
    }
    tracing::info!(path = %sock_path.display(), project = %project_root.display(), "daemon: listening");

    // Register global path for cleanup on exit/signals
    let _ = SOCKET_PATH.set(sock_path.clone());
    let _cleanup = SocketCleanup;

    // Initialize workspace
    let workspace = Arc::new(Workspace::new());

    // Register a logging notify sink — daemon has no LSP client, so warnings go to logs.
    let _ = workspace.notify_sink.set(std::sync::Arc::new(|msg: &str| {
        tracing::warn!("daemon: {msg}");
    }));

    initialize_daemon_workspace(&workspace, &project_root).await;

    // F-OPEN-068: stored as millis-since-`DAEMON_EPOCH` in an AtomicU64 so
    // the hot per-connection-accept + per-dispatch update is lock-free.
    // Previous `Arc<Mutex<Instant>>` serialised every connection at the lock.
    let last_activity = Arc::new(AtomicU64::new(now_activity_ms()));
    let shutdown_signal = Arc::new(Notify::new());

    // Idle timeout checker
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
                // F-OPEN-067: the `try_lock` here is intentional — if the
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

    // Connection semaphore — limits concurrent active connections to avoid FD/memory exhaustion.
    let connection_limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    // Set up OS signal streams so the daemon's select loop can handle them inline.
    // This avoids calling process::exit() from a spawned task, which would skip the
    // SocketCleanup drop guard. Instead, we break out of the accept loop so Drop runs.
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

    // Accept connections — break when shutdown is signalled so Drop guards run.
    let mut accept_backoff = ACCEPT_BACKOFF_START;
    loop {
        // Helper futures that resolve when a signal fires (or never, if registration failed).
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
                        // Reset backoff on success.
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

                        // Acquire a connection slot. If at the limit, drop this connection
                        // rather than blocking the accept loop.
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
                            // Permit is held for the lifetime of the connection task.
                            let _permit = permit;
                            if let Err(e) = handle_connection(stream, ws, activity, shutdown_conn).await {
                                tracing::warn!(error = %e, "daemon: connection error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "daemon: accept error");
                        // Exponential backoff to avoid spinning on persistent errors (e.g. EMFILE).
                        tokio::time::sleep(accept_backoff).await;
                        accept_backoff = (accept_backoff * 2).min(ACCEPT_BACKOFF_CAP);
                    }
                }
            }
            _ = shutdown_signal.notified() => {
                tracing::info!("daemon: idle timeout — shutting down");
                break;
            }
            // Graceful shutdown from OS signals: break so SocketCleanup drops before exit.
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

    // Stop the idle-timeout watcher so it doesn't fire after the accept loop exits.
    idle_timeout_handle.abort();

    // F-OPEN-070: graceful drain. The accept loop has broken; new connections
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

    // F-047: previously a 50 ms ring-buffer dedup over hover / completions /
    // signatureHelp / inlayHints replied to repeat requests with `null` /
    // `[]`. Editors that legitimately re-issue these (debounce flush, retry
    // after typing, parallel daemon clients) saw missing-info flicker.
    // Removed entirely — real coalescing requires keeping the request IDs
    // around to replay the completed result, and the workload here is small
    // enough that running the dispatch twice is cheaper than the
    // correctness debt.

    while let Some(line) = read_bounded_line(&mut reader, MAX_MESSAGE_SIZE).await? {
        let line = line.trim().to_string();
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
        let req = match serde_json::from_str::<Request>(&line) {
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

async fn dispatch_request(workspace: &Workspace, req: Request, shutdown: &Notify) -> Response {
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
        // Symbol queries
        "search" => lsp_dispatch::dispatch_search(workspace, id, &params),
        "object" => lsp_dispatch::dispatch_object(workspace, id, &params),
        "byId" => lsp_dispatch::dispatch_by_id(workspace, id, &params),
        "events" => lsp_dispatch::dispatch_events(workspace, id, &params),
        "subscribers" => lsp_dispatch::dispatch_subscribers(workspace, id, &params),
        "composed" => lsp_dispatch::dispatch_composed(workspace, id, &params),
        "packages" => lsp_dispatch::dispatch_packages(workspace, id),
        "deps" => lsp_dispatch::dispatch_deps(workspace, id),
        // Analysis
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
        "location" => build_dispatch::dispatch_location(workspace, id, &params),
        // Insight engine
        "trace" => insight_dispatch::dispatch_trace(workspace, id, &params),
        "entrypoints" => insight_dispatch::dispatch_entrypoints(workspace, id),
        "graphExport" => insight_dispatch::dispatch_graph_export(workspace, id, &params),
        "insightStats" => insight_dispatch::dispatch_insight_stats(workspace, id),
        "deadCode" => insight_dispatch::dispatch_dead_code(workspace, id),
        "impact" => insight_dispatch::dispatch_impact(workspace, id, &params),
        "suggestEvent" => insight_dispatch::dispatch_suggest_event(workspace, id, &params),
        // Semantic / toolchain
        "permissions" => build_dispatch::dispatch_permissions(workspace, id, &params),
        "compile" => build_dispatch::dispatch_compile(workspace, id).await,
        "package" => build_dispatch::dispatch_package(workspace, id).await,
        "newProject" => build_dispatch::dispatch_new_project(id, &params),
        "errorCodes" => build_dispatch::dispatch_error_codes(workspace, id),
        "builtinTypes" => build_dispatch::dispatch_builtin_types(workspace, id),
        "setup" => build_dispatch::dispatch_setup(workspace, id),
        "clearCache" => build_dispatch::dispatch_clear_cache(id).await,
        "authenticate" => build_dispatch::dispatch_authenticate(workspace, id, &params).await,
        "downloadSymbols" => {
            build_dispatch::dispatch_download_symbols(workspace, id, &params).await
        }
        "debug" => debug_dispatch::dispatch_debug(workspace, id, &params).await,
        "snapshot" => build_dispatch::dispatch_snapshot(id, &params).await,
        "profiling" => build_dispatch::dispatch_profiling(id, &params).await,
        // XLIFF / Translation
        "xlf.generate" => build_dispatch::dispatch_xlf_generate(workspace, id, &params).await,
        "xlf.refresh" => build_dispatch::dispatch_xlf_refresh(workspace, id, &params).await,
        "xlf.untranslated" => build_dispatch::dispatch_xlf_untranslated(id, &params),
        "xlf.suggest" => build_dispatch::dispatch_xlf_suggest(workspace, id, &params).await,
        // WP15: Test runner
        "tests.discover" => build_dispatch::dispatch_tests_discover(workspace, id),
        "tests.run" => build_dispatch::dispatch_tests_run(workspace, id, &params).await,
        "tests.coverage" => build_dispatch::dispatch_tests_coverage(workspace, id),
        // p1-5: Phase 1 test_engine endpoints
        "tests.run_batch" => build_dispatch::dispatch_tests_run_batch(workspace, id, &params).await,
        "tests.run_auto" => build_dispatch::dispatch_tests_run_auto(workspace, id, &params).await,
        "tests.last_results" => {
            build_dispatch::dispatch_tests_last_results(workspace, id, &params).await
        }
        // p2: routing + affected-tests endpoints
        "tests.affected" => build_dispatch::dispatch_tests_affected(workspace, id, &params),
        "tests.classify" => build_dispatch::dispatch_tests_classify(workspace, id),
        // Phase 4: snapshot record/replay/diff
        "tests.snapshot_record" => {
            build_dispatch::dispatch_tests_snapshot_record(workspace, id, &params).await
        }
        "tests.snapshot_replay" => {
            build_dispatch::dispatch_tests_snapshot_replay(id, &params).await
        }
        "tests.snapshot_diff" => build_dispatch::dispatch_tests_snapshot_diff(id, &params).await,
        // Phase 5: mutation testing
        "tests.mutate" => build_dispatch::dispatch_tests_mutate(workspace, id, &params).await,
        // WP16: Object generation
        "generate" => build_dispatch::dispatch_generate(workspace, id, &params),
        // WP17: Analysis differentiators
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
        // Diagnostics / observability
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
            let cache_stats = workspace.semantic_cache.read().ok().map(|c| { // SILENT: avoid RwLock poison panic per CLAUDE.md
                let (hits, misses) = c.stats();
                serde_json::json!({ "types": c.len(), "hits": hits, "misses": misses, "version": c.version() })
            });
            let status = serde_json::json!({
                "pid": std::process::id(),
                "indexedSymbols": workspace.symbols.len(),
                "workspaceFiles": workspace.file_index.len(),
                "workspaceObjects": workspace.file_index.objects.len(),
                "builtinTypes": workspace.builtins.read().ok().map(|g| g.len()).unwrap_or(0), // SILENT: avoid RwLock poison panic per CLAUDE.md
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

// ---------------------------------------------------------------------------
// Shared utility functions used by dispatch sub-modules
// ---------------------------------------------------------------------------

pub(crate) fn extract_uri(params: &serde_json::Value) -> Option<url::Url> {
    let uri_str = params.get("uri")?.as_str()?;
    url::Url::parse(uri_str).ok() // SILENT: bad client input is not user-affecting
}

pub(crate) fn extract_position(params: &serde_json::Value) -> Option<crate::queries::Position> {
    // Cap to u32::MAX to prevent silent truncation of attacker-controlled values.
    let line = u32::try_from(params.get("line")?.as_u64()?).ok()?;
    let character = u32::try_from(params.get("character")?.as_u64()?).ok()?;
    Some(crate::queries::Position { line, character })
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

/// Get the project root from workspace, or return an error Response.
pub(crate) fn require_project_root(workspace: &Workspace, id: u64) -> Result<PathBuf, Response> {
    workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .ok_or_else(|| rpc_error(id, error_codes::INTERNAL_ERROR, "No project loaded"))
}

/// Parse an ObjectKind from a string, or return an invalid-params Response.
pub(crate) fn parse_object_kind(
    id: u64,
    kind_str: &str,
) -> Result<crate::symbols::ObjectKind, Response> {
    kind_str.parse::<crate::symbols::ObjectKind>().map_err(|_| {
        rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            &format!("Unknown object kind: {kind_str}"),
        )
    })
}

/// Get document text, loading from disk if needed. Returns the text or a file-not-found Response.
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
    if workspace.documents.get_text(uri).is_some() {
        return Some(());
    }
    // Try to read from disk (block_in_place avoids blocking the tokio runtime)
    let path = uri.to_file_path().ok()?; // SILENT: non-file URIs legitimately have no path
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
            std::env::current_dir().ok()?.join(path) // SILENT: current_dir failure handled by returning None
        };
        let canon = abs_path.canonicalize().unwrap_or(abs_path);
        return url::Url::from_file_path(canon).ok(); // SILENT: non-absolute paths can't become file URIs
    }
    None
}

pub(crate) fn lint_diag_to_json(d: &crate::syntax::LintDiagnostic) -> serde_json::Value {
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

// ---------------------------------------------------------------------------
// Workspace initialization (daemon mode — no LSP Client)
// ---------------------------------------------------------------------------

async fn initialize_daemon_workspace(workspace: &Workspace, project_root: &Path) {
    // Delegate common steps (find project, load packages, scan, toolchain) to al-core.
    let result = crate::workspace::initialize_core_workspace(workspace, project_root).await;

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
    use super::{extract_i32, extract_position, file_uri_from_params, read_bounded_line};

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
        // i32::MAX + 1 — would silently wrap to i32::MIN under `as i32`.
        let params = serde_json::json!({ "id": (i32::MAX as i64) + 1 });
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": (i32::MIN as i64) - 1 });
        assert_eq!(extract_i32(&params, "id"), None);
        // Very large u64 that exceeds i64::MAX is rejected at the as_i64 step.
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
        // Matches the existing hardening — make sure we don't regress it.
        let params = serde_json::json!({ "line": (u32::MAX as u64) + 1, "character": 0 });
        assert!(extract_position(&params).is_none());
    }

    /// Verify socket_path produces the same result for the same canonical path.
    #[test]
    fn socket_path_is_deterministic() {
        // Ensure XDG_RUNTIME_DIR is set so socket_path returns Some.
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

    /// A line within the limit is returned successfully.
    #[tokio::test]
    async fn bounded_read_accepts_line_within_limit() {
        let input = b"hello world\n";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, Some("hello world".to_string()));
    }

    /// A line without a newline that exceeds the limit returns an error.
    #[tokio::test]
    async fn bounded_read_rejects_line_exceeding_limit() {
        // 10 bytes of data, no newline, limit of 5 bytes
        let input = b"0123456789";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let err = read_bounded_line(&mut reader, 5).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("byte limit"));
    }

    /// EOF with no data returns None.
    #[tokio::test]
    async fn bounded_read_returns_none_on_empty_eof() {
        let input: &[u8] = b"";
        let mut reader = tokio::io::BufReader::new(input);
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, None);
    }

    /// A line where a newline IS found but the content exceeds the limit returns an error.
    #[tokio::test]
    async fn bounded_read_rejects_line_with_newline_exceeding_limit() {
        let input = b"0123456789\nmore data";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let err = read_bounded_line(&mut reader, 5).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("byte limit"));
    }

    /// EOF without a newline (partial line) returns the data as Some.
    #[tokio::test]
    async fn bounded_read_returns_partial_line_on_eof() {
        let input = b"no newline here";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, Some("no newline here".to_string()));
    }

    // --- file_uri_from_params -------------------------------------------------

    #[test]
    fn file_uri_prefers_explicit_uri_field() {
        // When a "uri" is present it is used verbatim, ignoring any "file".
        let params = serde_json::json!({
            "uri": "file:///some/where.al",
            "file": "/other/path.al",
        });
        let uri = file_uri_from_params(&params).expect("uri must parse");
        assert_eq!(uri.as_str(), "file:///some/where.al");
    }

    #[test]
    fn file_uri_canonicalizes_absolute_existing_path() {
        // An absolute path to an existing file is canonicalized and converted
        // to a file:// URL.
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
        // A relative path is joined onto the current working directory.
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
        // A nonexistent absolute path can't be canonicalized; the function
        // falls back to the (already absolute) path rather than failing.
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
}
