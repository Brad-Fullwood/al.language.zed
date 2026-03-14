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
use al_protocol::jsonrpc::{error_codes, Request, Response, RpcError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;

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

/// Compute the deterministic socket path for a project root.
pub fn socket_path(project_root: &Path) -> PathBuf {
    let hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        project_root.hash(&mut h);
        format!("{:016x}", h.finish())
    };
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
    }

    // Remove stale socket file if it exists
    let _ = tokio::fs::remove_file(&sock_path).await;

    let listener = UnixListener::bind(&sock_path)?;
    tracing::info!(path = %sock_path.display(), project = %project_root.display(), "daemon: listening");

    // Register global path for cleanup on exit/signals
    let _ = SOCKET_PATH.set(sock_path.clone());
    let _cleanup = SocketCleanup;

    // Initialize workspace
    let workspace = Arc::new(Workspace::new());
    initialize_daemon_workspace(&workspace, &project_root).await;

    let last_activity = Arc::new(Mutex::new(Instant::now()));

    // Idle timeout checker
    let activity_clone = Arc::clone(&last_activity);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let elapsed = activity_clone.lock().await.elapsed();
            if elapsed >= IDLE_TIMEOUT {
                tracing::info!(idle_secs = elapsed.as_secs(), "daemon: idle timeout, shutting down");
                std::process::exit(0);
            }
        }
    });

    // Accept connections
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let ws = Arc::clone(&workspace);
                let activity = Arc::clone(&last_activity);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, ws, activity).await {
                        tracing::warn!(error = %e, "daemon: connection error");
                    }
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "daemon: accept error");
            }
        }
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    workspace: Arc<Workspace>,
    last_activity: Arc<Mutex<Instant>>,
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
            Ok(req) => dispatch_request(&workspace, req),
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

fn dispatch_request(workspace: &Workspace, req: Request) -> Response {
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
        "ping" => Response { id, result: Some(serde_json::json!("pong")), error: None },
        "shutdown" => {
            tracing::info!("daemon: shutdown requested");
            std::process::exit(0);
        }
        "status" => {
            let status = serde_json::json!({
                "pid": std::process::id(),
                "indexedSymbols": workspace.symbols.len(),
                "workspaceFiles": workspace.workspace_files.len(),
                "workspaceObjects": workspace.workspace_objects.len(),
                "builtinTypes": workspace.builtins.read().unwrap().len(),
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
    url::Url::parse(uri_str).ok() // URI parsing failure means bad input
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
    let value = result.and_then(|r| serde_json::to_value(r).ok());
    Response { id, result: value, error: None }
}

fn dispatch_folding_ranges(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let result = al_core::queries::folding::folding_ranges(workspace, &uri);
    let value = result.and_then(|r| serde_json::to_value(r).ok());
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

            // Load symbol packages
            let loaded = workspace.symbols.load_packages(&project.packages);
            let total_symbols: usize = loaded.iter().map(|p| p.objects.len()).sum();
            tracing::info!(packages = loaded.len(), symbols = total_symbols, "daemon: loaded symbol packages");

            // Scan workspace .al files
            let al_files = scan_al_files(&project.root);
            for path in &al_files {
                if let Ok(content) = std::fs::read_to_string(path) {
                    let result = al_syntax::AlParser::parse_quick(&content);
                    if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &content) {
                        let obj_name = obj_info.name.to_lowercase();
                        workspace.workspace_objects.insert(obj_name.clone(), path.clone());
                        workspace.file_to_object.insert(path.clone(), obj_name);
                    }
                    workspace.workspace_files.insert(path.clone(), content.clone());

                    // Also open in DocumentStore for query access
                    if let Ok(uri) = url::Url::from_file_path(path) {
                        workspace.documents.open(uri, content);
                    }
                }
            }

            // Store project info
            *workspace.project.blocking_write() = Some(project);

            tracing::info!(
                symbols = workspace.symbols.len(),
                workspace_files = workspace.workspace_files.len(),
                workspace_objects = workspace.workspace_objects.len(),
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
            *workspace.toolchain.blocking_write() = Some(tc);
        }
        Err(e) => {
            tracing::info!(error = %e, "daemon: no toolchain (syntax-only mode)");
        }
    }
}

fn scan_al_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .max_depth(10)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.') && name != "node_modules" && name != ".alpackages"
        });
    for entry in walker.flatten() {
        if entry.file_type().is_file() {
            if let Some(ext) = entry.path().extension() {
                if ext.eq_ignore_ascii_case("al") {
                    files.push(entry.into_path());
                }
            }
        }
    }
    files
}
