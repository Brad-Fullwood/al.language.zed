//! Project and workspace management.
//!
//! Handles workspace initialization: discovering the project, loading packages,
//! scanning workspace .al files, and initializing the semantic bridge lazily.
//! Auto-downloads missing BC symbol packages via BC server (launch.json) or NuGet.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::workspace::Workspace;
use tower_lsp::lsp_types::*;
use tower_lsp::Client;
use tracing::{debug, info, warn};

use super::AlServer;

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
///
/// `ready_flag` and `init_notify` are signalled as soon as the file scan is complete
/// so that LSP request handlers can serve workspace/symbol and other queries without
/// waiting for the (potentially blocking) package-download prompt to finish.
pub(crate) async fn initialize_workspace(
    workspace: Arc<Workspace>,
    client: Client,
    root_uri: Option<Url>,
    ready_flag: Arc<AtomicBool>,
    init_notify: Arc<tokio::sync::Notify>,
) {
    // Signal that workspace initialization has begun
    client
        .log_message(MessageType::INFO, "AL workspace: initializing...")
        .await;

    // 1. Discover toolchain.
    // find_toolchain() does sync filesystem traversal (PATH walk, ALTool
    // probe) which can take tens of ms — must run on a blocking thread so
    // we don't stall the tokio runtime during init.
    let toolchain_result = tokio::task::spawn_blocking(crate::toolchain::find_toolchain)
        .await
        .unwrap_or_else(|join_err| {
            Err(crate::errors::DiscoveryError::Io(std::io::Error::other(
                format!("find_toolchain task panicked: {join_err}"),
            )))
        });
    match toolchain_result {
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

    match crate::project::find_project(&workspace_root) {
        Ok(mut project) => {
            info!(
                name = %project.app_json.name,
                packages = project.packages.len(),
                "Found AL project"
            );

            // 3. Scan workspace for .al files FIRST (blocking std::fs walk;
            // isolate via block_in_place).  Scanning before the optional
            // package-download prompt means workspace/symbol can return
            // project-local objects immediately, even if the user has not yet
            // responded to the download dialog.
            let count = tokio::task::block_in_place(|| workspace.file_index.scan(&project.root));
            if count > 0 {
                info!(count, "Scanned workspace .al files");
            }

            *workspace.project.write().await = Some(project.clone());

            // F-018: load already-cached packages BEFORE flipping `ready` so
            // that warm-start queries (the common case) see complete symbol
            // coverage. The package-download prompt path still signals ready
            // before user interaction so a missing-dependencies dialog
            // doesn't strand the editor — but warm starts no longer race
            // package symbol load against the first hover/completion.
            if !project.packages.is_empty() {
                let cache = crate::symbols::cache::SymbolCache::default_location();
                let loaded = workspace
                    .symbols
                    .load_packages_cached(&project.packages, &cache);
                info!(
                    loaded = loaded.len(),
                    total_symbols = workspace.symbols.len(),
                    "Loaded symbol packages (pre-ready)"
                );
                workspace.symbols.load_runtime_enums();
                workspace.invalidate_insight_graph();
            }

            // Signal readiness. For warm starts the package symbol index is
            // already populated above; for cold starts (no cached packages
            // yet) we signal early to avoid blocking on the prompt and load
            // again after download completes.
            ready_flag.store(true, Ordering::Release);
            init_notify.notify_waiters();

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

                            // Now load the freshly-downloaded packages.
                            let cache = crate::symbols::cache::SymbolCache::default_location();
                            let loaded = workspace
                                .symbols
                                .load_packages_cached(&project.packages, &cache);
                            info!(
                                loaded = loaded.len(),
                                total_symbols = workspace.symbols.len(),
                                "Loaded symbol packages (post-download)"
                            );
                            workspace.symbols.load_runtime_enums();
                            workspace.invalidate_insight_graph();
                        }
                    }
                }
            }

            // Log which packages lack embedded source (outlines rendered automatically)
            log_source_availability(&project.packages);

            // Update project reference after any package downloads completed.
            *workspace.project.write().await = Some(project.clone());
        }
        Err(e) => {
            warn!(error = %e, "No AL project found (continuing without packages)");
            client
                .show_message(MessageType::INFO, format!("No AL project found: {e}"))
                .await;

            // Still try to scan for .al files in the workspace root
            let count = tokio::task::block_in_place(|| workspace.file_index.scan(&workspace_root));
            if count > 0 {
                info!(count, "Scanned workspace .al files");
            }

            // Signal readiness even without a project so request handlers
            // don't block forever waiting for initialization.
            ready_flag.store(true, Ordering::Release);
            init_notify.notify_waiters();
        }
    }

    client
        .log_message(MessageType::INFO, "AL workspace: ready")
        .await;

    // Offer recommended settings on first open of an AL project.
    // Runs after workspace init but before diagnostics — same timing window
    // as the symbol download prompt, which Zed reliably displays.
    let should_prompt =
        tokio::task::spawn_blocking(|| !settings_prompt_shown() && !zed_has_al_settings())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("settings prompt check panicked: {e}");
                false
            });
    if should_prompt {
        if let Ok(Some(action)) = client
            .show_message_request(
                MessageType::INFO,
                "Apply recommended AL development settings? (Updates Zed settings.json)"
                    .to_string(),
                Some(vec![
                    MessageActionItem {
                        title: "Yes".to_string(),
                        properties: Default::default(),
                    },
                    MessageActionItem {
                        title: "No".to_string(),
                        properties: Default::default(),
                    },
                ]),
            )
            .await
        {
            if action.title == "Yes" {
                match tokio::task::spawn_blocking(apply_recommended_settings).await {
                    Ok(Ok(())) => {
                        client
                            .show_message(
                                MessageType::INFO,
                                "Applied recommended AL settings. Reload Zed to activate.",
                            )
                            .await;
                    }
                    Ok(Err(e)) => {
                        client
                            .show_message(
                                MessageType::WARNING,
                                format!("Failed to apply settings: {e}"),
                            )
                            .await;
                    }
                    Err(e) => {
                        tracing::warn!("Settings task panicked: {e}");
                    }
                }
            }
            // Mark as shown only after user explicitly responded (Yes or No).
            // Run the sentinel write on the blocking pool so the async
            // executor thread is not stalled on disk I/O. Await the join
            // handle so a runtime shutdown mid-write surfaces in the log
            // instead of silently leaving the sentinel unwritten — the
            // user would otherwise see the prompt again on next launch.
            match tokio::task::spawn_blocking(mark_settings_prompt_shown).await {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!("settings prompt sentinel write task did not complete: {e}");
                }
            }
        }
        // If show_message_request returned Ok(None) or Err, do NOT mark —
        // the prompt was dismissed/lost, so retry next time.
    }

    // Project-scoped diagnostics: lint ALL .al files at startup.
    // Our native lint is fast enough to run on the entire project.
    {
        let config = workspace.config.read().await;
        if config.enable_native_lint
            && config.diagnostics_scope == crate::config::DiagnosticsScope::Project
        {
            // Snapshot lint config fields before iterating so we don't hold the
            // RwLock read guard across `client.publish_diagnostics().await`.
            let lint_overrides = config.native_lint_rules.clone();
            drop(config);
            let file_paths: Vec<std::path::PathBuf> = workspace
                .file_index
                .files
                .iter()
                .map(|entry| entry.key().clone())
                .collect();
            let file_count = file_paths.len();
            let is_lint_enabled = |code: &str| *lint_overrides.get(code).unwrap_or(&true);
            for (i, path) in file_paths.into_iter().enumerate() {
                if i > 0 && i % 10 == 0 {
                    tokio::task::yield_now().await;
                }
                if let Ok(uri) = url::Url::from_file_path(&path) {
                    // Use cached parse tree from file_index instead of re-parsing.
                    if let Some((text, tree)) = workspace.file_index.get_cached_parse(&path) {
                        let source = text.as_bytes();
                        let errors = crate::syntax::AlParser::errors_from_tree(&tree);
                        let mut lsp_diags = Vec::new();
                        for err in &errors {
                            lsp_diags.push(crate::server::diagnostics::syntax_error_to_diagnostic(
                                err, source,
                            ));
                        }
                        let lint_result = crate::syntax::lint(&tree, &text);
                        for lint in &lint_result {
                            if is_lint_enabled(&lint.code) {
                                lsp_diags.push(crate::server::diagnostics::lint_to_diagnostic(
                                    lint, source,
                                ));
                            }
                        }
                        if !lsp_diags.is_empty() {
                            client.publish_diagnostics(uri, lsp_diags, None).await;
                        }
                    }
                }
            }
            info!(
                file_count,
                "Published project-scoped diagnostics for all .al files"
            );
        }
    }
}

/// Load builtins and error codes from disk cache (fast path, no bridge needed).
///
/// Extracted from `AlServer::load_caches_from_disk` to be callable from the background init task.
async fn load_caches_from_disk(workspace: &Workspace, version: &str) {
    // SILENT: .unwrap_or_else recovers from RwLock poison by taking the inner value
    if workspace
        .builtins
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
    {
        if let Some(cached) = crate::semantic::cache::read_builtins(version) {
            info!(
                count = cached.len(),
                "Loaded built-in types from disk cache"
            );
            crate::semantic::set_builtins(workspace, cached, version);
        }
    }
    if workspace.error_codes.is_empty() {
        if let Some(cached) = crate::semantic::cache::read_error_codes(version) {
            info!(count = cached.len(), "Loaded error codes from disk cache");
            for ec in cached {
                workspace
                    .error_codes
                    .insert(ec.code.clone(), ec.message.clone());
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
            if crate::symbols::virtual_file::app_has_source(path) {
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
            // The LSP client returned an error for window/showMessageRequest.
            // Surface this to the user via window/showMessage so they know an
            // automatic decision is being made on their behalf, then fall
            // back to NuGet (the safer default for clients without the
            // request-style prompt).
            warn!(error = %e, "showMessageRequest failed, falling back to NuGet");
            client
                .show_message(
                    MessageType::WARNING,
                    "Could not show symbol-source prompt — falling back to NuGet. \
                     Set al.symbolSource explicitly to silence this warning.",
                )
                .await;
            Some(DownloadSource::NuGet)
        }
    }
}

/// Download symbols from a running BC instance defined in launch.json.
///
/// Uses the first available server config. Returns downloaded .app file paths.
async fn download_symbols_from_server(
    project: &crate::project::AlProject,
    deps: &[crate::project::AppDependency],
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
    let message_sink: crate::symbols::bc_server::MessageSink = std::sync::Arc::new(move |msg| {
        let c = lsp.clone();
        let m = msg.to_string();
        tokio::spawn(async move {
            c.show_message(tower_lsp::lsp_types::MessageType::INFO, m)
                .await;
        });
    });
    let auth = match config.authentication {
        crate::launch::AuthMethod::Windows => crate::symbols::bc_server::AuthMethod::Windows,
        crate::launch::AuthMethod::UserPassword => {
            crate::symbols::bc_server::AuthMethod::UserPassword
        }
        crate::launch::AuthMethod::AAD => crate::symbols::bc_server::AuthMethod::AAD,
    };
    let insecure_tls = config.accept_invalid_certs;
    let client = match crate::symbols::bc_server::BcServerClient::new(
        auth,
        config.tenant.clone(),
        message_sink,
        insecure_tls,
    ) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Failed to build HTTP client for BC server; skipping server download");
            return Vec::new();
        }
    };
    // crate::project::AppDependency is re-exported from crate::symbols — clone directly.
    let url_deps: Vec<(String, crate::symbols::nuget::AppDependency)> = deps
        .iter()
        .filter_map(|dep| config.dev_packages_url(dep).map(|url| (url, dep.clone())))
        .collect();
    let results = client.download_all(&url_deps, &dest).await;

    let mut downloaded = Vec::new();
    // `results` is parallel to `url_deps` (not `deps`): some dependencies are
    // filtered out above when they lack a dev_packages_url, so indexing into
    // `deps[i]` would be out of bounds. Pair each result with its originating
    // dependency from `url_deps` instead.
    for ((_, dep), result) in url_deps.iter().zip(results) {
        match result {
            Ok(path) => {
                info!(
                    package = %dep.name,
                    path = %path.display(),
                    "Downloaded from BC server"
                );
                downloaded.push(path);
            }
            Err(e) => {
                warn!(
                    package = %dep.name,
                    error = %e,
                    "Failed to download from BC server"
                );
            }
        }
    }

    downloaded
}

/// Convert the global NuGet feed list to the symbol-loader type.
///
/// Called from both the LSP workspace initializer and the daemon download dispatcher
/// so the mapping is defined exactly once.
pub(crate) fn map_nuget_feeds(
    feeds: &[crate::project::NuGetFeed],
) -> Vec<crate::symbols::nuget::NuGetFeed> {
    feeds
        .iter()
        .map(|f| crate::symbols::nuget::NuGetFeed {
            index_url: f.index_url.clone(),
        })
        .collect()
}

/// Download symbol packages from NuGet into the project's .alpackages directory.
///
/// Returns paths to successfully downloaded .app files.
async fn download_packages_nuget(
    deps: &[crate::project::AppDependency],
    dest: &Path,
) -> Vec<PathBuf> {
    info!(
        count = deps.len(),
        dest = %dest.display(),
        "Downloading symbol packages from NuGet"
    );

    // crate::project::AppDependency is re-exported from crate::symbols — pass directly.
    let feeds = map_nuget_feeds(&crate::project::nuget_feeds());

    let client = crate::symbols::nuget::NuGetClient::new(feeds);
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
            format!(
                "Downloading {} symbol packages from {}...",
                deps.len(),
                source_name
            ),
        )
        .await;

    let packages = match source {
        DownloadSource::Server => {
            download_symbols_from_server(&project, &deps, &server.client).await
        }
        DownloadSource::NuGet => download_packages_nuget(&deps, &project.packages_dir).await,
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
    let cache = crate::symbols::cache::SymbolCache::default_location();
    let loaded = server
        .workspace
        .symbols
        .load_packages_cached(&packages, &cache);
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
///
/// Package symbols (from .app NuGet packages) are intentionally excluded — like
/// VS Code, this feature returns only the user's project files. Package symbols
/// are accessible via completion, hover, and go-to-definition.
pub(crate) fn handle_workspace_symbol(
    server: &AlServer,
    query: &str,
) -> Option<Vec<SymbolInformation>> {
    // Use al-core search to avoid duplicating the name-filter loop (ISSUE-057 fix:
    // reads from cached object_info, not re-parsing files on every request).
    const MAX_LSP_SYMBOLS: usize = 10_000;
    let ws_results =
        crate::queries::search::workspace_search(&server.workspace, query, MAX_LSP_SYMBOLS);

    let mut results = Vec::new();

    // Top-level objects (table, page, codeunit, etc.)
    for r in ws_results {
        if let Some(file_text_entry) = server.workspace.file_index.files.get(&r.file_path) {
            if let Ok(file_uri) = Url::from_file_path(&r.file_path) {
                #[allow(deprecated)]
                results.push(SymbolInformation {
                    name: r.info.name.clone(),
                    kind: SymbolKind::OBJECT,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: file_uri,
                        range: crate::syntax_lsp::ts_range_to_lsp(
                            &r.info.range,
                            file_text_entry.value().as_bytes(),
                        ),
                    },
                    container_name: Some(r.info.kind.clone()),
                });
            }
        }
    }

    // Child symbols: procedures, triggers, events.
    let remaining = MAX_LSP_SYMBOLS.saturating_sub(results.len());
    if remaining > 0 {
        let child_results =
            crate::queries::search::workspace_search_children(&server.workspace, query, remaining);
        for r in child_results {
            if let Ok(file_uri) = Url::from_file_path(&r.file_path) {
                #[allow(deprecated)]
                results.push(SymbolInformation {
                    name: r.name,
                    kind: r.kind.into(),
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: file_uri,
                        range: r.range.into(),
                    },
                    container_name: if r.container_name.is_empty() {
                        None
                    } else {
                        Some(r.container_name)
                    },
                });
            }
        }
    }

    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

// ---------------------------------------------------------------------------
// Recommended settings helpers
// ---------------------------------------------------------------------------

/// Check whether the settings prompt has already been shown (persistent sentinel).
fn settings_prompt_shown() -> bool {
    sentinel_path().map(|p| p.exists()).unwrap_or(false)
}

/// Mark that the settings prompt has been shown.
fn mark_settings_prompt_shown() {
    if let Some(path) = sentinel_path() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    path = %parent.display(),
                    error = %e,
                    "failed to create settings-sentinel parent dir"
                );
                return;
            }
        }
        if let Err(e) = std::fs::write(&path, b"") {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to write settings-sentinel file"
            );
        }
    }
}

/// Path to the sentinel file that records the popup was shown.
fn sentinel_path() -> Option<PathBuf> {
    let data_dir = std::env::var("XDG_DATA_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        })?;
    Some(data_dir.join("al-lsp").join(".settings-prompt-shown"))
}

/// Check whether Zed's settings.json already has correct AL settings.
///
/// Returns true only if both `lsp.al-lsp` exists AND
/// `languages.AL.language_servers` contains "al-lsp". This prevents the
/// prompt from being skipped when settings are present but broken (e.g.
/// empty `language_servers: []`).
fn zed_has_al_settings() -> bool {
    let Some(path) = zed_settings_path() else {
        return false;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    if !content.contains("al-lsp") {
        return false;
    }
    let Ok(v) = strip_jsonc_comments_and_parse(&content) else {
        return false;
    };
    // Check lsp.al-lsp exists
    let has_lsp_section = v.get("lsp").and_then(|lsp| lsp.get("al-lsp")).is_some();
    // Check languages.AL.language_servers contains "al-lsp"
    let has_lang_server = v
        .get("languages")
        .and_then(|l| l.get("AL"))
        .and_then(|al| al.get("language_servers"))
        .and_then(|ls| ls.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some("al-lsp")))
        .unwrap_or(false);
    has_lsp_section && has_lang_server
}

/// Apply recommended AL settings to Zed's settings.json.
///
/// Reads the current settings, merges recommended AL-specific settings,
/// and writes back. Creates the file (and parent directories) if needed.
pub(crate) fn apply_recommended_settings() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings_path = zed_settings_path().ok_or("Cannot determine Zed settings path")?;

    // Read current settings (or empty object if file doesn't exist yet).
    let current: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        strip_jsonc_comments_and_parse(&content)?
    } else {
        serde_json::json!({})
    };

    let recommended = recommended_al_settings();
    let merged = deep_merge(&current, &recommended);

    // Write back with pretty formatting.
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = serde_json::to_string_pretty(&merged)?;
    std::fs::write(&settings_path, output)?;

    Ok(())
}

/// Get the path to Zed's settings.json.
fn zed_settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let config = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })?;
        Some(config.join("zed").join("settings.json"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join("Library/Application Support/Zed/settings.json"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Strip JSONC comments (`//` and `/* */`) and parse as JSON.
fn strip_jsonc_comments_and_parse(
    input: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape_next = false;

    while let Some(c) = chars.next() {
        if escape_next {
            result.push(c);
            escape_next = false;
            continue;
        }

        if in_string {
            result.push(c);
            if c == '\\' {
                escape_next = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                result.push(c);
            }
            '/' => {
                if chars.peek() == Some(&'/') {
                    // Line comment — skip to end of line (handle both LF and CRLF).
                    for c2 in chars.by_ref() {
                        if c2 == '\n' || c2 == '\r' {
                            result.push('\n');
                            // Consume trailing \n after \r (CRLF)
                            if c2 == '\r' && chars.peek() == Some(&'\n') {
                                chars.next();
                            }
                            break;
                        }
                    }
                } else if chars.peek() == Some(&'*') {
                    // Block comment — skip to `*/`.
                    chars.next(); // consume `*`
                    loop {
                        match chars.next() {
                            Some('*') if chars.peek() == Some(&'/') => {
                                chars.next(); // consume `/`
                                break;
                            }
                            Some('\n') => result.push('\n'), // preserve line numbers
                            None => break,
                            _ => {}
                        }
                    }
                } else {
                    result.push(c);
                }
            }
            _ => result.push(c),
        }
    }

    // Zed's settings.json is JSONC: it permits trailing commas (e.g. the comma
    // after the last property in an object). serde_json is strict and rejects
    // them ("trailing comma at line N"), so strip them before parsing — exactly
    // as Zed itself tolerates them.
    let result = strip_trailing_commas(&result);

    Ok(serde_json::from_str(&result)?)
}

/// Remove JSON trailing commas: a `,` followed only by whitespace before a
/// closing `}` or `]`. String contents are preserved (a comma inside a string
/// is never treated as structural). Operates on comment-free input.
fn strip_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape_next = false;
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i] as char;

        if escape_next {
            out.push(c);
            escape_next = false;
            i += 1;
            continue;
        }
        if in_string {
            out.push(c);
            match c {
                '\\' => escape_next = true,
                '"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            // Look ahead past whitespace; if the next non-space byte closes a
            // container, this comma is trailing — drop it.
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1; // skip the comma, keep the whitespace/closer
                continue;
            }
        }
        out.push(c);
        i += 1;
    }

    out
}

/// Deep-merge `overrides` into `base`.
///
/// Objects are merged recursively; all other value types are replaced by
/// the override value.  Existing user settings are never removed.
fn deep_merge(base: &serde_json::Value, overrides: &serde_json::Value) -> serde_json::Value {
    match (base, overrides) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(override_map)) => {
            let mut merged = base_map.clone();
            for (key, override_val) in override_map {
                let merged_val = match merged.get(key) {
                    Some(base_val) => deep_merge(base_val, override_val),
                    None => override_val.clone(),
                };
                merged.insert(key.clone(), merged_val);
            }
            serde_json::Value::Object(merged)
        }
        (_, override_val) => override_val.clone(),
    }
}

/// Recommended Zed settings for optimal AL development.
fn recommended_al_settings() -> serde_json::Value {
    serde_json::json!({
        "lsp": {
            "al-lsp": {
                "settings": {
                    "al.enableCodeAnalysis": true,
                    "al.backgroundCodeAnalysis": true,
                    "al.diagnosticsScope": "project",
                    "al.diagnosticsTrigger": "continuous",
                    "al.codeAnalyzers": ["CodeCop", "AppSourceCop", "UICop", "PerTenantCop"],
                    "al.enableNativeLint": true,
                    "al.enableCodeActions": true,
                    "al.inlayHints.parameterNames": true
                }
            }
        },
        "languages": {
          "AL": {
            "document_symbols": "on",
            "document_folding_ranges": "on",
            "completions": {
              "words": "fallback"
            },
            "debuggers": ["al"],
            "language_servers": ["al-lsp"],
            "semantic_tokens": "combined",
            "colorize_brackets": true,
            "inlay_hints": { "enabled": true },
          },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_line_comments() {
        let input = r#"{
  // This is a comment
  "key": "value"
}"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn strip_block_comments() {
        let input = r#"{
  /* block comment */
  "key": "value"
}"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn preserve_url_in_string() {
        let input = r#"{"url": "https://example.com"}"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["url"], "https://example.com");
    }

    #[test]
    fn preserve_comment_like_string() {
        let input = r#"{"note": "// not a comment"}"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["note"], "// not a comment");
    }

    #[test]
    fn strip_crlf_line_comments() {
        let input = "{\r\n  // comment\r\n  \"key\": \"value\"\r\n}";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn trailing_comma_in_object_is_tolerated() {
        // Zed permits this; serde_json alone rejects it ("trailing comma").
        let input = r#"{ "a": 1, "b": 2, }"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], 2);
    }

    #[test]
    fn trailing_comma_in_array_is_tolerated() {
        let input = r#"{ "xs": [1, 2, 3,] }"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["xs"], json!([1, 2, 3]));
    }

    #[test]
    fn trailing_comma_before_nested_close_like_real_zed_settings() {
        // Reproduces the reported failure: a trailing comma after the last key
        // of a nested object (the `theme` block in a real Zed settings.json).
        let input = "{\n  \"ui_font_size\": 16,\n  \"theme\": {\n    \"mode\": \"dark\",\n    \"dark\": \"Business Central Dark\",\n  },\n}";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["ui_font_size"], 16);
        assert_eq!(parsed["theme"]["dark"], "Business Central Dark");
    }

    #[test]
    fn comma_inside_string_is_not_stripped() {
        let input = r#"{ "list": "a, b, c", "n": 1 }"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["list"], "a, b, c");
        assert_eq!(parsed["n"], 1);
    }

    #[test]
    fn comments_and_trailing_commas_together() {
        let input = "{\n  // leading\n  \"a\": 1, // inline\n  \"b\": [1, 2,], /* block */\n}";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], json!([1, 2]));
    }

    #[test]
    fn deep_merge_preserves_base_keys() {
        let base = json!({"a": 1, "b": 2});
        let overrides = json!({"c": 3});
        let merged = deep_merge(&base, &overrides);
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["b"], 2);
        assert_eq!(merged["c"], 3);
    }

    #[test]
    fn deep_merge_recurses_objects() {
        let base = json!({"lsp": {"other": {"enabled": true}}});
        let overrides = json!({"lsp": {"al-lsp": {"settings": {}}}});
        let merged = deep_merge(&base, &overrides);
        assert_eq!(merged["lsp"]["other"]["enabled"], true);
        assert!(merged["lsp"]["al-lsp"].is_object());
    }

    #[test]
    fn deep_merge_replaces_non_objects() {
        let base = json!({"theme": "dark"});
        let overrides = json!({"theme": "light"});
        let merged = deep_merge(&base, &overrides);
        assert_eq!(merged["theme"], "light");
    }

    #[test]
    fn deep_merge_replaces_arrays() {
        let base = json!({"items": [1, 2]});
        let overrides = json!({"items": [3, 4, 5]});
        let merged = deep_merge(&base, &overrides);
        assert_eq!(merged["items"], json!([3, 4, 5]));
    }

    #[test]
    fn recommended_settings_has_al_lsp_section() {
        let settings = recommended_al_settings();
        assert!(settings["lsp"]["al-lsp"]["settings"].is_object());
        assert!(settings["languages"]["AL"].is_object());
    }
}
