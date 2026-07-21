//! Project and workspace management.
//!
//! Handles workspace initialization: discovering the project, loading packages,
//! scanning workspace .al files, and initializing the semantic bridge lazily.
//! Auto-downloads missing BC symbol packages via BC server (launch.json) or NuGet.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use al_workspace::Workspace;
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

impl DownloadSource {
    /// User-facing name for status messages.
    fn display_name(self) -> &'static str {
        match self {
            DownloadSource::Server => "BC server",
            DownloadSource::NuGet => "NuGet",
        }
    }
}

/// Initialize the workspace: discover toolchain, load packages, scan files.
///
/// Called from the background task spawned by the `initialized` notification
/// handler. Failures are logged but do not prevent the server from operating
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
    client
        .log_message(MessageType::INFO, "AL workspace: initializing...")
        .await;

    // find_toolchain() does sync filesystem traversal (PATH walk, ALTool
    // probe) which can take tens of ms — must run on a blocking thread so
    // we don't stall the tokio runtime during init.
    let toolchain_result = tokio::task::spawn_blocking(crate::toolchain::find_toolchain)
        .await
        .unwrap_or_else(|join_err| {
            Err(al_project::errors::DiscoveryError::Io(
                std::io::Error::other(format!("find_toolchain task panicked: {join_err}")),
            ))
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

    let workspace_root = root_uri
        .as_ref()
        .and_then(|u| u.to_file_path().ok()) // Non-file URIs legitimately have no path.
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match al_project::project::find_project(&workspace_root) {
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

            // load already-cached packages BEFORE flipping `ready` so
            // that warm-start queries (the common case) see complete symbol
            // coverage. The package-download prompt path still signals ready
            // before user interaction so a missing-dependencies dialog
            // doesn't strand the editor — but warm starts no longer race
            // package symbol load against the first hover/completion.
            if !project.packages.is_empty() {
                let cache = al_symbols::cache::SymbolCache::default_location();
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
                                download_packages_nuget(&workspace, &deps, &project.packages_dir)
                                    .await
                            }
                        };
                        if !downloaded.is_empty() {
                            project.packages = downloaded;

                            let cache = al_symbols::cache::SymbolCache::default_location();
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

            log_source_availability(&project.packages);

            *workspace.project.write().await = Some(project.clone());
        }
        Err(e) => {
            warn!(error = %e, "No AL project found (continuing without packages)");
            client
                .show_message(MessageType::INFO, format!("No AL project found: {e}"))
                .await;

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
            && config.diagnostics_scope == al_project::config::DiagnosticsScope::Project
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
                        let errors = al_syntax::AlParser::errors_from_tree(&tree);
                        let mut lsp_diags = Vec::new();
                        for err in &errors {
                            lsp_diags.push(crate::server::diagnostics::syntax_error_to_diagnostic(
                                err, source,
                            ));
                        }
                        let lint_result = al_syntax::lint(&tree, &text);
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
    // Recover from RwLock poison by taking the inner value.
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
            if al_symbols::virtual_file::app_has_source(path) {
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
    project: &al_project::project::AlProject,
    deps: &[al_project::project::AppDependency],
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
    let message_sink: al_symbols::bc_server::MessageSink = std::sync::Arc::new(move |msg| {
        let c = lsp.clone();
        let m = msg.to_string();
        tokio::spawn(async move {
            c.show_message(tower_lsp::lsp_types::MessageType::INFO, m)
                .await;
        });
    });
    let auth = match config.authentication {
        al_bc::launch::AuthMethod::Windows => al_symbols::bc_server::AuthMethod::Windows,
        al_bc::launch::AuthMethod::UserPassword => al_symbols::bc_server::AuthMethod::UserPassword,
        al_bc::launch::AuthMethod::AAD => al_symbols::bc_server::AuthMethod::AAD,
    };
    let insecure_tls = config.accept_invalid_certs;
    let client = match al_symbols::bc_server::BcServerClient::new(
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
    // al_project::project::AppDependency is re-exported from al_symbols — clone directly.
    let url_deps: Vec<(String, al_symbols::nuget::AppDependency)> = deps
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
    feeds: &[al_project::project::NuGetFeed],
) -> Vec<al_symbols::nuget::NuGetFeed> {
    feeds
        .iter()
        .map(|f| al_symbols::nuget::NuGetFeed {
            index_url: f.index_url.clone(),
        })
        .collect()
}

/// Resolve the EFFECTIVE NuGet feed list from user config + built-in defaults
/// Custom feeds (`al.nugetFeeds`) are tried
/// first; the public Microsoft feeds (MSSymbols/AppSourceSymbols/MSApps)
/// are appended unless `al.useOnlyCustomFeeds` is set.
pub(crate) fn effective_nuget_feeds(
    config: &al_project::config::AlConfig,
) -> Vec<al_project::project::NuGetFeed> {
    let mut feeds: Vec<al_project::project::NuGetFeed> = config
        .nuget_feeds
        .iter()
        .map(|f| al_project::project::NuGetFeed {
            name: f.name.clone(),
            index_url: f.url.clone(),
        })
        .collect();
    if !config.use_only_custom_feeds {
        feeds.extend(al_project::project::nuget_feeds());
    }
    feeds
}

/// Download symbol packages from NuGet into the project's .alpackages directory.
///
/// Returns paths to successfully downloaded .app files.
async fn download_packages_nuget(
    workspace: &al_workspace::Workspace,
    deps: &[al_project::project::AppDependency],
    dest: &Path,
) -> Vec<PathBuf> {
    info!(
        count = deps.len(),
        dest = %dest.display(),
        "Downloading symbol packages from NuGet"
    );

    // al_project::project::AppDependency is re-exported from al_symbols — pass directly.
    // honor al.nugetFeeds / al.useOnlyCustomFeeds / al.symbolsCountryRegion.
    let (feeds, country) = {
        let cfg = workspace.config.read().await;
        (
            map_nuget_feeds(&effective_nuget_feeds(&cfg)),
            cfg.symbols_country_region.clone(),
        )
    };

    let client = al_symbols::nuget::NuGetClient::new(feeds).with_country(country);
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

    let source_name = source.display_name();

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
        DownloadSource::NuGet => {
            download_packages_nuget(&server.workspace, &deps, &project.packages_dir).await
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
    let cache = al_symbols::cache::SymbolCache::default_location();
    let loaded = server
        .workspace
        .symbols
        .load_packages_cached(&packages, &cache);
    server.workspace.symbols.load_runtime_enums();
    // Package changes invalidate the insight graph.
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
    // Use the shared search implementation, which reads cached object data
    // instead of reparsing files on every request.
    const MAX_LSP_SYMBOLS: usize = 10_000;
    let ws_results =
        al_analysis::queries::search::workspace_search(&server.workspace, query, MAX_LSP_SYMBOLS);

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
        let child_results = al_analysis::queries::search::workspace_search_children(
            &server.workspace,
            query,
            remaining,
        );
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

/// Check whether the settings prompt has already been shown (persistent sentinel).
fn settings_prompt_shown() -> bool {
    sentinel_path().map(|p| p.exists()).unwrap_or(false)
}

/// Create the parent directory of `path` (if any), so a subsequent file write
/// can succeed. Returns the underlying `create_dir_all` result so each caller
/// keeps its own error-handling policy (warn-and-skip vs. `?`-propagation).
fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
    } else {
        Ok(())
    }
}

fn mark_settings_prompt_shown() {
    if let Some(path) = sentinel_path() {
        if let Err(e) = ensure_parent_dir(&path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to create settings-sentinel parent dir"
            );
            return;
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
    let has_lsp_section = v.get("lsp").and_then(|lsp| lsp.get("al-lsp")).is_some();
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

    let current: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        strip_jsonc_comments_and_parse(&content)?
    } else {
        serde_json::json!({})
    };

    let recommended = recommended_al_settings();
    let merged = deep_merge(&current, &recommended);

    ensure_parent_dir(&settings_path)?;
    let output = serde_json::to_string_pretty(&merged)?;
    std::fs::write(&settings_path, output)?;

    Ok(())
}

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
                            if c2 == '\r' && chars.peek() == Some(&'\n') {
                                // Consume trailing \n after \r (CRLF)
                                chars.next();
                            }
                            break;
                        }
                    }
                } else if chars.peek() == Some(&'*') {
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
    // as Zed itself tolerates them. Delegate to the canonical byte-safe
    // implementation in `dap::json_util` so multi-byte UTF-8 (e.g. emoji in a
    // theme name or comment) is never corrupted by `byte as char` casting.
    let result = al_dap::dap::json_util::strip_trailing_commas(&result);

    Ok(serde_json::from_str(&result)?)
}

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
        let input = "{\n  \"ui_font_size\": 16,\n  \"theme\": {\n    \"mode\": \"dark\",\n    \"dark\": \"Business Central Dark\",\n  },\n}";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["ui_font_size"], 16);
        assert_eq!(parsed["theme"]["dark"], "Business Central Dark");
    }

    #[test]
    fn multibyte_utf8_survives_trailing_comma_strip() {
        let input = r#"{ "name": "Test 😀", "accent": "café", "value": 1, }"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["name"], "Test 😀");
        assert_eq!(parsed["accent"], "café");
        assert_eq!(parsed["value"], 1);
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

    #[test]
    fn download_source_display_names() {
        assert_eq!(DownloadSource::Server.display_name(), "BC server");
        assert_eq!(DownloadSource::NuGet.display_name(), "NuGet");
    }

    #[test]
    fn effective_feeds_honor_custom_and_only_flags() {
        let mut cfg = al_project::config::AlConfig::default();
        let feeds = effective_nuget_feeds(&cfg);
        assert_eq!(feeds.len(), 3, "the three public Microsoft feeds");

        cfg.nuget_feeds = vec![al_project::config::NuGetFeedConfig {
            name: "corp".into(),
            url: "https://nuget.corp.example/v3/index.json".into(),
        }];
        let feeds = effective_nuget_feeds(&cfg);
        assert_eq!(feeds.len(), 4);
        assert_eq!(
            feeds[0].index_url,
            "https://nuget.corp.example/v3/index.json"
        );

        cfg.use_only_custom_feeds = true;
        let feeds = effective_nuget_feeds(&cfg);
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].name, "corp");
    }

    #[test]
    fn map_nuget_feeds_preserves_index_urls_in_order() {
        let feeds = vec![
            al_project::project::NuGetFeed {
                name: "first".to_string(),
                index_url: "https://a.example/index.json".to_string(),
            },
            al_project::project::NuGetFeed {
                name: "second".to_string(),
                index_url: "https://b.example/index.json".to_string(),
            },
        ];
        let mapped = map_nuget_feeds(&feeds);
        assert_eq!(mapped.len(), 2);
        // The `name` field is dropped; only `index_url` carries over, in order.
        assert_eq!(mapped[0].index_url, "https://a.example/index.json");
        assert_eq!(mapped[1].index_url, "https://b.example/index.json");
    }

    #[test]
    fn map_nuget_feeds_empty_yields_empty() {
        let mapped = map_nuget_feeds(&[]);
        assert!(mapped.is_empty());
    }

    #[test]
    fn ensure_parent_dir_creates_missing_parents() {
        let tmp = std::env::temp_dir().join(format!("al-ws-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let target = tmp.join("nested").join("deep").join("file.txt");
        ensure_parent_dir(&target).unwrap();
        assert!(target.parent().unwrap().is_dir());
        std::fs::write(&target, b"ok").unwrap();
        assert!(target.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_parent_dir_handles_path_without_parent() {
        // A bare relative file name has parent == "" — create_dir_all("") is Ok.
        let p = Path::new("just_a_name");
        assert!(ensure_parent_dir(p).is_ok());
    }

    #[test]
    fn log_source_availability_handles_empty_and_missing_files() {
        // Empty package list: no-op, no panic.
        log_source_availability(&[]);
        // Non-existent .app path: app_has_source returns false, the stem is
        // collected, and the function logs without panicking.
        log_source_availability(&[PathBuf::from("/nonexistent/Some.App.app")]);
        // Path with no file stem must fall back to "unknown" without panic.
        log_source_availability(&[PathBuf::from("/")]);
    }

    /// RAII guard that snapshots and restores process env vars used by the
    /// path helpers, so these serial tests don't leak state into one another.
    struct EnvGuard {
        keys: Vec<(&'static str, Option<String>)>,
    }
    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let snapshot = keys.iter().map(|&k| (k, std::env::var(k).ok())).collect();
            Self { keys: snapshot }
        }
        fn set(&self, key: &str, val: &Path) {
            std::env::set_var(key, val);
        }
        fn remove(&self, key: &str) {
            std::env::remove_var(key);
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.keys {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn sentinel_path_uses_xdg_data_home() {
        let guard = EnvGuard::new(&["XDG_DATA_HOME", "HOME"]);
        let base = std::env::temp_dir().join("al-xdg-data");
        guard.set("XDG_DATA_HOME", &base);
        let p = sentinel_path().expect("path should resolve from XDG_DATA_HOME");
        assert_eq!(p, base.join("al-lsp").join(".settings-prompt-shown"));
    }

    #[test]
    #[serial_test::serial]
    fn sentinel_path_falls_back_to_home() {
        let guard = EnvGuard::new(&["XDG_DATA_HOME", "HOME"]);
        guard.remove("XDG_DATA_HOME");
        let home = std::env::temp_dir().join("al-home");
        guard.set("HOME", &home);
        let p = sentinel_path().expect("path should resolve from HOME fallback");
        assert_eq!(
            p,
            home.join(".local/share")
                .join("al-lsp")
                .join(".settings-prompt-shown")
        );
    }

    #[test]
    #[serial_test::serial]
    fn sentinel_path_none_without_env() {
        let guard = EnvGuard::new(&["XDG_DATA_HOME", "HOME"]);
        guard.remove("XDG_DATA_HOME");
        guard.remove("HOME");
        assert!(sentinel_path().is_none());
    }

    #[test]
    #[serial_test::serial]
    fn settings_prompt_round_trip_via_sentinel() {
        let guard = EnvGuard::new(&["XDG_DATA_HOME", "HOME"]);
        let base = std::env::temp_dir().join(format!("al-sentinel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        guard.set("XDG_DATA_HOME", &base);
        assert!(!settings_prompt_shown());
        mark_settings_prompt_shown();
        assert!(settings_prompt_shown());
        assert!(sentinel_path().unwrap().exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(target_os = "linux")]
    fn write_zed_settings(guard: &EnvGuard, contents: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "al-zedcfg-{}-{}",
            std::process::id(),
            // unique-ish per call so cases don't collide
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&base);
        guard.set("XDG_CONFIG_HOME", &base);
        let path = zed_settings_path().unwrap();
        ensure_parent_dir(&path).unwrap();
        std::fs::write(&path, contents).unwrap();
        base
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn zed_has_al_settings_true_when_both_sections_present() {
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = write_zed_settings(
            &guard,
            r#"{
                "lsp": { "al-lsp": { "settings": {} } },
                "languages": { "AL": { "language_servers": ["al-lsp"] } }
            }"#,
        );
        assert!(zed_has_al_settings());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn zed_has_al_settings_false_when_language_server_missing() {
        // Has the lsp.al-lsp block, but language_servers is empty: must be false
        // (the documented guard against "present but broken" settings).
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = write_zed_settings(
            &guard,
            r#"{
                "lsp": { "al-lsp": { "settings": {} } },
                "languages": { "AL": { "language_servers": [] } }
            }"#,
        );
        assert!(!zed_has_al_settings());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn zed_has_al_settings_false_when_no_lsp_section() {
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        // Mentions "al-lsp" only in language_servers, so the early text check
        // passes, but lsp.al-lsp is absent → false.
        let base = write_zed_settings(
            &guard,
            r#"{ "languages": { "AL": { "language_servers": ["al-lsp"] } } }"#,
        );
        assert!(!zed_has_al_settings());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn zed_has_al_settings_false_when_file_missing() {
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = std::env::temp_dir().join(format!("al-zedcfg-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        guard.set("XDG_CONFIG_HOME", &base);
        assert!(!zed_has_al_settings());
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn apply_recommended_settings_creates_and_merges() {
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = std::env::temp_dir().join(format!("al-apply-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        guard.set("XDG_CONFIG_HOME", &base);
        let path = zed_settings_path().unwrap();
        // Pre-existing user setting that must survive the merge.
        ensure_parent_dir(&path).unwrap();
        std::fs::write(&path, r#"{ "ui_font_size": 18, "theme": "Custom" }"#).unwrap();

        apply_recommended_settings().unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(v["languages"]["AL"]["language_servers"][0], "al-lsp");
        assert!(v["lsp"]["al-lsp"]["settings"].is_object());
        assert_eq!(v["ui_font_size"], 18);
        assert_eq!(v["theme"], "Custom");
        // The just-written file is itself recognized as having AL settings.
        assert!(zed_has_al_settings());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn block_comment_preserves_following_keys() {
        let input = "{\n  \"a\": 1,\n  /* this\n     spans\n     lines */\n  \"b\": 2\n}";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], 2);
    }

    #[test]
    fn unterminated_block_comment_does_not_panic_and_strips_to_eof() {
        // Hits the `None => break` arm of the block-comment loop: the comment
        // runs to EOF without a closing `*/`. The content up to the comment is
        // still valid JSON, so the value before it must parse.
        let input = "{ \"a\": 1 } /* dangling comment never closed";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn escaped_quote_inside_string_is_not_treated_as_string_end() {
        // Hits the `escape_next` path: a backslash-escaped quote must not close
        // the JSON string, so the `//` that follows stays inside the string and
        // is NOT stripped as a comment.
        let input = r#"{ "path": "C:\\dir\"// still in string", "n": 1 }"#;
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["path"], r#"C:\dir"// still in string"#);
        assert_eq!(parsed["n"], 1);
    }

    #[test]
    fn lone_slash_not_a_comment_is_an_error_not_silently_dropped() {
        // A bare `/` that is neither `//` nor `/*` must be preserved (pushed),
        // which then makes the JSON invalid — proving it was NOT swallowed.
        let input = r#"{ "a": 1 / 2 }"#;
        assert!(strip_jsonc_comments_and_parse(input).is_err());
    }

    #[test]
    fn malformed_json_after_stripping_returns_err() {
        // Comment stripping succeeds but the residue is not valid JSON.
        let input = "{ // comment\n  not valid json here\n}";
        assert!(strip_jsonc_comments_and_parse(input).is_err());
    }

    #[test]
    fn deep_merge_override_object_replaces_base_scalar() {
        // base has a scalar where the override has an object: the object wins
        // (the `(_, override_val)` arm), not a merge attempt.
        let base = json!({"al-lsp": "scalar"});
        let overrides = json!({"al-lsp": {"settings": {"x": 1}}});
        let merged = deep_merge(&base, &overrides);
        assert!(merged["al-lsp"].is_object());
        assert_eq!(merged["al-lsp"]["settings"]["x"], 1);
    }

    #[test]
    fn deep_merge_override_scalar_replaces_base_object() {
        // Inverse: base has an object, override has a scalar — scalar replaces.
        let base = json!({"al-lsp": {"settings": {"x": 1}}});
        let overrides = json!({"al-lsp": "scalar"});
        let merged = deep_merge(&base, &overrides);
        assert_eq!(merged["al-lsp"], "scalar");
    }

    #[test]
    fn deep_merge_recurses_three_levels_deep() {
        let base = json!({"a": {"b": {"keep": 1}}});
        let overrides = json!({"a": {"b": {"add": 2}}});
        let merged = deep_merge(&base, &overrides);
        assert_eq!(merged["a"]["b"]["keep"], 1);
        assert_eq!(merged["a"]["b"]["add"], 2);
    }

    #[test]
    fn recommended_settings_registers_al_lsp_language_server() {
        let s = recommended_al_settings();
        let servers = s["languages"]["AL"]["language_servers"]
            .as_array()
            .expect("language_servers must be an array");
        assert!(
            servers.iter().any(|v| v.as_str() == Some("al-lsp")),
            "recommended settings must register the al-lsp language server"
        );
        // The al debugger and code-analysis flag are also part of the contract.
        assert_eq!(s["languages"]["AL"]["debuggers"][0], "al");
        assert_eq!(
            s["lsp"]["al-lsp"]["settings"]["al.enableCodeAnalysis"],
            true
        );
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn zed_has_al_settings_false_when_text_lacks_al_lsp_marker() {
        // The fast `!content.contains("al-lsp")` early-out: a settings file with
        // no mention of al-lsp at all returns false without parsing.
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = write_zed_settings(&guard, r#"{ "ui_font_size": 14 }"#);
        assert!(!zed_has_al_settings());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn zed_has_al_settings_false_when_jsonc_is_malformed() {
        // Contains "al-lsp" (passes the text gate) but the JSON is broken, so
        // strip_jsonc_comments_and_parse errors → the function returns false.
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = write_zed_settings(&guard, r#"{ "lsp": { "al-lsp": broken }"#);
        assert!(!zed_has_al_settings());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn zed_has_al_settings_tolerates_comments_around_real_settings() {
        // JSONC comments must be stripped before the structural check, so a
        // commented but valid AL settings file is still recognized as valid.
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = write_zed_settings(
            &guard,
            "{\n  // language server config\n  \"lsp\": { \"al-lsp\": { \"settings\": {} } },\n  \"languages\": { \"AL\": { \"language_servers\": [\"al-lsp\"] } },\n}",
        );
        assert!(zed_has_al_settings());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn apply_recommended_settings_creates_file_when_absent() {
        // Exercises the `else { json!({}) }` branch: no settings file exists yet.
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = std::env::temp_dir().join(format!("al-apply-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        guard.set("XDG_CONFIG_HOME", &base);
        let path = zed_settings_path().unwrap();
        assert!(!path.exists(), "precondition: file must not exist");

        apply_recommended_settings().unwrap();

        assert!(path.exists(), "apply must create the settings file");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["languages"]["AL"]["language_servers"][0], "al-lsp");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(target_os = "linux")]
    fn apply_recommended_settings_parses_existing_jsonc_with_comments() {
        // Exercises the JSONC read+strip branch of apply: the existing file has
        // comments and a trailing comma (legal in Zed) that must survive the
        // read-merge-write round-trip.
        let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        let base = std::env::temp_dir().join(format!("al-apply-jsonc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        guard.set("XDG_CONFIG_HOME", &base);
        let path = zed_settings_path().unwrap();
        ensure_parent_dir(&path).unwrap();
        std::fs::write(
            &path,
            "{\n  // my editor prefs\n  \"ui_font_size\": 20,\n  \"theme\": \"Solarized\",\n}",
        )
        .unwrap();

        apply_recommended_settings().unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["ui_font_size"], 20);
        assert_eq!(v["theme"], "Solarized");
        assert!(v["lsp"]["al-lsp"]["settings"].is_object());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Build an `AlServer` (with a real tower-lsp `Client`) for in-process tests.
    /// `LspService::new` wires a live client without spawning the LSP transport.
    fn test_server() -> tower_lsp::LspService<AlServer> {
        let (service, _socket) = tower_lsp::LspService::new(AlServer::new);
        service
    }

    #[tokio::test]
    async fn handle_workspace_symbol_empty_workspace_returns_none() {
        let service = test_server();
        let server = service.inner();
        // No files indexed → no symbols → None (not an empty Vec).
        assert!(handle_workspace_symbol(server, "anything").is_none());
    }

    #[tokio::test]
    async fn handle_workspace_symbol_returns_top_level_objects() {
        let service = test_server();
        let server = service.inner();
        server.workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/CustomerCard.al"),
            r#"page 50100 "Customer Card" { }"#.to_string(),
        );
        server.workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/VendorCard.al"),
            r#"page 50101 "Vendor Card" { }"#.to_string(),
        );

        let results = handle_workspace_symbol(server, "Customer")
            .expect("a matching object must yield Some results");
        assert_eq!(results.len(), 1, "only the Customer object matches");
        let sym = &results[0];
        assert_eq!(sym.name, "Customer Card");
        assert_eq!(sym.kind, SymbolKind::OBJECT);
        assert_eq!(sym.container_name.as_deref(), Some("page"));
        assert!(sym.location.uri.as_str().ends_with("CustomerCard.al"));
    }

    #[tokio::test]
    async fn handle_workspace_symbol_includes_child_procedures() {
        let service = test_server();
        let server = service.inner();
        server.workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/MathUtil.al"),
            "codeunit 50100 \"Math Util\"\n{\n    procedure AddNumbers(a: Integer): Integer\n    begin\n    end;\n}\n".to_string(),
        );

        // Querying the procedure name must surface the child symbol, not just
        // the top-level object.
        let results = handle_workspace_symbol(server, "AddNumbers")
            .expect("procedure query must return Some");
        assert!(
            results.iter().any(|s| s.name == "AddNumbers"),
            "child procedure AddNumbers must appear in the results: {:?}",
            results.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn handle_workspace_symbol_no_match_returns_none() {
        let service = test_server();
        let server = service.inner();
        server.workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/CustomerCard.al"),
            r#"page 50100 "Customer Card" { }"#.to_string(),
        );
        assert!(handle_workspace_symbol(server, "ZZZ_no_such_symbol").is_none());
    }

    #[tokio::test]
    async fn handle_workspace_symbol_child_has_method_kind_and_container() {
        // The child-results branch must (a) map the transport-agnostic symbol
        // kind to the LSP kind (a procedure → FUNCTION, NOT the OBJECT kind used
        // for top-level objects) and (b) carry the parent object name as a
        // non-empty container_name → Some(...).
        let service = test_server();
        let server = service.inner();
        server.workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/MathUtil.al"),
            "codeunit 50100 \"Math Util\"\n{\n    procedure AddNumbers(a: Integer): Integer\n    begin\n    end;\n}\n".to_string(),
        );

        let results = handle_workspace_symbol(server, "AddNumbers")
            .expect("procedure query must return Some");
        let child = results
            .iter()
            .find(|s| s.name == "AddNumbers")
            .expect("AddNumbers child must be present");
        assert_eq!(child.kind, SymbolKind::FUNCTION);
        assert_ne!(child.kind, SymbolKind::OBJECT);
        assert_eq!(child.container_name.as_deref(), Some("Math Util"));
    }

    #[tokio::test]
    async fn handle_workspace_symbol_empty_query_returns_objects_and_children() {
        // An empty query matches everything: the result set must contain BOTH
        // the top-level object (OBJECT kind) and its child procedure (METHOD).
        let service = test_server();
        let server = service.inner();
        server.workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/MathUtil.al"),
            "codeunit 50100 \"Math Util\"\n{\n    procedure AddNumbers(a: Integer): Integer\n    begin\n    end;\n}\n".to_string(),
        );

        let results =
            handle_workspace_symbol(server, "").expect("empty query must return all symbols");
        assert!(
            results
                .iter()
                .any(|s| s.name == "Math Util" && s.kind == SymbolKind::OBJECT),
            "top-level object must be present for empty query"
        );
        assert!(
            results
                .iter()
                .any(|s| s.name == "AddNumbers" && s.kind == SymbolKind::FUNCTION),
            "child procedure must be present for empty query"
        );
    }

    #[test]
    fn block_comment_terminated_exactly_at_eof_after_star() {
        // Hits the block-comment loop arm where `*` is seen but the stream ends
        // before the closing `/` (peek() == None). The earlier object stays
        // parseable; the dangling `/*...*` is stripped without panic.
        let input = "{ \"a\": 1 } /* trailing star then eof *";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn block_comment_with_crlf_preserves_line_count_and_following_keys() {
        // A block comment spanning CRLF lines: the loop's `Some('\n')` arm pushes
        // a newline to preserve line numbers. The keys around it survive.
        let input =
            "{\r\n  \"a\": 1,\r\n  /* multi\r\n     line\r\n     comment */\r\n  \"b\": 2\r\n}";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], 2);
    }

    #[test]
    fn line_comment_at_eof_without_newline_is_stripped() {
        // A `//` line comment that runs to EOF with no terminating newline must
        // be fully consumed, leaving the preceding object parseable.
        let input = "{ \"a\": 1 } // trailing line comment, no newline";
        let parsed = strip_jsonc_comments_and_parse(input).unwrap();
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn deep_merge_inserts_override_key_absent_in_base() {
        // The `None => override_val.clone()` arm: a nested object key present in
        // the override but absent in the base is inserted wholesale.
        let base = json!({"lsp": {"existing": 1}});
        let overrides = json!({"lsp": {"al-lsp": {"settings": {"x": 5}}}});
        let merged = deep_merge(&base, &overrides);
        assert_eq!(merged["lsp"]["al-lsp"]["settings"]["x"], 5);
        assert_eq!(merged["lsp"]["existing"], 1);
    }

    #[test]
    fn deep_merge_into_empty_base_yields_overrides() {
        let merged = deep_merge(&json!({}), &recommended_al_settings());
        assert!(merged["lsp"]["al-lsp"]["settings"].is_object());
        assert_eq!(merged["languages"]["AL"]["language_servers"][0], "al-lsp");
    }
}
