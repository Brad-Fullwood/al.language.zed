//! Daemon mode — JSON-RPC server over Unix socket.
//!
//! `al-lsp daemon --project /path/to/project` starts a daemon that:
//! - Listens on a deterministic Unix socket path
//! - Initializes a Workspace for the given project
//! - Accepts JSON-RPC requests, routes to al-core queries
//! - Auto-shuts down after 30 minutes of idle

mod lsp_dispatch;
mod debug_dispatch;
mod insight_dispatch;
mod build_dispatch;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use al_core::workspace::Workspace;
use al_core::jsonrpc::{error_codes, Request, Response, RpcError};
use al_daemon_client::socket_path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, Notify, Semaphore};

/// Global socket path for cleanup on exit.
static SOCKET_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Clean up the socket file (called from signal handlers or shutdown).
pub fn cleanup_socket() {
    if let Some(path) = SOCKET_PATH.get() {
        let _ = std::fs::remove_file(path);
        tracing::info!("daemon: socket cleaned up");
    }
}

/// RAII guard that cleans up the socket on drop.
struct SocketCleanup;

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        cleanup_socket();
    }
}

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024; // 64 MB
const MAX_CONNECTIONS: usize = 64;
const ACCEPT_BACKOFF_START: Duration = Duration::from_millis(10);
const ACCEPT_BACKOFF_CAP: Duration = Duration::from_secs(5);


/// Run the daemon server for a project.
pub async fn run_daemon(project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let sock_path = socket_path(&project_root);

    // Ensure parent directory exists
    if let Some(parent) = sock_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
        // If using the /tmp fallback (not XDG_RUNTIME_DIR), lock down dir permissions.
        if std::env::var("XDG_RUNTIME_DIR").is_err() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
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

    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let shutdown_signal = Arc::new(Notify::new());

    // Idle timeout checker
    let activity_clone = Arc::clone(&last_activity);
    let ws_clone = Arc::clone(&workspace);
    let shutdown_idle = Arc::clone(&shutdown_signal);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let elapsed = activity_clone.lock().await.elapsed();
            if elapsed >= IDLE_TIMEOUT {
                // Don't shut down if a debug session is active
                let has_debug_session = ws_clone
                    .debug_session
                    .try_lock()
                    .map(|g| g.is_some())
                    .unwrap_or(true);
                if has_debug_session {
                    tracing::info!("daemon: idle timeout skipped (debug session active)");
                    continue;
                }
                tracing::info!(idle_secs = elapsed.as_secs(), "daemon: idle timeout, shutting down");
                shutdown_idle.notify_one();
                return;
            }
        }
    });

    // Connection semaphore — limits concurrent active connections to avoid FD/memory exhaustion.
    let connection_limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    // Accept connections — break when shutdown is signalled so Drop guards run.
    let mut accept_backoff = ACCEPT_BACKOFF_START;
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        // Reset backoff on success.
                        accept_backoff = ACCEPT_BACKOFF_START;

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
                tracing::info!("daemon: shutdown signal received, exiting");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    workspace: Arc<Workspace>,
    last_activity: Arc<Mutex<Instant>>,
    shutdown: Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        if line.len() > MAX_MESSAGE_SIZE {
            tracing::warn!(len = line.len(), "daemon: message exceeds size limit, dropping connection");
            break;
        }
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        // Update idle timer
        *last_activity.lock().await = Instant::now();

        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatch_request(&workspace, req, &shutdown).await,
            Err(e) => Response {
                id: 0,
                result: None,
                error: Some(RpcError {
                    code: error_codes::PARSE_ERROR,
                    message: format!("Invalid JSON-RPC: {}", e),
                }),
            },
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
        "hover" => lsp_dispatch::dispatch_hover(workspace, id, &params),
        "definition" => lsp_dispatch::dispatch_definition(workspace, id, &params),
        "references" => lsp_dispatch::dispatch_references(workspace, id, &params),
        "implementations" => lsp_dispatch::dispatch_implementations(workspace, id, &params),
        "completions" => lsp_dispatch::dispatch_completions(workspace, id, &params),
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
        "fix.applicationArea" => build_dispatch::dispatch_fix_application_area(workspace, id, &params),
        "fix.tooltips" => build_dispatch::dispatch_fix_tooltips(workspace, id, &params),
        "fix.dataClassification" => build_dispatch::dispatch_fix_data_classification(workspace, id, &params),
        "rules" => build_dispatch::dispatch_rules(id),
        "parse" => build_dispatch::dispatch_parse(workspace, id, &params),
        "metrics" => build_dispatch::dispatch_metrics(workspace, id, &params),
        "sqlPatterns" => build_dispatch::dispatch_sql_patterns(workspace, id, &params),
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
        "clearCache" => build_dispatch::dispatch_clear_cache(id),
        "authenticate" => build_dispatch::dispatch_authenticate(workspace, id, &params).await,
        "downloadSymbols" => build_dispatch::dispatch_download_symbols(workspace, id, &params),
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
        "tests.coverage" => build_dispatch::dispatch_tests_coverage(workspace, id),
        // WP16: Object generation
        "generate" => build_dispatch::dispatch_generate(workspace, id, &params),
        // WP17: Analysis differentiators
        "obsolete" => build_dispatch::dispatch_obsolete(workspace, id),
        "audit.dataClassification" => build_dispatch::dispatch_audit_data_classification(workspace, id),
        "permissions.audit" => build_dispatch::dispatch_permission_set_audit(workspace, id),
        "deps.graph" => build_dispatch::dispatch_deps_graph(workspace, id, &params),
        "breaking" => build_dispatch::dispatch_breaking_changes(workspace, id, &params),
        "arch.lint" => build_dispatch::dispatch_arch_lint(workspace, id),
        "duplicates" => build_dispatch::dispatch_find_duplicates(workspace, id, &params),
        "upgrade" => build_dispatch::dispatch_upgrade_report(workspace, id, &params),
        "profiler.hints" => build_dispatch::dispatch_profiler_hints(workspace, id, &params),
        "ping" => Response { id, result: Some(serde_json::json!("pong")), error: None },
        "shutdown" => {
            tracing::info!("daemon: shutdown requested");
            shutdown.notify_one();
            Response { id, result: Some(serde_json::json!("ok")), error: None }
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
            Response { id, result: Some(status), error: None }
        }
        _ => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("Unknown method: {}", req.method),
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// Shared utility functions used by dispatch sub-modules
// ---------------------------------------------------------------------------

pub(crate) fn extract_uri(params: &serde_json::Value) -> Option<url::Url> {
    let uri_str = params.get("uri")?.as_str()?;
    url::Url::parse(uri_str).ok() // SILENT: bad client input is not user-affecting
}

pub(crate) fn extract_position(params: &serde_json::Value) -> Option<al_core::queries::Position> {
    let line = params.get("line")?.as_u64()? as u32;
    let character = params.get("character")?.as_u64()? as u32;
    Some(al_core::queries::Position { line, character })
}

pub(crate) fn invalid_params(id: u64) -> Response {
    Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INVALID_PARAMS,
            message: "Missing or invalid parameters".to_string(),
        }),
    }
}

pub(crate) fn rpc_error(id: u64, code: i32, message: &str) -> Response {
    Response {
        id,
        result: None,
        error: Some(al_core::jsonrpc::RpcError {
            code,
            message: message.to_string(),
        }),
    }
}

/// Ensure a file is loaded in the document store. If not found, read from disk.
pub(crate) fn ensure_document(workspace: &Workspace, uri: &url::Url) -> Option<()> {
    if workspace.documents.get_text(uri).is_some() {
        return Some(());
    }
    // Try to read from disk
    let path = uri.to_file_path().ok()?; // SILENT: non-file URIs legitimately have no path
    let content = std::fs::read_to_string(&path).ok()?; // SILENT: file read failure handled by returning None
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

pub(crate) fn lint_diag_to_json(d: &al_core::syntax::LintDiagnostic) -> serde_json::Value {
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

pub(crate) fn generate_fix(
    diag: &al_core::syntax::LintDiagnostic,
    lines: &[&str],
) -> Option<serde_json::Value> {
    let start_row = diag.range.start_point.row;
    let end_row = diag.range.end_point.row;
    let start_col = diag.range.start_point.column;
    let end_col = diag.range.end_point.column;

    match diag.code.as_str() {
        // AL-L001: Empty begin..end block — delete the entire line range.
        "AL-L001" => {
            Some(serde_json::json!({
                "code": diag.code,
                "message": diag.message,
                "range": {
                    "start": { "line": start_row, "character": 0 },
                    "end": { "line": end_row + 1, "character": 0 }
                },
                "newText": "",
            }))
        }
        // AL-L005: Unused variable declaration — delete the declaration line.
        "AL-L005" => {
            Some(serde_json::json!({
                "code": diag.code,
                "message": diag.message,
                "range": {
                    "start": { "line": start_row, "character": 0 },
                    "end": { "line": end_row + 1, "character": 0 }
                },
                "newText": "",
            }))
        }
        // AL-L006: Empty trigger — delete the entire trigger declaration.
        "AL-L006" => {
            Some(serde_json::json!({
                "code": diag.code,
                "message": diag.message,
                "range": {
                    "start": { "line": start_row, "character": 0 },
                    "end": { "line": end_row + 1, "character": 0 }
                },
                "newText": "",
            }))
        }
        // AL-L007: TODO comment — not auto-fixable, report only.
        "AL-L007" => None,
        // AL-L016: Procedure name not PascalCase — capitalise the first letter.
        "AL-L016" => {
            if let Some(line) = lines.get(start_row) {
                let chars: Vec<char> = line.chars().collect();
                if start_col < chars.len() {
                    let name_end_col = if start_row == end_row { end_col } else { chars.len() };
                    let name: String = chars[start_col..name_end_col.min(chars.len())].iter().collect();
                    if let Some(first) = name.chars().next() {
                        let fixed_name = format!("{}{}", first.to_uppercase(), &name[first.len_utf8()..]);
                        return Some(serde_json::json!({
                            "code": diag.code,
                            "message": diag.message,
                            "range": {
                                "start": { "line": start_row, "character": start_col },
                                "end": { "line": end_row, "character": name_end_col }
                            },
                            "newText": fixed_name,
                        }));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Workspace initialization (daemon mode — no LSP Client)
// ---------------------------------------------------------------------------

async fn initialize_daemon_workspace(workspace: &Workspace, project_root: &Path) {
    // Discover project
    match al_core::project::find_project(project_root) {
        Ok(project) => {
            tracing::info!(
                name = %project.app_json.name,
                root = %project.root.display(),
                packages = project.packages.len(),
                "daemon: project discovered"
            );

            // Load symbol packages (with disk cache for fast warm starts)
            let cache = al_core::symbols::cache::SymbolCache::default_location();
            let loaded = workspace.symbols.load_packages_cached(&project.packages, &cache);
            let total_symbols: usize = loaded.iter().map(|p| p.objects.len()).sum();
            tracing::info!(packages = loaded.len(), symbols = total_symbols, "daemon: loaded symbol packages");

            // Invalidate insight graph — packages changed
            workspace.invalidate_insight_graph();

            // Store package metadata for the `packages` query
            let pkg_info: Vec<al_core::workspace::PackageInfo> = loaded
                .iter()
                .map(|p| al_core::workspace::PackageInfo {
                    name: p.name.clone(),
                    publisher: p.publisher.clone(),
                    version: p.version.clone(),
                    object_count: p.objects.len(),
                })
                .collect();
            *workspace.package_info.write().unwrap_or_else(|e| e.into_inner()) = pkg_info; // SILENT: recover from RwLock poison

            // Scan workspace .al files
            let file_count = workspace.file_index.scan(&project.root);

            // Also open scanned files in DocumentStore for query access
            for entry in workspace.file_index.files.iter() {
                if let Ok(uri) = url::Url::from_file_path(entry.key()) {
                    workspace.documents.open(uri, entry.value().clone());
                }
            }

            // Store project info
            *workspace.project.write().await = Some(project);

            tracing::info!(
                symbols = workspace.symbols.len(),
                workspace_files = file_count,
                workspace_objects = workspace.file_index.objects.len(),
                "daemon: workspace initialized"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, root = %project_root.display(), "daemon: project discovery failed");
        }
    }

    // Discover toolchain
    match al_core::toolchain::find_toolchain() {
        Ok(tc) => {
            tracing::info!(version = %tc.version, "daemon: toolchain found");
            *workspace.toolchain.write().await = Some(tc);
        }
        Err(e) => {
            tracing::info!(error = %e, "daemon: no toolchain (syntax-only mode)");
        }
    }
}

#[cfg(test)]
mod tests {
    /// Verify socket_path produces the same result for the same canonical path.
    #[test]
    fn socket_path_is_deterministic() {
        let p = std::path::Path::new("/tmp");
        let path1 = al_daemon_client::socket_path(p);
        let path2 = al_daemon_client::socket_path(p);
        assert_eq!(path1, path2);
        assert!(path1.to_str().unwrap().ends_with(".sock"));
        let filename = path1.file_name().unwrap().to_str().unwrap();
        let hash_part = filename.strip_suffix(".sock").unwrap();
        assert_eq!(hash_part.len(), 16);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
