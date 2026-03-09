//! Project and workspace management.
//!
//! Handles workspace initialization: discovering the project, loading packages,
//! scanning workspace .al files, and spawning the semantic bridge.
//! Auto-downloads missing BC symbol packages via BC server (launch.json) or NuGet.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tower_lsp::lsp_types::*;
use tracing::{debug, info, warn};

use crate::server::AlServer;


/// Initialize the workspace: discover toolchain, load packages, scan files.
///
/// Called during LSP `initialized` notification. Failures are logged but
/// do not prevent the server from operating (graceful degradation).
pub(crate) async fn initialize_workspace(server: &AlServer, root_uri: Option<&Url>) {
    // 1. Discover toolchain
    match al_discovery::find_toolchain() {
        Ok(tc) => {
            info!(version = %tc.version, "Found AL toolchain");
            *server.toolchain.write().await = Some(tc.clone());

            // 3. Spawn semantic bridge (optional)
            match al_semantic::SemanticBridge::spawn(&tc).await {
                Ok(bridge) => {
                    info!("Semantic bridge spawned successfully");
                    *server.semantic.write().await = Some(bridge);
                    server.ensure_builtins_loaded().await;
                }
                Err(e) => {
                    warn!(error = %e, "Failed to spawn semantic bridge (continuing without)");
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "AL toolchain not found (continuing without)");
        }
    }

    // 2. Find project and load packages
    let workspace_root = root_uri
        .and_then(|u| u.to_file_path().ok())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match al_discovery::find_project(&workspace_root) {
        Ok(mut project) => {
            info!(
                name = %project.app_json.name,
                packages = project.packages.len(),
                "Found AL project"
            );

            // Auto-download missing packages if needed.
            // Strategy: try BC server (launch.json) first, fall back to NuGet.
            if project.packages.is_empty() {
                let deps = project.all_dependencies();
                if !deps.is_empty() {
                    let downloaded =
                        download_symbols_from_server(&project, &deps).await;
                    if !downloaded.is_empty() {
                        project.packages = downloaded;
                    } else {
                        // Fall back to NuGet
                        let downloaded = download_packages_nuget(&deps).await;
                        if !downloaded.is_empty() {
                            project.packages = downloaded;
                        }
                    }
                }
            }

            // Load .alpackages / cached packages
            if !project.packages.is_empty() {
                let loaded = server.symbols.load_packages(&project.packages);
                info!(
                    loaded = loaded.len(),
                    total_symbols = server.symbols.len(),
                    "Loaded symbol packages"
                );
            }

            *server.project.write().await = Some(project.clone());

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

/// Get the symbol cache directory (~/.cache/al-lsp/packages/).
/// Creates the directory if it doesn't exist.
fn symbol_cache_dir() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("packages");

    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        warn!(error = %e, path = %cache_dir.display(), "Failed to create cache directory");
    }

    cache_dir
}

/// Download symbols from a running BC instance defined in launch.json.
///
/// Uses the first available server config. Returns downloaded .app file paths.
async fn download_symbols_from_server(
    project: &al_discovery::AlProject,
    deps: &[al_discovery::AppDependency],
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
    let client = al_symbols::bc_server::BcServerClient::new(config.clone());
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

/// Download missing symbol packages from NuGet to the cache directory.
///
/// Returns paths to successfully downloaded .app files.
async fn download_packages_nuget(deps: &[al_discovery::AppDependency]) -> Vec<PathBuf> {
    let cache_dir = symbol_cache_dir();

    info!(
        count = deps.len(),
        cache = %cache_dir.display(),
        "Downloading missing symbol packages"
    );

    // Convert al_discovery types to al_symbols::nuget types
    let nuget_deps: Vec<al_symbols::nuget::AppDependency> = deps
        .iter()
        .map(|d| al_symbols::nuget::AppDependency {
            id: d.id.clone(),
            name: d.name.clone(),
            publisher: d.publisher.clone(),
            version: d.version.clone(),
        })
        .collect();

    let feeds: Vec<al_symbols::nuget::NuGetFeed> = al_discovery::nuget_feeds()
        .iter()
        .map(|f| al_symbols::nuget::NuGetFeed {
            index_url: f.index_url.clone(),
        })
        .collect();

    let client = al_symbols::nuget::NuGetClient::new(feeds);
    let results = client.download_all(&nuget_deps, &cache_dir).await;

    let mut downloaded = Vec::new();
    for (i, result) in results.into_iter().enumerate() {
        match result {
            Ok(path) => {
                info!(
                    package = %deps[i].name,
                    path = %path.display(),
                    "Downloaded symbol package"
                );
                downloaded.push(path);
            }
            Err(e) => {
                warn!(
                    package = %deps[i].name,
                    error = %e,
                    "Failed to download symbol package (continuing without)"
                );
            }
        }
    }

    // Also scan cache dir for any previously downloaded .app files
    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext.eq_ignore_ascii_case("app")) {
                if !downloaded.contains(&path) {
                    debug!(path = %path.display(), "Found cached package");
                    downloaded.push(path);
                }
            }
        }
    }

    downloaded
}

/// Handle the `al.downloadSymbols` command.
///
/// Downloads symbols from the BC server (launch.json), falling back to NuGet.
/// Reloads the symbol index after download.
pub(crate) async fn download_symbols_command(server: &AlServer) {
    let project = server.project.read().await.clone();
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

    server
        .client
        .show_message(
            MessageType::INFO,
            format!("Downloading {} symbol packages...", deps.len()),
        )
        .await;

    // Try BC server first, then NuGet
    let downloaded = download_symbols_from_server(&project, &deps).await;
    let packages = if !downloaded.is_empty() {
        downloaded
    } else {
        info!("BC server download failed or unavailable, falling back to NuGet");
        download_packages_nuget(&deps).await
    };

    if packages.is_empty() {
        server
            .client
            .show_message(MessageType::WARNING, "Failed to download symbol packages")
            .await;
        return;
    }

    // Reload symbol index
    let loaded = server.symbols.load_packages(&packages);
    info!(
        loaded = loaded.len(),
        total_symbols = server.symbols.len(),
        "Reloaded symbol packages after download"
    );

    server
        .client
        .show_message(
            MessageType::INFO,
            format!(
                "Downloaded {} packages ({} symbols)",
                loaded.len(),
                server.symbols.len()
            ),
        )
        .await;
}

/// Scan a directory for .al files and add them to the workspace_files map.
fn scan_workspace_files(server: &AlServer, root: &Path) {
    let mut count = 0;
    scan_dir_recursive(root, server, &mut count, 0);
    if count > 0 {
        info!(count, "Scanned workspace .al files");
    }
}

fn scan_dir_recursive(dir: &Path, server: &AlServer, count: &mut usize, depth: usize) {
    if depth > 10 {
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
            .map_or(false, |ext| ext.eq_ignore_ascii_case("al"))
        {
            if let Ok(content) = std::fs::read_to_string(&path) {
                // Index the object name for fast lookups
                {
                    let mut parser = server.parser.lock().unwrap();
                    let result = parser.parse(&content);
                    if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &content) {
                        server.workspace_objects.insert(obj_info.name.to_lowercase(), path.clone());
                    }
                }
                server.workspace_files.insert(path, content);
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
        server.symbols.search("", 50)
    } else {
        server.symbols.search(query, 50)
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
    for ws_entry in server.workspace_objects.iter() {
        let obj_name_lower = ws_entry.key();
        let file_path = ws_entry.value();

        if !query.is_empty() && !obj_name_lower.contains(&query_lower) {
            continue;
        }

        if let Some(file_text_entry) = server.workspace_files.get(file_path) {
            let file_text = file_text_entry.value();
            let mut parser = server.parser.lock().unwrap();
            let result = parser.parse(file_text);

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
