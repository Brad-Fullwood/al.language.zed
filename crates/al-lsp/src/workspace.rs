//! Project and workspace management.
//!
//! Handles workspace initialization: discovering the project, loading packages,
//! scanning workspace .al files, and initializing the semantic bridge lazily.
//! Auto-downloads missing BC symbol packages via BC server (launch.json) or NuGet.

use std::path::{Path, PathBuf};

use al_syntax::AlParser;
use tower_lsp::lsp_types::*;
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
/// Called during LSP `initialized` notification. Failures are logged but
/// do not prevent the server from operating (graceful degradation).
pub(crate) async fn initialize_workspace(server: &AlServer, root_uri: Option<&Url>) {
    // 1. Discover toolchain
    match al_core::toolchain::find_toolchain() {
        Ok(tc) => {
            info!(version = %tc.version, "Found AL toolchain");
            *server.workspace.toolchain.write().await = Some(tc.clone());

            // Load builtins + error codes from disk cache (no bridge needed, <1ms)
            server.load_caches_from_disk(&tc.version).await;
        }
        Err(e) => {
            warn!(error = %e, "AL toolchain not found (continuing without)");
        }
    }

    // 2. Find project and load packages
    let workspace_root = root_uri
        .and_then(|u| u.to_file_path().ok())
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
                        prompt_download_symbols(&server.client, deps.len(), has_server).await
                    {
                        let downloaded = match source {
                            DownloadSource::Server => {
                                download_symbols_from_server(&project, &deps, &server.client).await
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

            // Load .alpackages / cached packages
            if !project.packages.is_empty() {
                let loaded = server.workspace.symbols.load_packages(&project.packages);
                info!(
                    loaded = loaded.len(),
                    total_symbols = server.workspace.symbols.len(),
                    "Loaded symbol packages"
                );
            }

            // Check for packages without source and prompt before generating outlines
            check_source_availability(server, &project.packages).await;

            // Load runtime enum definitions (compiler built-ins not in any package)
            server.workspace.symbols.load_runtime_enums();

            *server.workspace.project.write().await = Some(project.clone());

            // 5. Scan workspace for .al files
            scan_workspace_files(server, &project.root);
        }
        Err(e) => {
            warn!(error = %e, "No AL project found (continuing without packages)");

            // Still try to scan for .al files in the workspace root
            scan_workspace_files(server, &workspace_root);
        }
    }
}

/// Check which loaded packages lack `.al` source files and prompt the user
/// before generating symbol outlines as a fallback.
async fn check_source_availability(server: &AlServer, packages: &[PathBuf]) {
    let no_source: Vec<String> = packages
        .iter()
        .filter_map(|path| {
            if al_symbols::virtual_file::app_has_source(path) {
                return None;
            }
            // Extract a readable name from the filename
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            Some(stem.to_string())
        })
        .collect();

    if no_source.is_empty() {
        // All packages have source — allow fallback unconditionally (shouldn't be needed)
        server
            .workspace.outline_fallback_approved
            .store(true, std::sync::atomic::Ordering::Relaxed);
        return;
    }

    let names = no_source.join(", ");
    let message = format!(
        "No source available for the following dependencies: {}. Generate outline from symbols?",
        names
    );

    let actions = vec![
        MessageActionItem {
            title: "Yes".to_string(),
            properties: Default::default(),
        },
        MessageActionItem {
            title: "No".to_string(),
            properties: Default::default(),
        },
    ];

    match server
        .client
        .show_message_request(MessageType::INFO, message, Some(actions))
        .await
    {
        Ok(Some(action)) if action.title == "Yes" => {
            info!(
                packages = ?no_source,
                "User approved symbol outline generation for packages without source"
            );
            server
                .workspace.outline_fallback_approved
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        _ => {
            info!("User declined symbol outline generation");
        }
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
    let message_sink: al_symbols::bc_server::MessageSink =
        std::sync::Arc::new(move |msg| {
            let c = lsp.clone();
            let m = msg.to_string();
            tokio::spawn(async move {
                c.show_message(tower_lsp::lsp_types::MessageType::INFO, m).await;
            });
        });
    let client = al_symbols::bc_server::BcServerClient::new(config.clone(), message_sink);
    let results = client.download_all(deps, &dest).await;

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

    // Convert al_core types to al_symbols::nuget types
    let nuget_deps: Vec<al_symbols::nuget::AppDependency> = deps
        .iter()
        .map(|d| al_symbols::nuget::AppDependency {
            id: d.id.clone(),
            name: d.name.clone(),
            publisher: d.publisher.clone(),
            version: d.version.clone(),
        })
        .collect();

    let feeds: Vec<al_symbols::nuget::NuGetFeed> = al_core::project::nuget_feeds()
        .iter()
        .map(|f| al_symbols::nuget::NuGetFeed {
            index_url: f.index_url.clone(),
        })
        .collect();

    let client = al_symbols::nuget::NuGetClient::new(feeds);
    let results = client.download_all(&nuget_deps, dest).await;

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

    // Reload symbol index
    let loaded = server.workspace.symbols.load_packages(&packages);
    server.workspace.symbols.load_runtime_enums();
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

/// Maximum number of .al files to scan. Prevents runaway memory usage
/// if a workspace root accidentally includes a huge directory tree.
const MAX_WORKSPACE_FILES: usize = 10_000;

/// Scan a directory for .al files and add them to the workspace_files map.
fn scan_workspace_files(server: &AlServer, root: &Path) {
    let mut count = 0;
    scan_dir_recursive(root, server, &mut count, 0);
    if count > 0 {
        if count >= MAX_WORKSPACE_FILES {
            warn!(count, limit = MAX_WORKSPACE_FILES, "Workspace scan hit file limit — some files may be missing");
        }
        info!(count, "Scanned workspace .al files");
    }
}

fn scan_dir_recursive(dir: &Path, server: &AlServer, count: &mut usize, depth: usize) {
    if depth > 10 || *count >= MAX_WORKSPACE_FILES {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Skip hidden directories and .alpackages
        if path.is_dir() {
            let dir_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            if dir_name.starts_with('.')
                || dir_name == "node_modules"
                || dir_name == ".alpackages"
            {
                continue;
            }

            scan_dir_recursive(&path, server, count, depth + 1);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("al"))
        {
            if let Ok(content) = std::fs::read_to_string(&path) {
                // Index the object name for fast lookups
                {
                    let result = AlParser::parse_quick(&content);
                    if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &content) {
                        let obj_name = obj_info.name.to_lowercase();
                        server.workspace.workspace_objects.insert(obj_name.clone(), path.clone());
                        server.workspace.file_to_object.insert(path.clone(), obj_name);
                    }
                }
                server.workspace.workspace_files.insert(path, content);
                *count += 1;
            }
        }
    }
}

/// Handle workspace/symbol request.
pub(crate) fn handle_workspace_symbol(
    server: &AlServer,
    query: &str,
) -> Option<Vec<SymbolInformation>> {
    let mut results = Vec::new();

    // Search symbol index
    let entries = if query.is_empty() {
        server.workspace.symbols.search("", 50)
    } else {
        server.workspace.symbols.search(query, 50)
    };

    #[allow(deprecated)]
    for entry in &entries {
        let kind = match entry.kind {
            al_symbols::ObjectKind::Table | al_symbols::ObjectKind::TableExtension => {
                SymbolKind::STRUCT
            }
            al_symbols::ObjectKind::Page | al_symbols::ObjectKind::PageExtension => {
                SymbolKind::CLASS
            }
            al_symbols::ObjectKind::Codeunit => SymbolKind::MODULE,
            al_symbols::ObjectKind::Report | al_symbols::ObjectKind::ReportExtension => {
                SymbolKind::FILE
            }
            al_symbols::ObjectKind::Enum | al_symbols::ObjectKind::EnumExtension => {
                SymbolKind::ENUM
            }
            al_symbols::ObjectKind::Interface => SymbolKind::INTERFACE,
            _ => SymbolKind::OBJECT,
        };

        results.push(SymbolInformation {
            name: entry.name.clone(),
            kind,
            tags: None,
            deprecated: None,
            location: Location {
                uri: Url::parse("file:///unknown").unwrap(),
                range: Range::default(),
            },
            container_name: Some(format!("{} ({} {})", entry.package, entry.kind, entry.id)),
        });
    }

    // Search workspace files using the object name index
    let query_lower = query.to_lowercase();
    for ws_entry in server.workspace.workspace_objects.iter() {
        let obj_name_lower = ws_entry.key();
        let file_path = ws_entry.value();

        if !query.is_empty() && !obj_name_lower.contains(&query_lower) {
            continue;
        }

        if let Some(file_text_entry) = server.workspace.workspace_files.get(file_path) {
            let file_text = file_text_entry.value();
            let result = AlParser::parse_quick(file_text);

            if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, file_text) {
                if let Ok(file_uri) = Url::from_file_path(file_path) {
                    #[allow(deprecated)]
                    results.push(SymbolInformation {
                        name: obj_info.name,
                        kind: SymbolKind::OBJECT,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: file_uri,
                            range: al_syntax::ts_range_to_lsp(&obj_info.range),
                        },
                        container_name: Some(obj_info.kind),
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
