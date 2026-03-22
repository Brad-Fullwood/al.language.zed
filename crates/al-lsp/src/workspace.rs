//! Project and workspace management.
//!
//! Handles workspace initialization: discovering the project, loading packages,
//! scanning workspace .al files, and initializing the semantic bridge lazily.
//! Auto-downloads missing BC symbol packages via BC server (launch.json) or NuGet.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use al_core::workspace::Workspace;
use tower_lsp::lsp_types::*;
use tower_lsp::Client;
use tracing::{debug, info, warn};

use crate::server::AlServer;

/// Where to download symbol packages from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DownloadSource {
    /// From a running BC instance (uses .zed/debug.json connection details).
    Server,
    /// From NuGet package feeds.
    NuGet,
}

/// Initialize the workspace: discover toolchain, load packages, scan files.
///
/// Called from the background task spawned by the `initialized` notification handler
/// (ISSUE-026 fix). Failures are logged but do not prevent the server from operating
/// (graceful degradation).
pub(crate) async fn initialize_workspace(workspace: Arc<Workspace>, client: Client, root_uri: Option<Url>) {
    // Signal that workspace initialization has begun
    client.log_message(MessageType::INFO, "AL workspace: initializing...").await;

    // 1. Discover toolchain
    match al_core::toolchain::find_toolchain() {
        Ok(tc) => {
            info!(version = %tc.version, "Found AL toolchain");
            *workspace.toolchain.write().await = Some(tc.clone());

            // Load builtins + error codes from disk cache (no bridge needed, <1ms)
            load_caches_from_disk(&workspace, &tc.version).await;
        }
        Err(e) => {
            warn!(error = %e, "AL toolchain not found (continuing without)");
            client
                .show_message(MessageType::WARNING, format!("AL toolchain not found: {e}"))
                .await;
        }
    }

    // 2. Find project and load packages
    let workspace_root = root_uri
        .as_ref()
        .and_then(|u| u.to_file_path().ok()) // SILENT: non-file URIs legitimately have no path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match al_core::project::find_project(&workspace_root) {
        Ok(mut project) => {
            info!(
                name = %project.app_json.name,
                packages = project.packages.len(),
                "Found AL project"
            );

            // Auto-download missing packages if needed.
            // Prompt the user and let them choose the download source.
            if project.packages.is_empty() {
                let deps = project.all_dependencies();
                if !deps.is_empty() {
                    let has_server = !project.server_configs.is_empty();
                    if let Some(source) =
                        prompt_download_symbols(&client, deps.len(), has_server).await
                    {
                        let downloaded = match source {
                            DownloadSource::Server => {
                                download_symbols_from_server(&project, &deps, &client).await
                            }
                            DownloadSource::NuGet => {
                                download_packages_nuget(&deps, &project.packages_dir).await
                            }
                        };
                        if !downloaded.is_empty() {
                            project.packages = downloaded;
                        }
                    }
                }
            }

            // Load .alpackages / cached packages (disk cache for fast warm starts)
            if !project.packages.is_empty() {
                let cache = al_core::symbols::cache::SymbolCache::default_location();
                let loaded = workspace.symbols.load_packages_cached(&project.packages, &cache);
                info!(
                    loaded = loaded.len(),
                    total_symbols = workspace.symbols.len(),
                    "Loaded symbol packages"
                );
            }

            // Log which packages lack embedded source (outlines rendered automatically)
            log_source_availability(&project.packages);

            // Load runtime enum definitions (compiler built-ins not in any package)
            workspace.symbols.load_runtime_enums();
            // Invalidate insight graph -- packages changed (ISSUE-132 fix)
            workspace.invalidate_insight_graph();

            *workspace.project.write().await = Some(project.clone());

            // 5. Scan workspace for .al files
            let count = workspace.file_index.scan(&project.root);
            if count > 0 {
                info!(count, "Scanned workspace .al files");
            }
        }
        Err(e) => {
            warn!(error = %e, "No AL project found (continuing without packages)");
            client
                .show_message(MessageType::INFO, format!("No AL project found: {e}"))
                .await;

            // Still try to scan for .al files in the workspace root
            let count = workspace.file_index.scan(&workspace_root);
            if count > 0 {
                info!(count, "Scanned workspace .al files");
            }
        }
    }

    client.log_message(MessageType::INFO, "AL workspace: ready").await;
}

/// Load builtins and error codes from disk cache (fast path, no bridge needed).
///
/// Extracted from `AlServer::load_caches_from_disk` to be callable from the background init task.
async fn load_caches_from_disk(workspace: &Workspace, version: &str) {
    // SILENT: .unwrap_or_else recovers from RwLock poison by taking the inner value
    if workspace.builtins.read().unwrap_or_else(|e| e.into_inner()).is_empty() {
        if let Some(cached) = al_core::semantic_types::cache::read_builtins(version) {
            info!(count = cached.len(), "Loaded built-in types from disk cache");
            al_core::semantic::set_builtins(workspace, cached, version);
        }
    }
    if workspace.error_codes.is_empty() {
        if let Some(cached) = al_core::semantic_types::cache::read_error_codes(version) {
            info!(count = cached.len(), "Loaded error codes from disk cache");
            for ec in cached {
                workspace.error_codes.insert(ec.code.clone(), ec.message.clone());
            }
        }
    }
}

/// Log which loaded packages lack `.al` source files.
/// Outlines are always generated from symbol metadata — no user prompt needed.
fn log_source_availability(packages: &[PathBuf]) {
    let no_source: Vec<String> = packages
        .iter()
        .filter_map(|path| {
            if al_core::symbols::virtual_file::app_has_source(path) {
                return None;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            Some(stem.to_string())
        })
        .collect();

    if !no_source.is_empty() {
        info!(
            packages = ?no_source,
            "Packages without embedded source — outlines will be rendered from symbol metadata"
        );
    }
}

/// Prompt the user to choose a download source via `window/showMessageRequest`.
///
/// Returns `Some(source)` if the user picks an option, `None` if dismissed.
async fn prompt_download_symbols(
    client: &tower_lsp::Client,
    dep_count: usize,
    has_server_config: bool,
) -> Option<DownloadSource> {
    let message = format!(
        "AL project has {} missing symbol package{}. Download now?",
        dep_count,
        if dep_count == 1 { "" } else { "s" }
    );

    let mut actions = Vec::new();
    if has_server_config {
        actions.push(MessageActionItem {
            title: "From Server".to_string(),
            properties: Default::default(),
        });
    }
    actions.push(MessageActionItem {
        title: "From NuGet".to_string(),
        properties: Default::default(),
    });

    match client
        .show_message_request(MessageType::INFO, message, Some(actions))
        .await
    {
        Ok(Some(action)) if action.title == "From Server" => {
            info!("User chose to download from BC server");
            Some(DownloadSource::Server)
        }
        Ok(Some(action)) if action.title == "From NuGet" => {
            info!("User chose to download from NuGet");
            Some(DownloadSource::NuGet)
        }
        Ok(_) => {
            info!("User dismissed symbol download prompt");
            None
        }
        Err(e) => {
            // Fall back to NuGet if the client doesn't support showMessageRequest
            warn!(error = %e, "showMessageRequest failed, falling back to NuGet");
            Some(DownloadSource::NuGet)
        }
    }
}

/// Download symbols from a running BC instance defined in launch.json.
///
/// Uses the first available server config. Returns downloaded .app file paths.
async fn download_symbols_from_server(
    project: &al_core::project::AlProject,
    deps: &[al_core::project::AppDependency],
    lsp_client: &tower_lsp::Client,
) -> Vec<PathBuf> {
    let configs = &project.server_configs;
    if configs.is_empty() {
        debug!("No BC server configs in launch.json, skipping server download");
        return Vec::new();
    }

    let config = &configs[0];
    info!(
        server = %config.display_name(),
        deps = deps.len(),
        "Downloading symbols from BC server"
    );

    let dest = project.root.join(".alpackages");
    // Wire auth messages to LSP showMessage so the user sees device code prompts
    let lsp = lsp_client.clone();
    let message_sink: al_core::symbols::bc_server::MessageSink =
        std::sync::Arc::new(move |msg| {
            let c = lsp.clone();
            let m = msg.to_string();
            tokio::spawn(async move {
                c.show_message(tower_lsp::lsp_types::MessageType::INFO, m).await;
            });
        });
    let auth = match config.authentication {
        al_core::launch::AuthMethod::Windows => al_core::symbols::bc_server::AuthMethod::Windows,
        al_core::launch::AuthMethod::UserPassword => al_core::symbols::bc_server::AuthMethod::UserPassword,
        al_core::launch::AuthMethod::AAD => al_core::symbols::bc_server::AuthMethod::AAD,
    };
    let insecure_tls = config.accept_invalid_certs;
    let client = al_core::symbols::bc_server::BcServerClient::new(auth, config.tenant.clone(), message_sink, insecure_tls);
    // al_core::project::AppDependency is re-exported from al-symbols — clone directly.
    let url_deps: Vec<(String, al_core::symbols::nuget::AppDependency)> = deps
        .iter()
        .filter_map(|dep| {
            config.dev_packages_url(dep).map(|url| (url, dep.clone()))
        })
        .collect();
    let results = client.download_all(&url_deps, &dest).await;

    let mut downloaded = Vec::new();
    for (i, result) in results.into_iter().enumerate() {
        match result {
            Ok(path) => {
                info!(
                    package = %deps[i].name,
                    path = %path.display(),
                    "Downloaded from BC server"
                );
                downloaded.push(path);
            }
            Err(e) => {
                warn!(
                    package = %deps[i].name,
                    error = %e,
                    "Failed to download from BC server"
                );
            }
        }
    }

    downloaded
}

/// Download symbol packages from NuGet into the project's .alpackages directory.
///
/// Returns paths to successfully downloaded .app files.
async fn download_packages_nuget(
    deps: &[al_core::project::AppDependency],
    dest: &Path,
) -> Vec<PathBuf> {
    info!(
        count = deps.len(),
        dest = %dest.display(),
        "Downloading symbol packages from NuGet"
    );

    // al_core::project::AppDependency is re-exported from al-symbols — pass directly.
    let feeds: Vec<al_core::symbols::nuget::NuGetFeed> = al_core::project::nuget_feeds()
        .iter()
        .map(|f| al_core::symbols::nuget::NuGetFeed {
            index_url: f.index_url.clone(),
        })
        .collect();

    let client = al_core::symbols::nuget::NuGetClient::new(feeds);
    let results = client.download_all(deps, dest).await;

    let mut downloaded = Vec::new();
    for (i, result) in results.into_iter().enumerate() {
        match result {
            Ok(path) => {
                info!(
                    package = %deps[i].name,
                    path = %path.display(),
                    "Downloaded symbol package from NuGet"
                );
                downloaded.push(path);
            }
            Err(e) => {
                warn!(
                    package = %deps[i].name,
                    error = %e,
                    "Failed to download symbol package from NuGet"
                );
            }
        }
    }

    downloaded
}

/// Handle the `al.downloadSymbols*` commands.
///
/// Downloads symbols from the specified source and reloads the symbol index.
pub(crate) async fn download_symbols_command(server: &AlServer, source: DownloadSource) {
    let project = server.workspace.project.read().await.clone();
    let Some(project) = project else {
        warn!("No AL project found — cannot download symbols");
        server
            .client
            .show_message(MessageType::WARNING, "No AL project found")
            .await;
        return;
    };

    let deps = project.all_dependencies();
    if deps.is_empty() {
        info!("No dependencies to download");
        server
            .client
            .show_message(MessageType::INFO, "No dependencies to download")
            .await;
        return;
    }

    let source_name = match source {
        DownloadSource::Server => "BC server",
        DownloadSource::NuGet => "NuGet",
    };

    server
        .client
        .show_message(
            MessageType::INFO,
            format!("Downloading {} symbol packages from {}...", deps.len(), source_name),
        )
        .await;

    let packages = match source {
        DownloadSource::Server => download_symbols_from_server(&project, &deps, &server.client).await,
        DownloadSource::NuGet => {
            download_packages_nuget(&deps, &project.packages_dir).await
        }
    };

    if packages.is_empty() {
        server
            .client
            .show_message(
                MessageType::WARNING,
                format!("Failed to download symbol packages from {}", source_name),
            )
            .await;
        return;
    }

    // Reload symbol index (with cache for fast subsequent starts)
    let cache = al_core::symbols::cache::SymbolCache::default_location();
    let loaded = server.workspace.symbols.load_packages_cached(&packages, &cache);
    server.workspace.symbols.load_runtime_enums();
    // Invalidate insight graph -- packages changed (ISSUE-132 fix)
    server.workspace.invalidate_insight_graph();
    info!(
        loaded = loaded.len(),
        total_symbols = server.workspace.symbols.len(),
        source = source_name,
        "Reloaded symbol packages after download"
    );

    server
        .client
        .show_message(
            MessageType::INFO,
            format!(
                "Downloaded {} packages from {} ({} symbols)",
                loaded.len(),
                source_name,
                server.workspace.symbols.len()
            ),
        )
        .await;
}

/// Handle workspace/symbol request.
pub(crate) fn handle_workspace_symbol(
    server: &AlServer,
    query: &str,
) -> Option<Vec<SymbolInformation>> {
    let mut results = Vec::new();

    // Package symbols (from .app NuGet packages) are intentionally excluded from
    // workspace/symbol. Like VS Code, this feature returns only the user's project
    // files. Package symbols are accessible via completion, hover, and go-to-definition
    // which query the symbol index directly.

    // Search workspace files using the object name index.
    // Read object metadata from the cache built at scan/open time — avoids re-parsing
    // every workspace file on every workspace/symbol request (ISSUE-057 fix).
    let query_lower = query.to_lowercase();
    for ws_entry in server.workspace.file_index.objects.iter() {
        let obj_name_lower = ws_entry.key();
        let file_path = ws_entry.value().clone();

        if !query.is_empty() && !obj_name_lower.contains(&query_lower) {
            continue;
        }

        if let Some(cached) = server.workspace.file_index.object_info.get(&file_path) {
            let obj_info = cached.value();
            if let Some(file_text_entry) = server.workspace.file_index.files.get(&file_path) {
                if let Ok(file_uri) = Url::from_file_path(&file_path) {
                    #[allow(deprecated)]
                    results.push(SymbolInformation {
                        name: obj_info.name.clone(),
                        kind: SymbolKind::OBJECT,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: file_uri,
                            range: al_core::syntax::ts_range_to_lsp(&obj_info.range, file_text_entry.value().as_bytes()),
                        },
                        container_name: Some(obj_info.kind.clone()),
                    });
                }
            }
        }
    }

    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}
