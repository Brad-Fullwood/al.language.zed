//! Project and workspace management.
//!
//! Handles workspace initialization: discovering the project, loading packages,
//! scanning workspace .al files, and initializing the semantic bridge lazily.
//! Auto-downloads missing BC symbol packages via BC server (launch.json) or NuGet.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use al_workspace::Workspace;
use tower_lsp::lsp_types::*;
use tower_lsp::Client;
use tracing::{info, warn};

use super::lsp::LspSessionState;
use super::{diagnostics, AlServer, WorkspaceInitState};

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

#[derive(Debug, Default)]
struct DownloadBatch {
    paths: Vec<PathBuf>,
    failures: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct WorkspaceInitError(String);

fn publish_ready(state: &Option<tokio::sync::watch::Sender<WorkspaceInitState>>) {
    if let Some(state) = state {
        state.send_replace(WorkspaceInitState::Ready);
    }
}

fn session_cancelled(session: &Option<LspSessionState>) -> bool {
    session.as_ref().is_some_and(LspSessionState::is_cancelled)
}

/// Initialize the workspace: discover toolchain, load packages, scan files.
///
/// Called from the background task spawned by the `initialized` notification
/// handler. A project/source/package failure before a complete generation is
/// returned to the caller and retained as a failed server state.
///
/// `init_state` is set to Ready as soon as the file and warm package scans are
/// complete so request handlers do not wait for an interactive cold-download
/// prompt. Reindex passes `None` because an existing generation remains usable.
pub(crate) async fn initialize_workspace(
    workspace: Arc<Workspace>,
    client: Client,
    root_uri: Option<Url>,
    init_state: Option<tokio::sync::watch::Sender<WorkspaceInitState>>,
    diagnostic_state: Option<diagnostics::DiagnosticPublicationState>,
) -> Result<(), WorkspaceInitError> {
    let session = diagnostic_state.as_ref().map(|state| state.session.clone());
    if session_cancelled(&session) {
        return Ok(());
    }
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
    if session_cancelled(&session) {
        return Ok(());
    }
    match toolchain_result {
        Ok(tc) => {
            info!(version = %tc.version, "Found AL toolchain");
            *workspace.toolchain.write().await = Some(tc.clone());

            // Load builtins + error codes from disk cache (no bridge needed, <1ms)
            load_caches_from_disk(&workspace, &tc.version).await?;
        }
        Err(e) => {
            warn!(error = %e, "AL toolchain not found (continuing without)");
            client
                .show_message(MessageType::WARNING, format!("AL toolchain not found: {e}"))
                .await;
        }
    }

    let workspace_root = match root_uri.as_ref() {
        Some(uri) => match uri.to_file_path() {
            Ok(path) => path,
            Err(()) => {
                warn!(%uri, "non-file workspace URI; starting in open-document syntax mode");
                client
                    .show_message(
                        MessageType::INFO,
                        format!(
                            "Workspace URI '{uri}' is not a local file URI; project indexing is unavailable"
                        ),
                    )
                    .await;
                publish_ready(&init_state);
                return Ok(());
            }
        },
        None => std::env::current_dir().map_err(|error| {
            WorkspaceInitError(format!(
                "workspace root was not supplied and the current directory is unavailable: {error}"
            ))
        })?,
    };

    match al_project::project::find_project(&workspace_root) {
        Ok(mut project) => {
            let symbol_config = workspace.config.read().await.clone();
            if let Err(error) = project.apply_symbol_settings(&symbol_config) {
                tracing::error!(%error, "configured symbol package folders could not be scanned");
                let message = format!("AL workspace initialization failed: {error}");
                client
                    .show_message(MessageType::ERROR, message.clone())
                    .await;
                return Err(WorkspaceInitError(message));
            }
            info!(
                name = %project.app_json.name,
                packages = project.packages.len(),
                "Found AL project"
            );

            // Stage source and package indexes independently from the active
            // workspace. A failed reindex must not publish new source files
            // alongside the previous package/project generation.
            let source_root = project.root.clone();
            let (staged_files, count) = match tokio::task::spawn_blocking(move || {
                let staged = al_source::file_index::FileIndex::new();
                staged.scan(&source_root).map(|count| (staged, count))
            })
            .await
            {
                Ok(Ok(staged)) => staged,
                Ok(Err(error)) => {
                    tracing::error!(%error, "AL workspace source scan failed");
                    let message = format!(
                        "AL workspace initialization failed; no partial source index was published: {error}"
                    );
                    client
                        .show_message(MessageType::ERROR, message.clone())
                        .await;
                    return Err(WorkspaceInitError(message));
                }
                Err(error) => {
                    let message = format!("AL workspace source-index worker failed: {error}");
                    tracing::error!(%error, "AL workspace source-index worker failed");
                    client
                        .show_message(MessageType::ERROR, message.clone())
                        .await;
                    return Err(WorkspaceInitError(message));
                }
            };
            if session_cancelled(&session) {
                return Ok(());
            }
            if count > 0 {
                info!(count, "Scanned workspace .al files");
            }

            // load already-cached packages BEFORE flipping `ready` so
            // that warm-start queries (the common case) see complete symbol
            // coverage. The package-download prompt path still signals ready
            // before user interaction so a missing-dependencies dialog
            // doesn't strand the editor — but warm starts no longer race
            // package symbol load against the first hover/completion.
            let package_paths = project.packages.clone();
            let (staged_symbols, loaded_packages) = match tokio::task::spawn_blocking(move || {
                let staged = al_symbols::SymbolIndex::new();
                let cache = al_symbols::cache::SymbolCache::default_location();
                let loaded = staged.load_packages_cached(&package_paths, &cache)?;
                staged.load_runtime_enums();
                Ok::<_, al_symbols::PackageLoadError>((staged, loaded))
            })
            .await
            {
                Ok(Ok(staged)) => staged,
                Ok(Err(error)) => {
                    tracing::error!(%error, "configured AL symbol package load failed");
                    let message = format!(
                        "AL workspace initialization failed; no partial package batch was indexed: {error}"
                    );
                    client
                        .show_message(MessageType::ERROR, message.clone())
                        .await;
                    return Err(WorkspaceInitError(message));
                }
                Err(error) => {
                    let message = format!("AL workspace package-index worker failed: {error}");
                    tracing::error!(%error, "AL workspace package-index worker failed");
                    client
                        .show_message(MessageType::ERROR, message.clone())
                        .await;
                    return Err(WorkspaceInitError(message));
                }
            };
            if session_cancelled(&session) {
                return Ok(());
            }
            if !project.packages.is_empty() {
                info!(
                    loaded = loaded_packages.len(),
                    total_symbols = staged_symbols.len(),
                    "Staged symbol packages (pre-ready)"
                );
            }

            // Signal readiness. For warm starts the package symbol index is
            // already populated above; for cold starts (no cached packages
            // yet) we signal early to avoid blocking on the prompt and load
            // again after download completes. Publish the valid pre-download
            // project generation first so requests released by the ready
            // notification never observe a spurious "no active project".
            publish_complete_generation(
                &workspace,
                staged_files,
                &staged_symbols,
                Some(project.clone()),
                &loaded_packages,
            )
            .await;
            publish_ready(&init_state);

            let deps = missing_dependencies(&project.all_dependencies(), &loaded_packages);
            if !deps.is_empty() {
                if session_cancelled(&session) {
                    return Ok(());
                }
                let has_server = !project.server_configs.is_empty();
                if let Some(source) = prompt_download_symbols(&client, deps.len(), has_server).await
                {
                    // Resolve the whole dependency closure: a downloaded
                    // package's own manifest dependencies are fetched too.
                    let batch = download_dependency_closure(
                        &workspace,
                        &project,
                        source,
                        &client,
                        session.clone(),
                        &deps,
                    )
                    .await;
                    if !batch.failures.is_empty() {
                        client
                            .show_message(
                                MessageType::ERROR,
                                format!(
                                    "Symbol download completed with {} failure(s): {}",
                                    batch.failures.len(),
                                    batch.failures.join("; ")
                                ),
                            )
                            .await;
                    }
                    let downloaded = batch.paths;
                    if session_cancelled(&session) {
                        return Ok(());
                    }
                    if !downloaded.is_empty() {
                        match refresh_current_symbol_generation(&workspace).await {
                            Ok((loaded, total_symbols)) => {
                                info!(
                                    loaded,
                                    total_symbols, "Published symbol packages (post-download)"
                                );
                                if let Some(active_project) = workspace.project.read().await.clone()
                                {
                                    project = active_project;
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, "downloaded AL symbol generation rejected");
                                client
                                    .show_message(
                                        MessageType::ERROR,
                                        format!(
                                            "Downloaded symbol packages were not indexed; the previous complete generation remains active: {error}"
                                        ),
                                    )
                                    .await;
                            }
                        }
                    }
                }
            }

            log_source_availability(&project.packages);
        }
        Err(e @ al_project::errors::DiscoveryError::NoProjectFound { .. }) => {
            warn!(error = %e, "No AL project found (continuing without packages)");
            client
                .show_message(MessageType::INFO, format!("No AL project found: {e}"))
                .await;

            let source_root = workspace_root.clone();
            let (staged_files, count) = match tokio::task::spawn_blocking(move || {
                let staged = al_source::file_index::FileIndex::new();
                staged.scan(&source_root).map(|count| (staged, count))
            })
            .await
            {
                Ok(Ok(staged)) => staged,
                Ok(Err(error)) => {
                    tracing::error!(%error, "AL syntax-only workspace source scan failed");
                    let message = format!(
                        "AL workspace initialization failed; no partial source index was published: {error}"
                    );
                    client
                        .show_message(MessageType::ERROR, message.clone())
                        .await;
                    return Err(WorkspaceInitError(message));
                }
                Err(error) => {
                    let message = format!("AL workspace source-index worker failed: {error}");
                    tracing::error!(%error, "AL workspace source-index worker failed");
                    client
                        .show_message(MessageType::ERROR, message.clone())
                        .await;
                    return Err(WorkspaceInitError(message));
                }
            };
            if session_cancelled(&session) {
                return Ok(());
            }
            if count > 0 {
                info!(count, "Scanned workspace .al files");
            }

            let staged_symbols = al_symbols::SymbolIndex::new();
            staged_symbols.load_runtime_enums();
            publish_complete_generation(&workspace, staged_files, &staged_symbols, None, &[]).await;

            // Signal readiness even without a project so request handlers
            // don't block forever waiting for initialization.
            publish_ready(&init_state);
        }
        Err(error) => {
            tracing::error!(%error, "AL project discovery failed");
            let message = format!("AL workspace initialization failed: {error}");
            client
                .show_message(MessageType::ERROR, message.clone())
                .await;
            return Err(WorkspaceInitError(message));
        }
    }

    if session_cancelled(&session) {
        return Ok(());
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
    if session_cancelled(&session) {
        return Ok(());
    }
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

    if let Some(diagnostic_state) = diagnostic_state {
        if diagnostic_state.session.is_cancelled() {
            return Ok(());
        }
        diagnostic_state.semantic_cache.lock().await.clear();
        if workspace.config.read().await.diagnostics_scope
            == al_project::config::DiagnosticsScope::Project
        {
            let session = diagnostic_state.session.clone();
            let published = diagnostics::publish_workspace_diagnostics_parts(
                Arc::clone(&workspace),
                client.clone(),
                diagnostic_state.semantic_cache,
                diagnostic_state.published_uris,
                None,
                &session,
            )
            .await;
            if published {
                info!("Published project-scoped diagnostics generation");
            }
        }
    }
    Ok(())
}

/// Load builtins and error codes from disk cache (fast path, no bridge needed).
///
/// Extracted from `AlServer::load_caches_from_disk` to be callable from the background init task.
async fn load_caches_from_disk(
    workspace: &Workspace,
    version: &str,
) -> Result<(), WorkspaceInitError> {
    let (needs_builtins, poisoned_builtins) = match workspace.builtins.read() {
        Ok(builtins) => (builtins.is_empty(), false),
        Err(_) => (true, true),
    };
    if needs_builtins {
        if let Some(cached) = crate::semantic::cache::read_builtins(version) {
            info!(
                count = cached.len(),
                "Loaded built-in types from disk cache"
            );
            crate::semantic::set_builtins(workspace, cached, version);
        } else if poisoned_builtins {
            return Err(WorkspaceInitError(
                "built-in type catalog is poisoned and no complete disk-cache payload is available"
                    .to_string(),
            ));
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
    Ok(())
}

async fn publish_complete_generation(
    workspace: &Workspace,
    staged_files: al_source::file_index::FileIndex,
    staged_symbols: &al_symbols::SymbolIndex,
    project: Option<al_project::project::AlProject>,
    packages: &[al_symbols::model::SymbolPackage],
) {
    // didOpen/didChange take a generation read guard. Once this write guard is
    // acquired, the document store and its corresponding file-index overlays
    // cannot advance until publication finishes.
    let _publication = workspace.generation_lock.write().await;
    for uri in workspace.documents.open_uris() {
        let (Ok(path), Some(text)) = (uri.to_file_path(), workspace.documents.get_text(&uri))
        else {
            continue;
        };
        staged_files.add_file(path, text);
    }

    workspace.file_index.replace_with(staged_files);
    workspace.symbols.replace_with(staged_symbols);
    *workspace.project.write().await = project;
    set_package_info(workspace, packages);
    workspace.invalidate_insight_graph();
    workspace
        .generation_revision
        .fetch_add(1, std::sync::atomic::Ordering::Release);
}

async fn refresh_current_symbol_generation(
    workspace: &Workspace,
) -> Result<(usize, usize), String> {
    loop {
        let generation = workspace.generation_lock.read().await;
        let revision = workspace
            .generation_revision
            .load(std::sync::atomic::Ordering::Acquire);
        let project = workspace.project.read().await.clone();
        let config = workspace.config.read().await.clone();
        let Some(mut project) = project else {
            return Err("the AL project was closed".to_string());
        };
        // Staging may parse many packages. The revision check below makes the
        // snapshot optimistic, so holding the generation read lock throughout
        // that blocking work would only freeze document mutations needlessly.
        drop(generation);

        let staged = tokio::task::spawn_blocking(move || {
            project
                .apply_symbol_settings(&config)
                .map_err(|error| error.to_string())?;
            let symbols = al_symbols::SymbolIndex::new();
            let cache = al_symbols::cache::SymbolCache::default_location();
            let loaded = symbols
                .load_packages_cached(&project.packages, &cache)
                .map_err(|error| error.to_string())?;
            symbols.load_runtime_enums();
            Ok::<_, String>((project, symbols, loaded))
        })
        .await;

        let (project, symbols, loaded) = match staged {
            Ok(Ok(staged)) => staged,
            Ok(Err(error)) => return Err(error),
            Err(error) => return Err(format!("package-index worker failed: {error}")),
        };

        let publication = workspace.generation_lock.write().await;
        if workspace
            .generation_revision
            .load(std::sync::atomic::Ordering::Acquire)
            != revision
        {
            drop(publication);
            continue;
        }
        workspace.symbols.replace_with(&symbols);
        *workspace.project.write().await = Some(project);
        set_package_info(workspace, &loaded);
        workspace.invalidate_insight_graph();
        workspace
            .generation_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        let counts = (loaded.len(), workspace.symbols.len());
        drop(publication);
        return Ok(counts);
    }
}

/// How many *transitive* dependency waves are resolved after the direct
/// `app.json` dependencies.
///
/// Microsoft's extension fetches a dependency's own dependencies recursively.
/// Real BC dependency chains are shallow (an app on top of a library on top of
/// Base Application), so this cap only exists to bound a cyclic or hostile
/// manifest graph; the visited set already prevents re-fetching.
pub(crate) const MAX_TRANSITIVE_DEPENDENCY_DEPTH: usize = 8;

/// Read `.app` manifests, skipping (with a warning) the ones that cannot be
/// read. A single unreadable package must not stop transitive resolution.
pub(crate) fn read_manifests(paths: &[PathBuf]) -> Vec<al_symbols::manifest::NavxManifest> {
    paths
        .iter()
        .filter_map(
            |path| match al_symbols::app_reader::read_app_manifest_file(path) {
                Ok(manifest) => Some(manifest),
                Err(error) => {
                    warn!(
                        package = %path.display(),
                        %error,
                        "Could not read package manifest for transitive dependency resolution"
                    );
                    None
                }
            },
        )
        .collect()
}

/// The next wave of dependencies to download.
///
/// `.app` manifests declare their own dependencies (`NavxManifest.dependencies`).
/// Those were parsed but never resolved, so a dependency's dependencies stayed
/// missing and their symbols never appeared. Returns the declared dependencies
/// of `downloaded` that are neither already `visited` nor satisfied by an
/// `available` package, and records them in `visited`.
pub(crate) fn next_transitive_dependencies(
    downloaded: &[al_symbols::manifest::NavxManifest],
    available: &[al_symbols::manifest::NavxManifest],
    visited: &mut std::collections::HashSet<String>,
) -> Vec<al_project::project::AppDependency> {
    let mut next = Vec::new();
    for manifest in downloaded {
        for dependency in &manifest.dependencies {
            let key = dependency.app_id.to_lowercase();
            if visited.contains(&key) {
                continue;
            }
            let satisfied = available.iter().any(|candidate| {
                candidate.app_id.eq_ignore_ascii_case(&dependency.app_id)
                    && al_symbols::model::version_at_least(
                        &candidate.version,
                        &dependency.min_version,
                    )
            });
            if satisfied {
                visited.insert(key);
                continue;
            }
            visited.insert(key);
            next.push(al_project::project::AppDependency {
                id: dependency.app_id.clone(),
                name: dependency.name.clone(),
                publisher: dependency.publisher.clone(),
                version: dependency.min_version.clone(),
            });
        }
    }
    next
}

/// Download `direct` and then, recursively, whatever those packages themselves
/// depend on — bounded by [`MAX_TRANSITIVE_DEPENDENCY_DEPTH`] and a visited set.
async fn download_dependency_closure(
    workspace: &al_workspace::Workspace,
    project: &al_project::project::AlProject,
    source: DownloadSource,
    client: &tower_lsp::Client,
    session: Option<LspSessionState>,
    direct: &[al_project::project::AppDependency],
) -> DownloadBatch {
    let mut visited: std::collections::HashSet<String> = direct
        .iter()
        .map(|dependency| dependency.id.to_lowercase())
        .collect();
    let mut queue = direct.to_vec();
    let mut batch = DownloadBatch::default();

    for wave in 0..=MAX_TRANSITIVE_DEPENDENCY_DEPTH {
        if queue.is_empty() {
            break;
        }
        if wave > 0 {
            info!(
                wave,
                count = queue.len(),
                "Resolving transitive symbol dependencies"
            );
        }
        let round = match source {
            DownloadSource::Server => {
                download_symbols_from_server(project, &queue, client, session.clone()).await
            }
            DownloadSource::NuGet => {
                download_packages_nuget(workspace, &queue, &project.packages_dir).await
            }
        };
        batch.failures.extend(round.failures);
        if round.paths.is_empty() {
            break;
        }
        if wave == MAX_TRANSITIVE_DEPENDENCY_DEPTH {
            batch.paths.extend(round.paths);
            warn!(
                depth = MAX_TRANSITIVE_DEPENDENCY_DEPTH,
                "Transitive dependency resolution stopped at the depth cap"
            );
            break;
        }
        let downloaded_manifests = read_manifests(&round.paths);
        batch.paths.extend(round.paths);
        let mut available_paths = project.packages.clone();
        available_paths.extend(batch.paths.iter().cloned());
        let available = read_manifests(&available_paths);
        queue = next_transitive_dependencies(&downloaded_manifests, &available, &mut visited);
    }

    batch
}

fn missing_dependencies(
    dependencies: &[al_project::project::AppDependency],
    packages: &[al_symbols::model::SymbolPackage],
) -> Vec<al_project::project::AppDependency> {
    dependencies
        .iter()
        .filter(|dependency| {
            !packages
                .iter()
                .any(|package| package.satisfies_dependency(dependency))
        })
        .cloned()
        .collect()
}

fn set_package_info(workspace: &Workspace, packages: &[al_symbols::model::SymbolPackage]) {
    let info = packages
        .iter()
        .map(|package| al_workspace::PackageInfo {
            name: package.name.clone(),
            publisher: package.publisher.clone(),
            version: package.version.clone(),
            object_count: package.object_count,
        })
        .collect();
    workspace.replace_package_info(info);
}

fn missing_dependencies_in_paths(
    dependencies: &[al_project::project::AppDependency],
    package_paths: &[PathBuf],
) -> Result<Vec<al_project::project::AppDependency>, String> {
    let mut manifests = Vec::with_capacity(package_paths.len());
    let mut failures = Vec::new();
    for path in package_paths {
        match al_symbols::app_reader::read_app_manifest_file(path) {
            Ok(manifest) => manifests.push(manifest),
            Err(error) => failures.push(format!("'{}': {error}", path.display())),
        }
    }
    if !failures.is_empty() {
        return Err(format!(
            "{} configured package manifest(s) could not be read: {}",
            failures.len(),
            failures.join("; ")
        ));
    }

    Ok(dependencies
        .iter()
        .filter(|dependency| {
            !manifests.iter().any(|manifest| {
                manifest.app_id.eq_ignore_ascii_case(&dependency.id)
                    && al_symbols::model::version_at_least(&manifest.version, &dependency.version)
            })
        })
        .cloned()
        .collect())
}

/// Log which loaded packages lack `.al` source files.
/// Outlines are always generated from symbol metadata — no user prompt needed.
fn log_source_availability(packages: &[PathBuf]) {
    let mut no_source = Vec::new();
    for path in packages {
        match al_symbols::virtual_file::app_has_source(path) {
            Ok(true) => {}
            Ok(false) => {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown");
                no_source.push(stem.to_string());
            }
            Err(error) => {
                warn!(
                    package = %path.display(),
                    %error,
                    "Could not inspect package source availability"
                );
            }
        }
    }

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
    session: Option<LspSessionState>,
) -> DownloadBatch {
    let configs = &project.server_configs;
    if configs.is_empty() {
        return DownloadBatch {
            paths: Vec::new(),
            failures: vec![
                "no BC server configuration exists in .zed/debug.json or .vscode/launch.json"
                    .to_string(),
            ],
        };
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
        if session_cancelled(&session) {
            return;
        }
        let c = lsp.clone();
        let session = session.clone();
        let m = msg.to_string();
        tokio::spawn(async move {
            if !session_cancelled(&session) {
                c.show_message(tower_lsp::lsp_types::MessageType::INFO, m)
                    .await;
            }
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
            return DownloadBatch {
                paths: Vec::new(),
                failures: vec![format!("cannot create BC server HTTP client: {e}")],
            };
        }
    };
    // al_project::project::AppDependency is re-exported from al_symbols — clone directly.
    let mut batch = DownloadBatch::default();
    let mut url_deps = Vec::new();
    for dep in deps {
        match config.dev_packages_url(dep) {
            Some(url) => url_deps.push((url, dep.clone())),
            None => batch.failures.push(format!(
                "{}: launch configuration cannot construct a BC dev-packages URL",
                dep.name
            )),
        }
    }
    let results = client.download_all(&url_deps, &dest).await;

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
                batch.paths.push(path);
            }
            Err(e) => {
                warn!(
                    package = %dep.name,
                    error = %e,
                    "Failed to download from BC server"
                );
                batch.failures.push(format!("{}: {e}", dep.name));
            }
        }
    }

    batch
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
) -> DownloadBatch {
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

    let client = match al_symbols::nuget::NuGetClient::new(feeds) {
        Ok(client) => client.with_country(country),
        Err(error) => {
            return DownloadBatch {
                paths: Vec::new(),
                failures: deps
                    .iter()
                    .map(|dependency| {
                        format!(
                            "{}: could not initialize NuGet client: {error}",
                            dependency.name
                        )
                    })
                    .collect(),
            };
        }
    };
    let results = client.download_all(deps, dest).await;

    let mut batch = DownloadBatch::default();
    for (i, result) in results.into_iter().enumerate() {
        match result {
            Ok(path) => {
                info!(
                    package = %deps[i].name,
                    path = %path.display(),
                    "Downloaded symbol package from NuGet"
                );
                batch.paths.push(path);
            }
            Err(e) => {
                warn!(
                    package = %deps[i].name,
                    error = %e,
                    "Failed to download symbol package from NuGet"
                );
                batch.failures.push(format!("{}: {e}", deps[i].name));
            }
        }
    }

    batch
}

/// Handle the `al.downloadSymbols*` commands.
///
/// Downloads symbols from the specified source and reloads the symbol index.
pub(crate) async fn download_symbols_command(server: &AlServer, source: DownloadSource) {
    let generation = server.workspace.generation_lock.read().await;
    let project = server.workspace.project.read().await.clone();
    let Some(project) = project else {
        drop(generation);
        warn!("No AL project found — cannot download symbols");
        server
            .client
            .show_message(MessageType::WARNING, "No AL project found")
            .await;
        return;
    };
    // Package inventory and downloads operate on this immutable project
    // snapshot. Do not block editor writes while filesystem/network work runs.
    drop(generation);

    let all_dependencies = project.all_dependencies();
    let indexed_paths = project.packages.clone();
    let deps = match tokio::task::spawn_blocking(move || {
        missing_dependencies_in_paths(&all_dependencies, &indexed_paths)
    })
    .await
    {
        Ok(Ok(deps)) => deps,
        Ok(Err(error)) => {
            tracing::error!(%error, "existing symbol package inventory is invalid");
            server
                .client
                .show_message(
                    MessageType::ERROR,
                    format!("Cannot determine missing symbols: {error}"),
                )
                .await;
            return;
        }
        Err(error) => {
            tracing::error!(%error, "symbol inventory worker failed");
            server
                .client
                .show_message(
                    MessageType::ERROR,
                    format!("Cannot determine missing symbols because the inventory worker failed: {error}"),
                )
                .await;
            return;
        }
    };
    if deps.is_empty() {
        info!("All symbol dependencies are already satisfied");
        server
            .client
            .show_message(
                MessageType::INFO,
                "All symbol dependencies are already satisfied",
            )
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

    let batch = download_dependency_closure(
        &server.workspace,
        &project,
        source,
        &server.client,
        Some(server.session.clone()),
        &deps,
    )
    .await;
    if server.session.is_cancelled() {
        return;
    }
    if !batch.failures.is_empty() {
        server
            .client
            .show_message(
                MessageType::ERROR,
                format!(
                    "{} symbol download failure(s): {}",
                    batch.failures.len(),
                    batch.failures.join("; ")
                ),
            )
            .await;
    }
    let packages = batch.paths;

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

    let (loaded_count, symbol_count) = match refresh_current_symbol_generation(&server.workspace)
        .await
    {
        Ok(counts) => counts,
        Err(error) => {
            tracing::error!(%error, "downloaded symbol generation rejected");
            server
                    .client
                    .show_message(
                        MessageType::ERROR,
                        format!(
                            "Downloaded packages were not indexed; the previous complete generation remains active: {error}"
                        ),
                    )
                    .await;
            return;
        }
    };

    info!(
        loaded = loaded_count,
        total_symbols = symbol_count,
        source = source_name,
        "Published downloaded symbol generation"
    );

    server
        .client
        .show_message(
            MessageType::INFO,
            format!(
                "Downloaded {} packages from {} ({} symbols)",
                loaded_count, source_name, symbol_count
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
    workspace: &al_workspace::Workspace,
    query: &str,
) -> Option<Vec<SymbolInformation>> {
    // Use the shared search implementation, which reads cached object data
    // instead of reparsing files on every request.
    const MAX_LSP_SYMBOLS: usize = 10_000;
    let ws_results =
        al_analysis::queries::search::workspace_search(workspace, query, MAX_LSP_SYMBOLS);

    let mut results = Vec::new();

    // Top-level objects (table, page, codeunit, etc.)
    for r in ws_results {
        if let Some(file_text_entry) = workspace.file_index.files.get(&r.file_path) {
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
            al_analysis::queries::search::workspace_search_children(workspace, query, remaining);
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

    fn write_manifest_app(path: &std::path::Path, id: &str, version: &str) {
        use std::io::Write;

        let mut bytes = Vec::from(&b"NAVX"[..]);
        bytes.resize(40, 0);
        let mut zip_bytes = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", options).unwrap();
            write!(
                zip,
                r#"<Package><App Id="{id}" Name="Dependency" Publisher="Test" Version="{version}" /></Package>"#
            )
            .unwrap();
            zip.finish().unwrap();
        }
        bytes.extend_from_slice(&zip_bytes);
        std::fs::write(path, bytes).unwrap();
    }

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
    fn multibyte_utf8_survives_trailing_comma_strip() {
        // Regression: the previous in-module `strip_trailing_commas` cast each
        // byte to `char`, corrupting multi-byte UTF-8 (e.g. emoji in a theme
        // name or comment) whenever the settings had a trailing comma.
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

    fn manifest(
        id: &str,
        version: &str,
        dependencies: &[(&str, &str)],
    ) -> al_symbols::manifest::NavxManifest {
        al_symbols::manifest::NavxManifest {
            app_id: id.to_string(),
            name: format!("App {id}"),
            publisher: "Tests".to_string(),
            version: version.to_string(),
            dependencies: dependencies
                .iter()
                .map(
                    |(dep_id, min_version)| al_symbols::manifest::ManifestDependency {
                        app_id: dep_id.to_string(),
                        name: format!("App {dep_id}"),
                        publisher: "Tests".to_string(),
                        min_version: min_version.to_string(),
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn transitive_dependencies_of_downloaded_packages_are_queued() {
        let downloaded = vec![manifest("A", "1.0.0.0", &[("B", "2.0.0.0")])];
        let available = vec![manifest("A", "1.0.0.0", &[("B", "2.0.0.0")])];
        let mut visited: std::collections::HashSet<String> =
            ["a".to_string()].into_iter().collect();
        let next = next_transitive_dependencies(&downloaded, &available, &mut visited);
        assert_eq!(
            next.len(),
            1,
            "B is declared by A and not present: {next:?}"
        );
        assert_eq!(next[0].id, "B");
        assert_eq!(next[0].version, "2.0.0.0");
        assert!(
            visited.contains("b"),
            "the queued dependency must be marked visited"
        );
    }

    #[test]
    fn already_satisfied_or_visited_dependencies_are_not_requeued() {
        let downloaded = vec![manifest(
            "A",
            "1.0.0.0",
            &[("B", "2.0.0.0"), ("C", "1.0.0.0")],
        )];
        // B is already on disk at a new-enough version; C was requested before.
        let available = vec![manifest("B", "2.5.0.0", &[])];
        let mut visited: std::collections::HashSet<String> =
            ["a".to_string(), "c".to_string()].into_iter().collect();
        let next = next_transitive_dependencies(&downloaded, &available, &mut visited);
        assert!(next.is_empty(), "nothing new to download: {next:?}");
    }

    #[test]
    fn an_older_available_package_still_queues_the_dependency() {
        let downloaded = vec![manifest("A", "1.0.0.0", &[("B", "3.0.0.0")])];
        let available = vec![manifest("B", "2.0.0.0", &[])];
        let mut visited: std::collections::HashSet<String> =
            ["a".to_string()].into_iter().collect();
        let next = next_transitive_dependencies(&downloaded, &available, &mut visited);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].id, "B");
    }

    #[test]
    fn a_dependency_cycle_terminates_via_the_visited_set() {
        // A depends on B, B depends back on A.
        let mut visited: std::collections::HashSet<String> =
            ["a".to_string()].into_iter().collect();
        let first = next_transitive_dependencies(
            &[manifest("A", "1.0.0.0", &[("B", "1.0.0.0")])],
            &[],
            &mut visited,
        );
        assert_eq!(first.len(), 1);
        let second = next_transitive_dependencies(
            &[manifest("B", "1.0.0.0", &[("A", "1.0.0.0")])],
            &[],
            &mut visited,
        );
        assert!(
            second.is_empty(),
            "the cycle must not requeue A: {second:?}"
        );
    }

    #[test]
    fn transitive_depth_cap_is_bounded() {
        assert!(
            (1..=16).contains(&MAX_TRANSITIVE_DEPENDENCY_DEPTH),
            "the depth cap must stay a small bound"
        );
    }

    #[test]
    fn unreadable_manifests_are_skipped_rather_than_failing_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("not-an-app.app");
        std::fs::write(&bogus, b"definitely not a NAVX package").unwrap();
        assert!(read_manifests(&[bogus]).is_empty());
    }

    #[test]
    fn download_source_display_names() {
        assert_eq!(DownloadSource::Server.display_name(), "BC server");
        assert_eq!(DownloadSource::NuGet.display_name(), "NuGet");
    }

    #[test]
    fn dependency_check_does_not_treat_one_cached_package_as_all_dependencies() {
        let dependencies = vec![
            al_project::project::AppDependency {
                id: "app-a".into(),
                name: "A".into(),
                publisher: "P".into(),
                version: "1.0.0.0".into(),
            },
            al_project::project::AppDependency {
                id: "app-b".into(),
                name: "B".into(),
                publisher: "P".into(),
                version: "1.0.0.0".into(),
            },
        ];
        let packages = vec![al_symbols::model::SymbolPackage {
            app_id: "APP-A".into(),
            name: "A".into(),
            publisher: "P".into(),
            version: "1.2.0.0".into(),
            objects: vec![],
            object_count: 0,
        }];

        let missing = missing_dependencies(&dependencies, &packages);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "app-b");
    }

    #[test]
    fn dependency_check_requires_minimum_version() {
        assert!(al_symbols::model::version_at_least(
            "27.4.10.0",
            "27.3.999.0"
        ));
        assert!(al_symbols::model::version_at_least("27.3", "27.3.0.0"));
        assert!(!al_symbols::model::version_at_least(
            "26.9.999.0",
            "27.0.0.0"
        ));
        assert!(!al_symbols::model::version_at_least("preview", "27.0.0.0"));
        assert!(al_symbols::model::version_at_least("preview", "PREVIEW"));
    }

    #[test]
    fn path_dependency_check_uses_manifest_identity_and_minimum_version() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("misleading-filename.app");
        write_manifest_app(&package, "wanted-id", "2.1.0.0");
        let dependencies = vec![
            al_project::project::AppDependency {
                id: "WANTED-ID".into(),
                name: "Dependency".into(),
                publisher: "Test".into(),
                version: "2.0.0.0".into(),
            },
            al_project::project::AppDependency {
                id: "other-id".into(),
                name: "Other".into(),
                publisher: "Test".into(),
                version: "1.0.0.0".into(),
            },
        ];

        let missing = missing_dependencies_in_paths(&dependencies, &[package]).unwrap();

        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "other-id");
    }

    #[test]
    fn path_dependency_check_rejects_unreadable_package_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("broken.app");
        std::fs::write(&package, b"not a package").unwrap();

        let error = missing_dependencies_in_paths(&[], &[package]).unwrap_err();

        assert!(error.contains("could not be read"));
        assert!(error.contains("broken.app"));
    }

    /// (`al.nugetFeeds` / `al.useOnlyCustomFeeds` parity):
    /// custom feeds take priority; defaults are appended unless the
    /// only-custom flag is set.
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
        // Non-existent and malformed package paths are logged as inspection
        // errors rather than being misclassified as source-less packages.
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
    async fn malformed_project_is_returned_as_failed_initialization_state() {
        let root = tempfile::tempdir().expect("temporary workspace");
        std::fs::write(root.path().join("app.json"), "{not json").expect("malformed manifest");
        let service = test_server();
        let server = service.inner();
        let state = server.workspace_init_state.clone();
        let result = initialize_workspace(
            Arc::clone(&server.workspace),
            server.client.clone(),
            Some(Url::from_directory_path(root.path()).expect("workspace URI")),
            Some(state.clone()),
            None,
        )
        .await;

        let error = result.expect_err("malformed app.json must fail initialization");
        state.send_replace(WorkspaceInitState::Failed(error.to_string()));
        assert!(matches!(
            state.borrow().clone(),
            WorkspaceInitState::Failed(message)
                if message.contains("app.json") || message.contains("JSON")
        ));
        assert_eq!(server.workspace.file_index.len(), 0);
        assert!(server.workspace.project.read().await.is_none());
    }

    #[tokio::test]
    async fn failed_reindex_retains_the_previous_complete_generation() {
        let root = tempfile::tempdir().expect("temporary workspace");
        std::fs::write(
            root.path().join("app.json"),
            r#"{
                "id":"00000000-0000-0000-0000-000000000001",
                "name":"Replacement",
                "publisher":"Test",
                "version":"1.0.0.0"
            }"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("Replacement.al"),
            r#"codeunit 50101 Replacement { }"#,
        )
        .unwrap();
        std::fs::create_dir(root.path().join(".alpackages")).unwrap();
        std::fs::write(
            root.path().join(".alpackages/Broken.app"),
            b"not a NAVX package",
        )
        .unwrap();

        let service = test_server();
        let server = service.inner();
        let old_path = PathBuf::from("/previous/Stable.al");
        server
            .workspace
            .file_index
            .add_file(old_path.clone(), r#"codeunit 50100 Stable { }"#.to_string());
        server
            .workspace
            .symbols
            .add_entries(&[al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Codeunit,
                id: 50_100,
                name: "Stable".to_string(),
                package: "Previous".to_string(),
                ..Default::default()
            }]);

        let result = initialize_workspace(
            Arc::clone(&server.workspace),
            server.client.clone(),
            Some(Url::from_directory_path(root.path()).expect("workspace URI")),
            None,
            None,
        )
        .await;

        assert!(result.is_err(), "invalid replacement package must fail");
        assert_eq!(
            server
                .workspace
                .file_index
                .get_content(&old_path)
                .as_deref(),
            Some(r#"codeunit 50100 Stable { }"#),
            "failed reindex must leave the old source generation intact"
        );
        assert!(
            server.workspace.symbols.find_by_name("Stable").is_some(),
            "failed reindex must leave the old symbol generation intact"
        );
        assert!(
            server
                .workspace
                .file_index
                .find_by_object_name("Replacement")
                .is_none(),
            "staged replacement sources must not leak into the active index"
        );
    }

    #[tokio::test]
    async fn successful_reindex_reapplies_unsaved_open_documents_at_commit() {
        let root = tempfile::tempdir().expect("temporary workspace");
        std::fs::write(
            root.path().join("app.json"),
            r#"{
                "id":"00000000-0000-0000-0000-000000000001",
                "name":"OpenBuffer",
                "publisher":"Test",
                "version":"1.0.0.0"
            }"#,
        )
        .unwrap();
        let path = root.path().join("OpenBuffer.al");
        std::fs::write(&path, r#"codeunit 50100 "Saved Name" { }"#).unwrap();

        let service = test_server();
        let server = service.inner();
        let uri = Url::from_file_path(&path).unwrap();
        let unsaved = r#"codeunit 50100 "Unsaved Name" { }"#;
        server
            .workspace
            .documents
            .open(uri.clone(), unsaved.to_string())
            .unwrap();

        initialize_workspace(
            Arc::clone(&server.workspace),
            server.client.clone(),
            Some(Url::from_directory_path(root.path()).expect("workspace URI")),
            None,
            None,
        )
        .await
        .expect("valid generation");

        assert_eq!(
            server.workspace.file_index.get_content(&path).as_deref(),
            Some(unsaved)
        );
        assert_eq!(
            server
                .workspace
                .file_index
                .object_info
                .get(&path)
                .map(|info| info.name.clone())
                .as_deref(),
            Some("Unsaved Name")
        );
    }

    #[tokio::test]
    async fn handle_workspace_symbol_empty_workspace_returns_none() {
        let service = test_server();
        let server = service.inner();
        // No files indexed → no symbols → None (not an empty Vec).
        assert!(handle_workspace_symbol(&server.workspace, "anything").is_none());
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

        let results = handle_workspace_symbol(&server.workspace, "Customer")
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
        let results = handle_workspace_symbol(&server.workspace, "AddNumbers")
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
        assert!(handle_workspace_symbol(&server.workspace, "ZZZ_no_such_symbol").is_none());
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

        let results = handle_workspace_symbol(&server.workspace, "AddNumbers")
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

        let results = handle_workspace_symbol(&server.workspace, "")
            .expect("empty query must return all symbols");
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
