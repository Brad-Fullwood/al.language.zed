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
                        None,
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
    // The project guard is taken before the first index swap, so the mutation
    // sequence below contains no await point. `al.reindex` aborts any previous
    // reindex task, and an await between swapping the file and symbol indexes
    // and swapping the project left the two disagreeing, with the revision
    // never bumped: every optimistic publisher then concluded nothing had
    // changed and `require_project_root` handed out the old root against the
    // new file index.
    let mut published_project = workspace.project.write().await;
    for uri in workspace.documents.open_uris() {
        let (Ok(path), Some(text)) = (uri.to_file_path(), workspace.documents.get_text(&uri))
        else {
            continue;
        };
        staged_files.add_file(path, text);
    }

    workspace.file_index.replace_with(staged_files);
    workspace.symbols.replace_with(staged_symbols);
    *published_project = project;
    set_package_info(workspace, packages);
    workspace.invalidate_insight_graph();
    workspace.mark_package_generation_changed();
    workspace
        .generation_revision
        .fetch_add(1, std::sync::atomic::Ordering::Release);
}

async fn refresh_current_symbol_generation(
    workspace: &Workspace,
) -> Result<(usize, usize), String> {
    loop {
        let generation = workspace.generation_lock.read().await;
        // Keyed on the package revision, not the source revision: staging
        // re-reads every `.app` in the cache, which takes seconds on a real
        // project, and a retry keyed on `generation_revision` restarted on
        // every keystroke and never published.
        let revision = workspace.package_revision();
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
        if workspace.package_revision() != revision {
            drop(publication);
            continue;
        }
        // As in `publish_complete_generation`: no await between the first swap
        // and the revision bump.
        let mut published_project = workspace.project.write().await;
        workspace.symbols.replace_with(&symbols);
        *published_project = Some(project);
        set_package_info(workspace, &loaded);
        workspace.invalidate_insight_graph();
        workspace.mark_package_generation_changed();
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
    requested_config: Option<&str>,
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
                download_symbols_from_server(
                    project,
                    &queue,
                    client,
                    session.clone(),
                    requested_config,
                )
                .await
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
        .map(al_workspace::PackageInfo::from)
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
/// `requested_config` names one of the project's launch configurations; with
/// no name the project's first entry is used. Either way the chosen
/// configuration is named in the log and in the messages the user sees, so a
/// project listing Sandbox and Production never downloads from one of them
/// silently. Returns downloaded .app file paths.
async fn download_symbols_from_server(
    project: &al_project::project::AlProject,
    deps: &[al_project::project::AppDependency],
    lsp_client: &tower_lsp::Client,
    session: Option<LspSessionState>,
    requested_config: Option<&str>,
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

    let config = match al_bc::launch::pick_config(configs, requested_config) {
        Ok(config) => config,
        Err(error) => {
            return DownloadBatch {
                paths: Vec::new(),
                failures: vec![error],
            };
        }
    };
    // The launch configuration ships in the repository, and this download
    // presents the user's Business Central credential to the server it names.
    if let Err(error) = al_project::trust::authorize_cached_credential(
        &project.root,
        &al_project::trust::BcTarget::from_launch(config),
        match config.authentication {
            al_bc::launch::AuthMethod::AAD => al_project::trust::CredentialKind::Bearer,
            _ => al_project::trust::CredentialKind::Basic,
        },
        al_project::trust::TargetSource::Repository,
    ) {
        return DownloadBatch {
            paths: Vec::new(),
            failures: vec![error],
        };
    }
    info!(
        config = %config.name,
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
///
/// A feed that is neither https nor http to loopback is dropped here, named in
/// the log, so the configured list and the list the client is given agree. The
/// client refuses the same URLs again.
pub(crate) fn effective_nuget_feeds(
    config: &al_project::config::AlConfig,
) -> Vec<al_project::project::NuGetFeed> {
    let mut feeds: Vec<al_project::project::NuGetFeed> = config
        .nuget_feeds
        .iter()
        .filter(|feed| {
            let acceptable = al_symbols::nuget::is_acceptable_feed_url(&feed.url);
            if !acceptable {
                warn!(
                    feed = %feed.name,
                    url = %feed.url,
                    "Ignoring a NuGet feed that is neither https nor http to loopback"
                );
            }
            acceptable
        })
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
pub(crate) async fn download_symbols_command(
    server: &AlServer,
    source: DownloadSource,
    requested_config: Option<&str>,
) {
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
        requested_config,
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
    let data_dir = al_project::project::user_data_dir()?;
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
    // Zed's settings.json is JSONC: comments and a trailing comma after the
    // last property are both legal there and both rejected by serde_json. The
    // one stripper for the workspace lives in `al_types::jsonc`.
    Ok(serde_json::from_str(
        &al_types::jsonc::strip_json_comments(input),
    )?)
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
mod tests;
