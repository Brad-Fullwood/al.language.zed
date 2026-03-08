//! Project and workspace management.
//!
//! Handles workspace initialization: discovering the project, loading packages,
//! scanning workspace .al files, and spawning the semantic bridge.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tower_lsp::lsp_types::*;
use tracing::{info, warn};

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

                    // 4. Fetch built-in types
                    match bridge.builtin_types().await {
                        Ok(types) => {
                            info!(count = types.len(), "Loaded built-in types");
                            *server.builtins.write().unwrap() = Arc::new(types);
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to load built-in types");
                        }
                    }

                    *server.semantic.write().await = Some(bridge);
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
        Ok(project) => {
            info!(
                name = %project.app_json.name,
                packages = project.packages.len(),
                "Found AL project"
            );

            // Load .alpackages
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
