//! Daemon mode — JSON-RPC server over Unix socket.
//!
//! `al-lsp daemon --project /path/to/project` starts a daemon that:
//! - Listens on a deterministic Unix socket path
//! - Initializes a Workspace for the given project
//! - Accepts JSON-RPC requests, routes to al-core queries
//! - Auto-shuts down after 30 minutes of idle

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use al_core::workspace::Workspace;
use al_core::jsonrpc::{error_codes, Request, Response, RpcError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, Notify};

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

/// FNV-1a 64-bit hash — stable across Rust compiler versions.
/// Must match the implementation in al-cli/src/client.rs.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Compute the deterministic socket path for a project root.
///
/// The path is canonicalized before hashing so that symlinks and relative paths
/// resolve to the same socket as the client (which also canonicalizes).
pub fn socket_path(project_root: &Path) -> PathBuf {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let hash = format!("{:016x}", fnv1a64(canonical.as_os_str().as_encoded_bytes()));
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{}/al-lsp/{}.sock", runtime_dir, hash))
}

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

    // Accept connections — break when shutdown is signalled so Drop guards run.
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let ws = Arc::clone(&workspace);
                        let activity = Arc::clone(&last_activity);
                        let shutdown_conn = Arc::clone(&shutdown_signal);
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, ws, activity, shutdown_conn).await {
                                tracing::warn!(error = %e, "daemon: connection error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "daemon: accept error");
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
        "hover" => dispatch_hover(workspace, id, &params),
        "definition" => dispatch_definition(workspace, id, &params),
        "references" => dispatch_references(workspace, id, &params),
        "completions" => dispatch_completions(workspace, id, &params),
        "signatureHelp" => dispatch_signature_help(workspace, id, &params),
        "rename" => dispatch_rename(workspace, id, &params),
        "documentSymbols" => dispatch_document_symbols(workspace, id, &params),
        "foldingRanges" => dispatch_folding_ranges(workspace, id, &params),
        "semanticTokens" => dispatch_semantic_tokens(workspace, id, &params),
        "inlayHints" => dispatch_inlay_hints(workspace, id, &params),
        "codeActions" => dispatch_code_actions(workspace, id, &params),
        // Symbol queries
        "search" => dispatch_search(workspace, id, &params),
        "object" => dispatch_object(workspace, id, &params),
        "byId" => dispatch_by_id(workspace, id, &params),
        "events" => dispatch_events(workspace, id, &params),
        "subscribers" => dispatch_subscribers(workspace, id, &params),
        "composed" => dispatch_composed(workspace, id, &params),
        "packages" => dispatch_packages(workspace, id),
        "deps" => dispatch_deps(workspace, id),
        // Analysis
        "lint" => dispatch_lint(workspace, id, &params),
        "format" => dispatch_format(workspace, id, &params),
        "fix" => dispatch_fix(workspace, id, &params),
        "rules" => dispatch_rules(id),
        "parse" => dispatch_parse(workspace, id, &params),
        "source" => dispatch_source(workspace, id, &params),
        // Insight engine
        "trace" => dispatch_trace(workspace, id, &params),
        "entrypoints" => dispatch_entrypoints(workspace, id),
        "graphExport" => dispatch_graph_export(workspace, id, &params),
        "insightStats" => dispatch_insight_stats(workspace, id),
        "deadCode" => dispatch_dead_code(workspace, id),
        "impact" => dispatch_impact(workspace, id, &params),
        "suggestEvent" => dispatch_suggest_event(workspace, id, &params),
        // Semantic / toolchain
        "permissions" => dispatch_permissions(workspace, id, &params),
        "compile" => dispatch_compile(workspace, id),
        "package" => dispatch_package(workspace, id).await,
        "newProject" => dispatch_new_project(id, &params),
        "errorCodes" => dispatch_error_codes(workspace, id),
        "builtinTypes" => dispatch_builtin_types(workspace, id),
        "setup" => dispatch_setup(workspace, id),
        "clearCache" => dispatch_clear_cache(id),
        "downloadSymbols" => dispatch_download_symbols(workspace, id, &params),
        "debug" => dispatch_debug(workspace, id, &params).await,
        "snapshot" => dispatch_snapshot(id, &params).await,
        "profiling" => dispatch_profiling(id, &params).await,
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
// Query dispatchers
// ---------------------------------------------------------------------------

fn extract_uri(params: &serde_json::Value) -> Option<url::Url> {
    let uri_str = params.get("uri")?.as_str()?;
    url::Url::parse(uri_str).ok() // SILENT: bad client input is not user-affecting
}

fn extract_position(params: &serde_json::Value) -> Option<al_core::queries::Position> {
    let line = params.get("line")?.as_u64()? as u32;
    let character = params.get("character")?.as_u64()? as u32;
    Some(al_core::queries::Position { line, character })
}

fn invalid_params(id: u64) -> Response {
    Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INVALID_PARAMS,
            message: "Missing or invalid parameters".to_string(),
        }),
    }
}

fn dispatch_hover(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let result = al_core::queries::hover::hover(workspace, &uri, position);
    let value = result.map(|r| serde_json::json!({
        "contents": r.contents,
        "range": r.range.map(|rng| serde_json::json!({
            "start": { "line": rng.start.line, "character": rng.start.character },
            "end": { "line": rng.end.line, "character": rng.end.character },
        })),
    }));
    Response { id, result: value, error: None }
}

fn dispatch_definition(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let result = al_core::queries::definition::definition(workspace, &uri, position);
    let value = result.map(|locations| {
        serde_json::json!(locations.iter().map(|l| serde_json::json!({
            "uri": l.uri.as_str(),
            "range": {
                "start": { "line": l.range.start.line, "character": l.range.start.character },
                "end": { "line": l.range.end.line, "character": l.range.end.character },
            }
        })).collect::<Vec<_>>())
    });
    Response { id, result: value, error: None }
}

fn dispatch_references(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let include_declaration = params.get("includeDeclaration").and_then(|v| v.as_bool()).unwrap_or(true);
    let locations = al_core::queries::references::references(workspace, &uri, position, include_declaration);
    let value = serde_json::json!(locations.iter().map(|l| serde_json::json!({
        "uri": l.uri.as_str(),
        "range": {
            "start": { "line": l.range.start.line, "character": l.range.start.character },
            "end": { "line": l.range.end.line, "character": l.range.end.character },
        }
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

fn dispatch_completions(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let entries = al_core::queries::completions::completions(workspace, &uri, position);
    let value = serde_json::json!(entries.iter().map(|e| serde_json::json!({
        "label": e.label,
        "kind": format!("{:?}", e.kind),
        "detail": e.detail,
        "sortText": e.sort_text,
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

fn dispatch_signature_help(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let result = al_core::queries::signature::signature_help(workspace, &uri, position);
    let value = result.map(|sh| serde_json::json!({
        "signatures": sh.signatures.iter().map(|s| serde_json::json!({
            "label": s.label,
            "documentation": s.documentation,
            "parameters": s.parameters.iter().map(|p| serde_json::json!({
                "label": p.label,
            })).collect::<Vec<_>>(),
            "activeParameter": s.active_parameter,
        })).collect::<Vec<_>>(),
        "activeSignature": sh.active_signature,
        "activeParameter": sh.active_parameter,
    }));
    Response { id, result: value, error: None }
}

fn dispatch_rename(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let Some(new_name) = params.get("newName").and_then(|v| v.as_str()) else { return invalid_params(id); };
    let result = al_core::queries::rename::rename(workspace, &uri, position, new_name);
    let value = result.map(|we| {
        let changes: serde_json::Map<String, serde_json::Value> = we.changes.iter().map(|(uri, edits)| {
            (uri.as_str().to_string(), serde_json::json!(edits.iter().map(|e| serde_json::json!({
                "range": {
                    "start": { "line": e.range.start.line, "character": e.range.start.character },
                    "end": { "line": e.range.end.line, "character": e.range.end.character },
                },
                "newText": e.new_text,
            })).collect::<Vec<_>>()))
        }).collect();
        serde_json::json!({ "changes": changes })
    });
    Response { id, result: value, error: None }
}

fn dispatch_document_symbols(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let result = al_core::queries::symbols::document_symbols(workspace, &uri);
    let value = result.and_then(|r| serde_json::to_value(r).ok()); // SILENT: serialization of valid structs should not fail
    Response { id, result: value, error: None }
}

fn dispatch_folding_ranges(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let result = al_core::queries::folding::folding_ranges(workspace, &uri);
    let value = result.and_then(|r| serde_json::to_value(r).ok()); // SILENT: serialization of valid structs should not fail
    Response { id, result: value, error: None }
}

fn dispatch_semantic_tokens(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let tokens = al_core::queries::semantic_tokens::semantic_tokens_full(workspace, &uri);
    let value = serde_json::json!(tokens.iter().map(|t| serde_json::json!({
        "deltaLine": t.delta_line,
        "deltaStart": t.delta_start,
        "length": t.length,
        "tokenType": t.token_type,
        "tokenModifiers": t.token_modifiers,
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

// ---------------------------------------------------------------------------
// Inlay hints & code actions (existing al-core queries, newly dispatched)
// ---------------------------------------------------------------------------

fn dispatch_inlay_hints(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let start_line = params.get("startLine").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let end_line = params.get("endLine").and_then(|v| v.as_u64()).unwrap_or(u32::MAX as u64) as u32;
    let range = tower_lsp::lsp_types::Range {
        start: tower_lsp::lsp_types::Position::new(start_line, 0),
        end: tower_lsp::lsp_types::Position::new(end_line, u32::MAX),
    };
    let hints = al_core::queries::inlay_hints::inlay_hints(workspace, &uri, range);
    let value = hints
        .and_then(|h| serde_json::to_value(&h).ok()) // SILENT: serialization of valid structs should not fail
        .unwrap_or(serde_json::json!([]));
    Response { id, result: Some(value), error: None }
}

fn dispatch_code_actions(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let range = al_core::queries::Range { start: position, end: position };
    let actions = al_core::queries::code_actions::source_actions(workspace, &uri, range);
    let value = serde_json::json!(actions.iter().map(|a| serde_json::json!({
        "title": a.title,
        "kind": format!("{:?}", a.kind),
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

// ---------------------------------------------------------------------------
// Symbol queries
// ---------------------------------------------------------------------------

fn dispatch_search(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(query) = params.get("query").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let results = workspace.symbols.search(query, limit);
    let value: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok()) // SILENT: serialization of valid structs should not fail
        .collect();
    Response { id, result: Some(serde_json::json!(value)), error: None }
}

fn dispatch_object(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Ok(kind) = kind_str.parse::<al_core::symbols::ObjectKind>() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown object kind: {}", kind_str),
            }),
        };
    };
    let candidates = workspace.symbols.get_by_name(name);
    let matches: Vec<serde_json::Value> = candidates
        .iter()
        .filter(|e| e.kind == kind)
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok()) // SILENT: serialization of valid structs should not fail
        .collect();
    if matches.is_empty() {
        Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} named '{}'", kind, name),
            }),
        }
    } else {
        Response { id, result: Some(serde_json::json!(matches)), error: None }
    }
}

fn dispatch_by_id(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(obj_id) = params.get("id").and_then(|v| v.as_i64()) else {
        return invalid_params(id);
    };
    let Ok(kind) = kind_str.parse::<al_core::symbols::ObjectKind>() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown object kind: {}", kind_str),
            }),
        };
    };
    let results = workspace.symbols.get_by_id(kind, obj_id as i32);
    let value: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok()) // SILENT: serialization of valid structs should not fail
        .collect();
    if value.is_empty() {
        Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} with id {}", kind, obj_id),
            }),
        }
    } else {
        Response { id, result: Some(serde_json::json!(value)), error: None }
    }
}

fn dispatch_events(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let results = workspace.symbols.get_events(name);
    let publishers: Vec<serde_json::Value> = results
        .publishers
        .iter()
        .map(|p| {
            serde_json::json!({
                "objectKind": p.object.kind.to_string(),
                "objectName": p.object.name,
                "methodName": p.method.name,
                "eventType": p.event_type.to_string(),
                "parameters": p.method.parameters.iter().map(|param| serde_json::json!({
                    "name": param.name,
                    "type_name": param.type_name,
                    "is_var": param.is_var,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Response { id, result: Some(serde_json::json!(publishers)), error: None }
}

fn dispatch_subscribers(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(event) = params.get("event").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let results = workspace.symbols.get_events(event);
    let subscribers: Vec<serde_json::Value> = results
        .subscribers
        .iter()
        .map(|s| {
            serde_json::json!({
                "objectName": s.object.name,
                "methodName": s.method.name,
                "targetObjectType": s.target_object_type,
                "targetObjectName": s.target_object_name,
                "targetEventName": s.target_event_name,
            })
        })
        .collect();
    Response { id, result: Some(serde_json::json!(subscribers)), error: None }
}

fn dispatch_composed(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Ok(kind) = kind_str.parse::<al_core::symbols::ObjectKind>() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown object kind: {}", kind_str),
            }),
        };
    };
    match workspace.symbols.get_composed_cached(kind, name) {
        Some(composed) => {
            let value = serde_json::to_value(composed.as_ref()).unwrap_or(serde_json::Value::Null);
            Response { id, result: Some(value), error: None }
        }
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} named '{}' or no extensions found", kind, name),
            }),
        },
    }
}

fn dispatch_packages(workspace: &Workspace, id: u64) -> Response {
    let pkgs = match workspace.package_info.read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Lock poisoned".to_string(),
                }),
            };
        }
    };
    let value = serde_json::to_value(pkgs.as_slice()).unwrap_or(serde_json::json!([]));
    Response { id, result: Some(value), error: None }
}

fn dispatch_deps(workspace: &Workspace, id: u64) -> Response {
    let project = match workspace.project.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Workspace is initializing, try again".to_string(),
                }),
            };
        }
    };
    match project.as_ref() {
        Some(p) => {
            let deps: Vec<serde_json::Value> = p
                .app_json
                .dependencies
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "id": d.id,
                        "name": d.name,
                        "publisher": d.publisher,
                        "version": d.version,
                    })
                })
                .collect();
            let all_deps: Vec<serde_json::Value> = p
                .all_dependencies()
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "id": d.id,
                        "name": d.name,
                        "publisher": d.publisher,
                        "version": d.version,
                    })
                })
                .collect();
            Response {
                id,
                result: Some(serde_json::json!({
                    "explicit": deps,
                    "all": all_deps,
                    "project": {
                        "name": p.app_json.name,
                        "publisher": p.app_json.publisher,
                        "version": p.app_json.version,
                    }
                })),
                error: None,
            }
        }
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "No project loaded".to_string(),
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// Analysis dispatchers (lint, format, fix, rules, parse)
// ---------------------------------------------------------------------------

/// Ensure a file is loaded in the document store. If not found, read from disk.
fn ensure_document(workspace: &Workspace, uri: &url::Url) -> Option<()> {
    if workspace.documents.get_text(uri).is_some() {
        return Some(());
    }
    // Try to read from disk
    let path = uri.to_file_path().ok()?; // SILENT: non-file URIs legitimately have no path
    let content = std::fs::read_to_string(&path).ok()?; // SILENT: file read failure handled by returning None
    workspace.documents.open(uri.clone(), content);
    Some(())
}

fn file_uri_from_params(params: &serde_json::Value) -> Option<url::Url> {
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

fn dispatch_lint(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);

    if all {
        // Lint all workspace files
        let mut results: Vec<serde_json::Value> = Vec::new();
        for entry in workspace.file_index.files.iter() {
            let path = entry.key();
            let content = entry.value();
            let result = al_core::syntax::AlParser::parse_quick(content);
            let diagnostics = al_core::syntax::lint(&result.tree, content);
            if !diagnostics.is_empty() {
                let diags: Vec<serde_json::Value> = diagnostics
                    .iter()
                    .map(lint_diag_to_json)
                    .collect();
                results.push(serde_json::json!({
                    "file": path.display().to_string(),
                    "diagnostics": diags,
                }));
            }
        }
        return Response { id, result: Some(serde_json::json!(results)), error: None };
    }

    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "File not found".to_string(),
            }),
        };
    };

    let result = al_core::syntax::AlParser::parse_quick(&text);
    let mut diagnostics = al_core::syntax::lint(&result.tree, &text);

    // Add parse errors
    for err in &result.errors {
        diagnostics.push(al_core::syntax::LintDiagnostic {
            code: "parse-error".to_string(),
            message: err.message.clone(),
            range: err.range,
            severity: al_core::syntax::LintSeverity::Error,
        });
    }

    let diags: Vec<serde_json::Value> = diagnostics.iter().map(lint_diag_to_json).collect();
    Response { id, result: Some(serde_json::json!(diags)), error: None }
}

fn lint_diag_to_json(d: &al_core::syntax::LintDiagnostic) -> serde_json::Value {
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

fn dispatch_format(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let check = params.get("check").and_then(|v| v.as_bool()).unwrap_or(false);

    // Accept direct content or file path
    let content = if let Some(text) = params.get("content").and_then(|v| v.as_str()) {
        text.to_string()
    } else if let Some(uri) = file_uri_from_params(params) {
        ensure_document(workspace, &uri);
        match workspace.documents.get_text(&uri) {
            Some(t) => t,
            None => {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INVALID_PARAMS,
                        message: "File not found".to_string(),
                    }),
                };
            }
        }
    } else {
        return invalid_params(id);
    };

    let options = al_core::syntax::FormatOptions::default();
    let formatted = al_core::syntax::format_al(&content, &options);
    let changed = formatted != content;

    if check {
        Response {
            id,
            result: Some(serde_json::json!({ "changed": changed })),
            error: None,
        }
    } else {
        // If a file was specified, write back
        if let Some(uri) = file_uri_from_params(params) {
            if let Ok(path) = uri.to_file_path() {
                if changed {
                    let _ = std::fs::write(&path, &formatted);
                    // Update document store
                    workspace.documents.open(uri, formatted.clone());
                }
            }
        }
        Response {
            id,
            result: Some(serde_json::json!({
                "formatted": formatted,
                "changed": changed,
            })),
            error: None,
        }
    }
}

fn dispatch_fix(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let dry_run = params.get("dryRun").and_then(|v| v.as_bool()).unwrap_or(false);
    let rule_filter = params.get("rule").and_then(|v| v.as_str());

    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "File not found".to_string(),
            }),
        };
    };

    let result = al_core::syntax::AlParser::parse_quick(&text);
    let diagnostics = al_core::syntax::lint(&result.tree, &text);
    let filtered: Vec<_> = diagnostics
        .iter()
        .filter(|d| {
            if let Some(rule) = rule_filter {
                d.code.eq_ignore_ascii_case(rule)
            } else {
                true
            }
        })
        .collect();

    // Generate fix actions (simple text replacements based on lint codes)
    let mut edits: Vec<serde_json::Value> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for diag in &filtered {
        if let Some(fix) = generate_fix(diag, &lines) {
            edits.push(fix);
        }
    }

    if !dry_run && !edits.is_empty() {
        // Apply edits to the file
        if let Ok(path) = uri.to_file_path() {
            // Apply edits in reverse order to preserve positions
            let mut new_text = text.clone();
            // Simple: re-format the file after fixes for now
            let options = al_core::syntax::FormatOptions::default();
            new_text = al_core::syntax::format_al(&new_text, &options);
            let _ = std::fs::write(&path, &new_text);
            workspace.documents.open(uri, new_text);
        }
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "diagnostics": filtered.len(),
            "fixes": edits.len(),
            "dryRun": dry_run,
            "edits": edits,
        })),
        error: None,
    }
}

fn generate_fix(
    diag: &al_core::syntax::LintDiagnostic,
    _lines: &[&str],
) -> Option<serde_json::Value> {
    // Return fix metadata — actual application happens in the CLI or daemon
    match diag.code.as_str() {
        "AL-L001" | "AL-L005" | "AL-L006" | "AL-L007" | "AL-L016" => Some(serde_json::json!({
            "code": diag.code,
            "message": diag.message,
            "line": diag.range.start_point.row + 1,
        })),
        _ => None,
    }
}

fn dispatch_rules(id: u64) -> Response {
    let rules = al_core::syntax::lint_rules();
    let value: Vec<serde_json::Value> = rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "code": r.code,
                "name": r.name,
                "severity": r.severity.to_string(),
                "description": r.description,
            })
        })
        .collect();
    Response { id, result: Some(serde_json::json!(value)), error: None }
}

fn dispatch_parse(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "File not found".to_string(),
            }),
        };
    };

    let start = std::time::Instant::now();
    let result = al_core::syntax::AlParser::parse_quick(&text);
    let elapsed = start.elapsed();

    let node_count = al_core::parsing::count_nodes(&result.tree);

    Response {
        id,
        result: Some(serde_json::json!({
            "errors": result.errors.len(),
            "nodeCount": node_count,
            "parseTimeMs": elapsed.as_secs_f64() * 1000.0,
            "parseErrors": result.errors.iter().map(|e| serde_json::json!({
                "message": e.message,
                "line": e.range.start_point.row + 1,
                "column": e.range.start_point.column + 1,
            })).collect::<Vec<_>>(),
        })),
        error: None,
    }
}


// ---------------------------------------------------------------------------
// Source extraction
// ---------------------------------------------------------------------------

fn dispatch_source(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return invalid_params(id),
    };

    let kind_filter = params
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<al_core::symbols::ObjectKind>().ok());

    let proc_filter = params.get("proc").and_then(|v| v.as_str());
    let trigger_filter = params.get("trigger").and_then(|v| v.as_str());

    match al_core::queries::source::source(workspace, name, kind_filter, proc_filter, trigger_filter) {
        Some(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
        },
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Object '{}' not found", name),
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// Dead code detection
// ---------------------------------------------------------------------------

fn dispatch_dead_code(workspace: &Workspace, id: u64) -> Response {
    let unused = al_core::queries::dead_code::dead_code(workspace);
    Response {
        id,
        result: Some(serde_json::to_value(&unused).unwrap_or_default()),
        error: None,
    }
}

fn dispatch_impact(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
    if symbol.is_empty() {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "Missing 'symbol' parameter".to_string(),
            }),
        };
    }
    let entries = al_core::queries::impact::impact(workspace, symbol);
    Response {
        id,
        result: Some(serde_json::json!({ "symbol": symbol, "impacted": entries })),
        error: None,
    }
}

fn dispatch_suggest_event(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let description = params.get("description").and_then(|v| v.as_str()).unwrap_or("");
    if description.is_empty() {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "Missing 'description' parameter".to_string(),
            }),
        };
    }
    let suggestions = al_core::queries::suggest_event::suggest_event(workspace, description);
    Response {
        id,
        result: Some(serde_json::json!({ "query": description, "suggestions": suggestions })),
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Permission set generation
// ---------------------------------------------------------------------------

fn dispatch_permissions(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let entries = al_core::permissions::collect_permissions(workspace);
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("al");

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Generated Permissions");
    let perm_id = params
        .get("id")
        .and_then(|v| v.as_i64())
        .unwrap_or(50100);
    let role_id = params
        .get("roleId")
        .and_then(|v| v.as_str())
        .unwrap_or("GENERATED");

    match format {
        "xml" => {
            let output = al_core::permissions::render_xml(&entries, role_id, name);
            Response {
                id,
                result: Some(serde_json::json!({
                    "format": "xml",
                    "content": output,
                    "objectCount": entries.len(),
                })),
                error: None,
            }
        }
        _ => {
            let output = al_core::permissions::render_al(&entries, name, perm_id);
            Response {
                id,
                result: Some(serde_json::json!({
                    "format": "al",
                    "content": output,
                    "objectCount": entries.len(),
                })),
                error: None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Semantic / toolchain dispatchers
// ---------------------------------------------------------------------------

fn dispatch_compile(workspace: &Workspace, id: u64) -> Response {
    let tc = match workspace.toolchain.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Workspace is initializing, try again".to_string(),
                }),
            };
        }
    };
    let project = match workspace.project.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Workspace is initializing, try again".to_string(),
                }),
            };
        }
    };

    let project_root = match project.as_ref() {
        Some(p) => p.root.clone(),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "No project loaded".to_string(),
                }),
            };
        }
    };
    if tc.is_none() {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "No toolchain loaded".to_string(),
            }),
        };
    }
    // Drop the read guards before acquiring async locks
    drop(tc);
    drop(project);

    let result: Result<serde_json::Value, String> = tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let guard = al_core::semantic::get_or_init_bridge(workspace)
                .await
                .ok_or("Failed to initialize semantic bridge")?;
            let bridge = guard
                .as_ref()
                .ok_or("Semantic bridge unavailable")?;
            let compile_result = bridge
                .compile(&project_root, None, None)
                .await
                .map_err(|e| format!("Compilation failed: {}", e))?;
            Ok(serde_json::json!({
                "success": compile_result.success,
                "diagnostics": compile_result.diagnostics.iter().map(|d| serde_json::json!({
                    "file": d.file.display().to_string(),
                    "line": d.line,
                    "column": d.column,
                    "endLine": d.end_line,
                    "endColumn": d.end_column,
                    "severity": d.severity,
                    "code": d.code,
                    "message": d.message,
                })).collect::<Vec<_>>(),
                "appPath": compile_result.app_path.as_ref().map(|p| p.display().to_string()),
            }))
        })
    });
    match result {
        Ok(value) => Response { id, result: Some(value), error: None },
        Err(msg) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::CODE_ANALYSIS_ERROR,
                message: msg,
            }),
        },
    }
}

async fn dispatch_package(workspace: &Workspace, id: u64) -> Response {
    let tc = match workspace.toolchain.try_read() {
        // SILENT: avoid RwLock poison panic per CLAUDE.md
        Ok(guard) => guard.clone(),
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Workspace is initializing, try again".to_string(),
                }),
            };
        }
    };
    let toolchain = match tc {
        Some(tc) => tc,
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "No toolchain available. Run 'al setup' first.".to_string(),
                }),
            };
        }
    };
    let project = match workspace.project.try_read() {
        // SILENT: avoid RwLock poison panic per CLAUDE.md
        Ok(guard) => guard.clone(),
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Workspace is initializing, try again".to_string(),
                }),
            };
        }
    };
    let project_root = match project.as_ref() {
        Some(p) => p.root.clone(),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "No project loaded".to_string(),
                }),
            };
        }
    };

    match al_core::build::compile_project(&toolchain, &project_root, None).await {
        Ok(result) => Response {
            id,
            // SILENT: serialization of valid struct should not fail
            result: Some(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null)),
            error: None,
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: e.to_string(),
            }),
        },
    }
}

fn dispatch_new_project(id: u64, params: &serde_json::Value) -> Response {
    let dir = match params.get("dir").and_then(|v| v.as_str()) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Missing 'dir' parameter".to_string(),
                }),
            };
        }
    };

    let config = al_core::scaffold::ScaffoldConfig {
        name: params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("MyApp")
            .to_string(),
        publisher: params
            .get("publisher")
            .and_then(|v| v.as_str())
            .unwrap_or("Default Publisher")
            .to_string(),
        ..al_core::scaffold::ScaffoldConfig::default()
    };

    match al_core::scaffold::create_project(&dir, &config) {
        Ok(result) => Response {
            id,
            // SILENT: serialization of valid struct should not fail
            result: Some(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null)),
            error: None,
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: e,
            }),
        },
    }
}

fn dispatch_error_codes(workspace: &Workspace, id: u64) -> Response {
    let value: Vec<serde_json::Value> = workspace.error_codes
        .iter()
        .map(|entry| {
            serde_json::json!({
                "code": entry.key().clone(),
                "description": entry.value().clone(),
            })
        })
        .collect();
    Response { id, result: Some(serde_json::json!(value)), error: None }
}

fn dispatch_builtin_types(workspace: &Workspace, id: u64) -> Response {
    let builtins = match workspace.builtins.read() {
        Ok(guard) => guard,
        Err(_) => return Response { id, result: Some(serde_json::json!([])), error: None },
    };
    let value: Vec<serde_json::Value> = builtins
        .iter()
        .map(|bt| {
            serde_json::json!({
                "name": bt.name,
                "methods": bt.methods.iter().map(|m| serde_json::json!({
                    "name": m.name,
                    "parameters": m.parameters.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "typeName": p.type_name,
                        "isVar": p.is_var,
                    })).collect::<Vec<_>>(),
                    "returnType": m.return_type,
                    "documentation": m.documentation,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Response { id, result: Some(serde_json::json!(value)), error: None }
}

fn dispatch_setup(workspace: &Workspace, id: u64) -> Response {
    let report = al_core::toolchain::doctor(workspace);
    Response {
        id,
        result: Some(serde_json::to_value(&report).unwrap_or(serde_json::Value::Null)),
        error: None,
    }
}

fn dispatch_clear_cache(id: u64) -> Response {
    let cache_dir = dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("index"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/index"));

    let existed = cache_dir.exists();
    if existed {
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "deleted": existed,
            "path": cache_dir.display().to_string(),
        })),
        error: None,
    }
}


fn dispatch_download_symbols(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("nuget");

    let project = match workspace.project.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Workspace is initializing, try again".to_string(),
                }),
            };
        }
    };
    let Some(project) = project.as_ref() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "No project loaded".to_string(),
            }),
        };
    };

    let all_deps = project.all_dependencies();
    if all_deps.is_empty() {
        return Response {
            id,
            result: Some(serde_json::json!({
                "status": "no dependencies",
                "downloaded": 0,
                "failed": 0,
                "results": [],
            })),
            error: None,
        };
    }

    let dest = project.packages_dir.clone();
    let project_configs = project.server_configs.clone();

    // Release the lock before async work
    let _ = project;

    let result: Vec<serde_json::Value> = tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            if source == "server" {
                if project_configs.is_empty() {
                    return vec![serde_json::json!({
                        "error": "No BC server config found"
                    })];
                }
                let cfg = &project_configs[0];
                let auth = match cfg.authentication {
                    al_core::launch::AuthMethod::Windows => al_core::symbols::bc_server::AuthMethod::Windows,
                    al_core::launch::AuthMethod::UserPassword => al_core::symbols::bc_server::AuthMethod::UserPassword,
                    al_core::launch::AuthMethod::AAD => al_core::symbols::bc_server::AuthMethod::AAD,
                };
                let client = al_core::symbols::bc_server::BcServerClient::new_cli(auth, cfg.tenant.clone());
                let url_deps: Vec<(String, al_core::symbols::AppDependency)> = all_deps
                    .iter()
                    .filter_map(|dep| {
                        let sym_dep = al_core::symbols::AppDependency {
                            id: dep.id.clone(), name: dep.name.clone(),
                            publisher: dep.publisher.clone(), version: dep.version.clone(),
                        };
                        cfg.dev_packages_url(dep).map(|url| (url, sym_dep))
                    })
                    .collect();
                let bc_results = client.download_all(&url_deps, &dest).await;
                bc_results
                    .into_iter()
                    .zip(url_deps.iter())
                    .map(|(r, (_url, sym_dep))| match r {
                        Ok(path) => serde_json::json!({
                            "name": sym_dep.name,
                            "status": "ok",
                            "path": path.display().to_string(),
                        }),
                        Err(e) => serde_json::json!({
                            "name": sym_dep.name,
                            "status": "error",
                            "error": e.to_string(),
                        }),
                    })
                    .collect()
            } else {
                let feeds = al_core::project::nuget_feeds();
                let nuget_feeds: Vec<al_core::symbols::NuGetFeed> = feeds
                    .iter()
                    .map(|f| al_core::symbols::NuGetFeed {
                        index_url: f.index_url.clone(),
                    })
                    .collect();
                let nuget_deps: Vec<al_core::symbols::AppDependency> = all_deps
                    .iter()
                    .map(|d| al_core::symbols::AppDependency {
                        id: d.id.clone(),
                        name: d.name.clone(),
                        publisher: d.publisher.clone(),
                        version: d.version.clone(),
                    })
                    .collect();
                let client = al_core::symbols::NuGetClient::new(nuget_feeds);
                let nuget_results = client.download_all(&nuget_deps, &dest).await;
                nuget_results
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| match r {
                        Ok(path) => serde_json::json!({
                            "name": nuget_deps[i].name,
                            "status": "ok",
                            "path": path.display().to_string(),
                        }),
                        Err(e) => serde_json::json!({
                            "name": nuget_deps[i].name,
                            "status": "error",
                            "error": e.to_string(),
                        }),
                    })
                    .collect()
            }
        })
    });

    let success = result.iter().filter(|r| r.get("status").and_then(|v| v.as_str()) == Some("ok")).count();
    let failed = result.iter().filter(|r| r.get("status").and_then(|v| v.as_str()) == Some("error")).count();

    Response {
        id,
        result: Some(serde_json::json!({
            "source": source,
            "downloaded": success,
            "failed": failed,
            "results": result,
        })),
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Insight engine dispatchers
// ---------------------------------------------------------------------------

fn dispatch_trace(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(event_name) = params.get("event").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let max_depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let mut graph = al_core::insight::graph::InsightGraph::new();
    graph.build_from_index(&workspace.symbols);

    let steps = al_core::insight::search::trace_event(&graph, event_name, max_depth);
    // SILENT: serialization of valid Vec<TraceStep> should not fail
    let value = serde_json::to_value(&steps).unwrap_or(serde_json::Value::Null);
    Response { id, result: Some(value), error: None }
}

fn dispatch_entrypoints(workspace: &Workspace, id: u64) -> Response {
    let mut graph = al_core::insight::graph::InsightGraph::new();
    graph.build_from_index(&workspace.symbols);

    let entry_points = al_core::insight::search::find_entry_points(&graph);
    // SILENT: serialization of valid Vec<&InsightNode> should not fail
    let value = serde_json::to_value(&entry_points).unwrap_or(serde_json::Value::Null);
    Response { id, result: Some(value), error: None }
}

fn dispatch_graph_export(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("json");

    let mut graph = al_core::insight::graph::InsightGraph::new();
    graph.build_from_index(&workspace.symbols);

    match format {
        "dot" => {
            let dot = al_core::insight::search::export_dot(&graph);
            Response {
                id,
                result: Some(serde_json::json!({ "format": "dot", "content": dot })),
                error: None,
            }
        }
        _ => {
            let json = al_core::insight::search::export_json(&graph);
            // SILENT: serialization of valid GraphJson should not fail
            let value = serde_json::to_value(&json).unwrap_or(serde_json::Value::Null);
            Response { id, result: Some(value), error: None }
        }
    }
}

fn dispatch_insight_stats(workspace: &Workspace, id: u64) -> Response {
    let mut graph = al_core::insight::graph::InsightGraph::new();
    graph.build_from_index(&workspace.symbols);

    Response {
        id,
        result: Some(serde_json::json!({
            "nodes": graph.node_count(),
            "edges": graph.edge_count(),
        })),
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Debug session dispatcher
// ---------------------------------------------------------------------------

async fn dispatch_debug(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    use al_dap_client::session::DebugSession;
    use al_core::jsonrpc::error_codes;

    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Missing 'cmd' in debug params".to_string(),
                }),
            };
        }
    };

    match cmd {
        "start" => {
            let config_name = params.get("config").and_then(|v| v.as_str()).map(String::from);

            // Grab toolchain and project while not in async context for debug session
            let toolchain = match workspace.toolchain.read().await.clone() {
                Some(tc) => tc,
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: "No toolchain available (ALTool not installed)".to_string(),
                        }),
                    };
                }
            };
            let project_root = match workspace.project.read().await.as_ref().map(|p| p.root.clone()) {
                Some(root) => root,
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: "No project loaded".to_string(),
                        }),
                    };
                }
            };

            // Compilation can take minutes — run in a blocking task
            let config_name_clone = config_name.clone();
            let result = tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
                rt.block_on(async {
                    DebugSession::start(&toolchain.alc, &toolchain.dotnet_root, &project_root, config_name_clone.as_deref())
                        .await
                        .map_err(|e| e.to_string())
                })
            })
            .await;

            match result {
                Ok(Ok(session)) => {
                    let session_id = session.session_id().to_string();
                    *workspace.debug_session.lock().await = Some(session);
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "start",
                            "status": "running",
                            "session": session_id,
                        })),
                        error: None,
                    }
                }
                Ok(Err(e)) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("Debug start failed: {e}"),
                    }),
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("Debug start task failed: {e}"),
                    }),
                },
            }
        }

        "breakpoint" => {
            let file = match params.get("file").and_then(|v| v.as_str()) {
                Some(f) => f.to_string(),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'file' parameter".to_string(),
                        }),
                    };
                }
            };
            let line = params.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let condition = params.get("condition").and_then(|v| v.as_str()).map(String::from);

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: "No active debug session".to_string(),
                    }),
                },
                Some(session) => {
                    let bps: Vec<(u32, Option<&str>)> = vec![(line, condition.as_deref())];
                    match session.set_breakpoints(&file, &bps).await {
                        Ok(verified) => {
                            let bp_json: Vec<serde_json::Value> = verified
                                .iter()
                                .map(|bp| serde_json::to_value(bp).unwrap_or_default())
                                .collect();
                            Response {
                                id,
                                result: Some(serde_json::json!({
                                    "cmd": "breakpoint",
                                    "breakpoints": bp_json,
                                })),
                                error: None,
                            }
                        }
                        Err(e) => Response {
                            id,
                            result: None,
                            error: Some(RpcError {
                                code: error_codes::INTERNAL_ERROR,
                                message: format!("set_breakpoints failed: {e}"),
                            }),
                        },
                    }
                }
            }
        }

        "state" => {
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: "No active debug session".to_string(),
                    }),
                },
                Some(session) => match session.state().await {
                    Ok(state) => Response {
                        id,
                        result: serde_json::to_value(state).ok(), // SILENT: serialization of valid structs should not fail
                        error: None,
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("state() failed: {e}"),
                        }),
                    },
                },
            }
        }

        "eval" => {
            let expr = match params.get("expr").and_then(|v| v.as_str()) {
                Some(e) => e.to_string(),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'expr' parameter".to_string(),
                        }),
                    };
                }
            };

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: "No active debug session".to_string(),
                    }),
                },
                Some(session) => match session.eval(&expr).await {
                    Ok(eval_result) => Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "eval",
                            "result": eval_result.result,
                            "typeName": eval_result.type_name,
                        })),
                        error: None,
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("eval() failed: {e}"),
                        }),
                    },
                },
            }
        }

        "continue" => {
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: "No active debug session".to_string(),
                    }),
                },
                Some(session) => match session.continue_().await {
                    Ok(state) => Response {
                        id,
                        result: serde_json::to_value(state).ok(), // SILENT: serialization of valid structs should not fail
                        error: None,
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("continue() failed: {e}"),
                        }),
                    },
                },
            }
        }

        "step" => {
            let step_type = params
                .get("stepType")
                .and_then(|v| v.as_str())
                .unwrap_or("over")
                .to_string();

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: "No active debug session".to_string(),
                    }),
                },
                Some(session) => match session.step(&step_type).await {
                    Ok(state) => Response {
                        id,
                        result: serde_json::to_value(state).ok(), // SILENT: serialization of valid structs should not fail
                        error: None,
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("step() failed: {e}"),
                        }),
                    },
                },
            }
        }

        "history" => {
            let var_filter = params
                .get("var")
                .and_then(|v| v.as_str())
                .map(String::from);

            let guard = workspace.debug_session.lock().await;
            match guard.as_ref() {
                None => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: "No active debug session".to_string(),
                    }),
                },
                Some(session) => {
                    let hits: Vec<serde_json::Value> = session
                        .history(var_filter.as_deref())
                        .iter()
                        .filter_map(|h| serde_json::to_value(h).ok()) // SILENT: serialization of valid structs should not fail
                        .collect();
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "history",
                            "hits": hits,
                        })),
                        error: None,
                    }
                }
            }
        }

        "stop" => {
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => Response {
                    id,
                    result: Some(serde_json::json!({"cmd": "stop", "status": "stopped"})),
                    error: None,
                },
                Some(session) => {
                    let stop_result = session.stop().await;
                    *guard = None;
                    match stop_result {
                        Ok(()) => Response {
                            id,
                            result: Some(serde_json::json!({"cmd": "stop", "status": "stopped"})),
                            error: None,
                        },
                        Err(e) => Response {
                            id,
                            result: None,
                            error: Some(RpcError {
                                code: error_codes::INTERNAL_ERROR,
                                message: format!("stop() failed: {e}"),
                            }),
                        },
                    }
                }
            }
        }

        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown debug command: {other}"),
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// Snapshot dispatcher
// ---------------------------------------------------------------------------

async fn dispatch_snapshot(id: u64, params: &serde_json::Value) -> Response {
    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Missing 'cmd' parameter (expected: start, list, download)".to_string(),
                }),
            };
        }
    };

    let server_url = params
        .get("serverUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("http://localhost:7049/BC")
        .to_string();
    let company = params
        .get("company")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let output_dir = params
        .get("outputDir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("al-lsp")
                .join("snapshots")
        });
    let username = params
        .get("username")
        .and_then(|v| v.as_str())
        .map(String::from);
    let password = params
        .get("password")
        .and_then(|v| v.as_str())
        .map(String::from);

    let config = al_core::snapshot::SnapshotConfig {
        server_url,
        company,
        output_dir,
        username,
        password,
    };

    match cmd {
        "start" => {
            let description = params.get("description").and_then(|v| v.as_str());
            match al_core::snapshot::start_snapshot(&config, description).await {
                Ok(snapshot_id) => Response {
                    id,
                    result: Some(serde_json::json!({
                        "cmd": "start",
                        "snapshotId": snapshot_id,
                        "status": "started",
                    })),
                    error: None,
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot start failed: {e}"),
                    }),
                },
            }
        }

        "list" => {
            match al_core::snapshot::list_snapshots(&config).await {
                Ok(snapshots) => {
                    let items: Vec<serde_json::Value> = snapshots
                        .iter()
                        .filter_map(|s| serde_json::to_value(s).ok()) // SILENT: serialization of valid structs should not fail
                        .collect();
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "list",
                            "snapshots": items,
                        })),
                        error: None,
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot list failed: {e}"),
                    }),
                },
            }
        }

        "download" => {
            let snapshot_id = match params.get("snapshotId").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'snapshotId' parameter".to_string(),
                        }),
                    };
                }
            };

            match al_core::snapshot::download_snapshot(&config, &snapshot_id).await {
                Ok(path) => Response {
                    id,
                    result: Some(serde_json::json!({
                        "cmd": "download",
                        "snapshotId": snapshot_id,
                        "path": path.display().to_string(),
                        "status": "downloaded",
                    })),
                    error: None,
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot download failed: {e}"),
                    }),
                },
            }
        }

        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown snapshot command: {other}"),
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// Profiling dispatcher
// ---------------------------------------------------------------------------

async fn dispatch_profiling(id: u64, params: &serde_json::Value) -> Response {
    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Missing 'cmd' parameter (expected: start, stop, analyze)".to_string(),
                }),
            };
        }
    };

    let server_url = params
        .get("serverUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("http://localhost:7049/BC")
        .to_string();
    let company = params
        .get("company")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let output_dir = params
        .get("outputDir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("al-lsp")
                .join("profiles")
        });
    let username = params
        .get("username")
        .and_then(|v| v.as_str())
        .map(String::from);
    let password = params
        .get("password")
        .and_then(|v| v.as_str())
        .map(String::from);

    let config = al_core::profiling::ProfilingConfig {
        server_url,
        company,
        output_dir,
        username,
        password,
    };

    match cmd {
        "start" => {
            match al_core::profiling::start_profiling(&config).await {
                Ok(session_id) => Response {
                    id,
                    result: Some(serde_json::json!({
                        "cmd": "start",
                        "sessionId": session_id,
                        "status": "profiling",
                    })),
                    error: None,
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("profiling start failed: {e}"),
                    }),
                },
            }
        }

        "stop" => {
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("profiling-session")
                .to_string();

            match al_core::profiling::stop_profiling(&config, &session_id).await {
                Ok(path) => Response {
                    id,
                    result: Some(serde_json::json!({
                        "cmd": "stop",
                        "sessionId": session_id,
                        "path": path.display().to_string(),
                        "status": "stopped",
                    })),
                    error: None,
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("profiling stop failed: {e}"),
                    }),
                },
            }
        }

        "analyze" => {
            let profile_path = match params.get("path").and_then(|v| v.as_str()) {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'path' parameter for analyze command".to_string(),
                        }),
                    };
                }
            };
            let top_n = params
                .get("topN")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;

            match al_core::profiling::analyze_profile_file(&profile_path, top_n).await {
                Ok(result) => {
                    let hotspots: Vec<serde_json::Value> = result
                        .hotspots
                        .iter()
                        .filter_map(|h| serde_json::to_value(h).ok()) // SILENT: serialization of valid structs should not fail
                        .collect();
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "analyze",
                            "durationMs": result.duration_ms,
                            "hotspots": hotspots,
                            "profilePath": result.profile_path.as_ref().map(|p| p.display().to_string()),
                        })),
                        error: None,
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("profiling analyze failed: {e}"),
                    }),
                },
            }
        }

        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown profiling command: {other}"),
            }),
        },
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
    use super::*;

    /// Verify FNV-1a produces a known-stable value so we catch any accidental
    /// algorithm drift in future edits.
    #[test]
    fn fnv1a64_stable_known_value() {
        // FNV-1a of b"hello" is a well-known constant: 0xa430d84680aabd0b
        assert_eq!(fnv1a64(b"hello"), 0xa430d84680aabd0b);
        // Empty input is the offset basis
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
    }

    /// Verify socket_path produces the same result for the same canonical path.
    #[test]
    fn socket_path_is_deterministic() {
        let p = std::path::Path::new("/tmp");
        let path1 = socket_path(p);
        let path2 = socket_path(p);
        assert_eq!(path1, path2);
        assert!(path1.to_str().unwrap().ends_with(".sock"));
        let filename = path1.file_name().unwrap().to_str().unwrap();
        let hash_part = filename.strip_suffix(".sock").unwrap();
        assert_eq!(hash_part.len(), 16);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
