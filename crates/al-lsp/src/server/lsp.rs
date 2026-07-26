//! AlServer state and LSP lifecycle.

use al_workspace::Workspace;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, Mutex, RwLock};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::request::{GotoImplementationParams, GotoImplementationResponse};
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use super::commands;
use super::completions;
use super::definition;
use super::diagnostics;
use super::formatting;
use super::handlers;
use super::hover;
use super::workspace;

fn internal_error(message: impl Into<std::borrow::Cow<'static, str>>) -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::InternalError,
        message: message.into(),
        data: None,
    }
}

fn content_modified_error() -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        // LSP ContentModified: the client may retry against its current text.
        code: tower_lsp::jsonrpc::ErrorCode::ServerError(-32801),
        message: "workspace changed while diagnostics were being computed".into(),
        data: None,
    }
}

/// Debounce delay for diagnostics: wait this long after the last keystroke before running.
/// prevents bridge calls (up to 5s) from blocking hover/completion.
const DIAGNOSTICS_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

/// Every `al.*` command the server advertises in `executeCommandProvider` and
/// handles in [`AlServer::execute_command`]. Single source of truth: the
/// capability list and the dispatch both derive from this slice, and the
/// CodeLens commands (`al.findReferences`, `al.showProfiler`, `al.runTest`)
/// must all appear here so no clickable lens is a dead no-op. The
/// `code_lens` test asserts `LENS_COMMAND_IDS ⊆ SUPPORTED_COMMANDS`.
pub(crate) const SUPPORTED_COMMANDS: &[&str] = &[
    "al.downloadSymbols",
    "al.downloadSymbolsServer",
    "al.downloadSymbolsNuget",
    "al.clearSymbolCache",
    "al.formatFile",
    "al.lintFile",
    "al.getStatus",
    "al.reindex",
    "al.compile",
    "al.applyRecommendedSettings",
    // Keep CodeLens-backed commands in sync with
    // `al_analysis::queries::code_lens::LENS_COMMAND_IDS`.
    "al.findReferences",
    "al.showProfiler",
    "al.runTest",
];

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunnablesParams {
    text_document: TextDocumentIdentifier,
    #[serde(default)]
    position: Option<Position>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Runnable {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<LocationLink>,
    kind: &'static str,
    args: ShellRunnableArgs,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellRunnableArgs {
    environment: std::collections::HashMap<String, String>,
    cwd: std::path::PathBuf,
    program: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceInitState {
    Initializing,
    Ready,
    Failed(String),
}

/// Shared cancellation state for every task owned by one LSP session.
///
/// tower-lsp closes its client transport as soon as it receives `exit`, but
/// workspace initialization and debounced diagnostics run in tasks outside the
/// request that spawned them. A JoinHandle abort is still useful for prompt
/// shutdown, but it is not a sufficient transport-safety contract: a task can
/// be between handle publication and cancellation, or already executing a
/// ready future. Every background path therefore observes this state before it
/// sends anything to the client.
#[derive(Clone, Debug, Default)]
pub(crate) struct LspSessionState {
    cancelled: Arc<AtomicBool>,
}

impl LspSessionState {
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod session_lifecycle_tests {
    use super::*;

    #[test]
    fn cancellation_is_shared_by_every_session_clone() {
        let session = LspSessionState::default();
        let background_task = session.clone();

        assert!(!background_task.is_cancelled());
        session.cancel();
        assert!(background_task.is_cancelled());
    }

    #[tokio::test]
    async fn language_server_shutdown_cancels_background_transport_access() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();

        assert!(!server.session.is_cancelled());
        LanguageServer::shutdown(server)
            .await
            .expect("shutdown should cancel an idle server");
        assert!(server.session.is_cancelled());
    }
}

pub struct AlServer {
    pub(crate) client: Client,
    pub(crate) workspace: Arc<Workspace>,
    /// Explicit lifetime shared by request handlers and detached/background
    /// tasks. Once cancelled, no task may write to the LSP client transport.
    pub(crate) session: LspSessionState,
    /// Root URI from initialize params, used in initialized().
    pub(crate) root_uri: RwLock<Option<Url>>,
    /// Handle to the currently-pending debounced diagnostics task.
    /// Replaced (and thus cancelled) on every new keystroke.
    /// diagnostics run async, not inline in did_change.
    pub(crate) diag_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Handle to the background workspace initialisation task.
    /// workspace init runs async so initialized() returns promptly.
    pub(crate) init_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// JoinHandle for the most recent al.reindex background task.
    /// Stored so a second al.reindex can abort an in-flight previous run.
    pub(crate) reindex_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Guard against double-initialization.
    /// Zed may send `initialized` twice when opening multiple worktrees.
    /// CAS ensures workspace init runs only once per server instance.
    pub(crate) init_done: AtomicBool,
    /// Observable workspace-initialization state. A watch channel provides an
    /// atomic snapshot plus race-free change notification, and preserves the
    /// actual failure reason instead of collapsing failed initialization into
    /// the same boolean as success.
    pub(crate) workspace_init_state: watch::Sender<WorkspaceInitState>,
    /// Set the first time we report a persistent semantic-bridge failure
    /// (`Timeout` / `Poisoned`) to the user, so subsequent file opens with
    /// the same broken bridge don't spam `window/showMessage(WARNING)`.
    pub(crate) semantic_failure_reported: AtomicBool,
    /// Whether the client advertised `textDocument.definition.linkSupport`.
    /// The LSP spec only permits a `LocationLink[]` go-to-definition response
    /// when this is set; otherwise the server must return a plain `Location[]`.
    /// Captured from the `initialize` capabilities.
    pub(crate) definition_link_support: AtomicBool,
    /// Whether the client advertised
    /// `textDocument.documentSymbol.hierarchicalDocumentSymbolSupport`. The LSP
    /// spec only permits the nested `DocumentSymbol[]` response when this is set;
    /// otherwise the server must return a flat `SymbolInformation[]`. Captured
    /// from the `initialize` capabilities.
    pub(crate) document_symbol_hierarchical: AtomicBool,
    /// URIs that received non-empty project-scope diagnostics in the previous
    /// complete publication. The next generation clears only entries that
    /// became clean instead of sending empty arrays for every indexed file.
    pub(crate) workspace_diagnostic_uris: Arc<Mutex<std::collections::HashSet<Url>>>,
    /// Latest semantic-bridge diagnostics keyed by the exact open-document
    /// snapshot they were computed from. Project-scope native refreshes merge
    /// these without repeating multi-second bridge calls.
    pub(crate) semantic_diagnostic_cache:
        Arc<Mutex<std::collections::HashMap<Url, diagnostics::CachedSemanticDiagnostics>>>,
}

#[cfg(test)]
mod symbol_package_configuration_tests {
    use super::*;

    fn project(root: std::path::PathBuf) -> al_project::project::AlProject {
        al_project::project::AlProject {
            root: root.clone(),
            app_json: al_project::project::AppManifest {
                id: "00000000-0000-0000-0000-000000000001".to_string(),
                name: "Configuration test".to_string(),
                publisher: "Tests".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: root.join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
        }
    }

    #[tokio::test]
    async fn configuration_reload_replaces_package_cache_and_local_folder_paths() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("symbols-cache");
        let local = root.path().join("shared-symbols");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        *server.workspace.project.write().await = Some(project(root.path().to_path_buf()));
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        server
            .did_change_configuration(DidChangeConfigurationParams {
                settings: serde_json::json!({
                    "al": {
                        "packageCachePath": "symbols-cache",
                        "appLocalFolderPaths": ["shared-symbols"]
                    }
                }),
            })
            .await;

        let stored = server.workspace.project.read().await;
        let project = stored.as_ref().expect("project remains loaded");
        assert_eq!(project.packages_dir, cache);
        assert!(project.packages.is_empty());
        let config = server.workspace.config.read().await;
        assert_eq!(
            config.package_cache_path.as_deref(),
            Some(std::path::Path::new("symbols-cache"))
        );
        assert_eq!(
            config.app_local_folder_paths,
            [std::path::PathBuf::from("shared-symbols")]
        );
    }

    #[tokio::test]
    async fn rejected_package_reload_retains_previous_config_project_and_symbols() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("symbols-cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("Broken.app"), b"not an app").unwrap();
        let original_project = project(root.path().to_path_buf());
        let original_packages_dir = original_project.packages_dir.clone();
        *server.workspace.project.write().await = Some(original_project);
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
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        server
            .did_change_configuration(DidChangeConfigurationParams {
                settings: serde_json::json!({
                    "al": {
                        "packageCachePath": "symbols-cache"
                    }
                }),
            })
            .await;

        let project = server.workspace.project.read().await;
        assert_eq!(
            project.as_ref().unwrap().packages_dir,
            original_packages_dir
        );
        assert!(server
            .workspace
            .config
            .read()
            .await
            .package_cache_path
            .is_none());
        assert!(server.workspace.symbols.find_by_name("Stable").is_some());
    }
}

#[cfg(test)]
mod document_close_generation_tests {
    use super::*;

    #[tokio::test]
    async fn close_restores_saved_source_instead_of_leaving_unsaved_overlay() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server.workspace.config.write().await.diagnostics_scope =
            al_project::config::DiagnosticsScope::OpenFiles;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Saved.al");
        let saved = r#"codeunit 50100 "Saved" { }"#;
        let unsaved = r#"codeunit 50100 "Unsaved" { }"#;
        std::fs::write(&path, saved).unwrap();
        let uri = Url::from_file_path(&path).unwrap();
        server
            .workspace
            .documents
            .open(uri.clone(), unsaved.to_string())
            .unwrap();
        server
            .workspace
            .file_index
            .add_file(path.clone(), unsaved.to_string());

        server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
            })
            .await;

        assert_eq!(
            server.workspace.file_index.get_content(&path).as_deref(),
            Some(saved)
        );
        assert!(server
            .workspace
            .file_index
            .find_by_object_name("Saved")
            .is_some());
        assert!(server
            .workspace
            .file_index
            .find_by_object_name("Unsaved")
            .is_none());
    }

    #[tokio::test]
    async fn close_removes_a_never_saved_transient_document() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server.workspace.config.write().await.diagnostics_scope =
            al_project::config::DiagnosticsScope::OpenFiles;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Transient.al");
        let uri = Url::from_file_path(&path).unwrap();
        let text = r#"codeunit 50100 "Transient" { }"#;
        server
            .workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        server
            .workspace
            .file_index
            .add_file(path.clone(), text.to_string());

        server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
            })
            .await;

        assert!(server.workspace.file_index.get_content(&path).is_none());
        assert!(server
            .workspace
            .file_index
            .find_by_object_name("Transient")
            .is_none());
    }
}

impl AlServer {
    pub(crate) fn new(client: Client) -> Self {
        let workspace = Arc::new(Workspace::new());
        let (workspace_init_state, _initial_receiver) =
            watch::channel(WorkspaceInitState::Initializing);
        let session = LspSessionState::default();

        // Register a notify sink so bridge failures reach the user.
        let sink_client = client.clone();
        let sink_session = session.clone();
        let _ = workspace
            .notify_sink
            .set(std::sync::Arc::new(move |msg: &str| {
                if sink_session.is_cancelled() {
                    return;
                }
                let c = sink_client.clone();
                let session = sink_session.clone();
                let m = msg.to_owned();
                tokio::spawn(async move {
                    if !session.is_cancelled() {
                        c.show_message(tower_lsp::lsp_types::MessageType::WARNING, m)
                            .await;
                    }
                });
            }));

        Self {
            client,
            workspace,
            session,
            root_uri: RwLock::new(None),
            diag_task: Mutex::new(None),
            init_task: Mutex::new(None),
            reindex_task: Mutex::new(None),
            init_done: AtomicBool::new(false),
            workspace_init_state,
            semantic_failure_reported: AtomicBool::new(false),
            definition_link_support: AtomicBool::new(false),
            document_symbol_hierarchical: AtomicBool::new(false),
            workspace_diagnostic_uris: Arc::new(Mutex::new(std::collections::HashSet::new())),
            semantic_diagnostic_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Atomically check whether we've already shown the user a bridge-broken
    /// notification this session, and if not, set the flag.
    ///
    /// Returns `true` exactly once per server session, so callers can fire a
    /// `window/showMessage(WARNING)` without spamming the user across many
    /// open files when the bridge is poisoned.
    pub(crate) fn should_report_semantic_failure(&self) -> bool {
        !self
            .semantic_failure_reported
            .swap(true, std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn diagnostic_publication_state(&self) -> diagnostics::DiagnosticPublicationState {
        diagnostics::DiagnosticPublicationState {
            semantic_cache: Arc::clone(&self.semantic_diagnostic_cache),
            published_uris: Arc::clone(&self.workspace_diagnostic_uris),
            session: self.session.clone(),
        }
    }

    /// Wait for a usable workspace generation. Initialization failures and
    /// timeouts are request errors: serving an empty/partial answer would make
    /// a broken startup indistinguishable from a valid "no result".
    async fn await_ready(&self) -> Result<tokio::sync::RwLockReadGuard<'_, ()>> {
        let mut receiver = self.workspace_init_state.subscribe();
        let wait = async {
            loop {
                let state = receiver.borrow().clone();
                match state {
                    WorkspaceInitState::Ready => return Ok(()),
                    WorkspaceInitState::Failed(error) => {
                        return Err(internal_error(format!(
                            "AL workspace initialization failed: {error}"
                        )));
                    }
                    WorkspaceInitState::Initializing => {}
                }
                receiver.changed().await.map_err(|_| {
                    internal_error("AL workspace initialization state channel closed")
                })?;
            }
        };

        match tokio::time::timeout(std::time::Duration::from_secs(30), wait).await {
            Ok(result) => result?,
            Err(_) => {
                tracing::error!(
                    "await_ready: timed out after 30s waiting for workspace initialization"
                );
                return Err(internal_error(
                    "AL workspace initialization timed out after 30 seconds",
                ));
            }
        }
        Ok(self.workspace.generation_lock.read().await)
    }

    /// Wait until workspace initialization has published the toolchain/project
    /// generation needed by semantic phase two.
    ///
    /// `didOpen` intentionally publishes native phase-one diagnostics before
    /// this wait. Without the wait, a fast open can call `get_or_init_bridge`
    /// before toolchain discovery stores its result, observe `None`, and never
    /// retry semantic analysis for that document.
    pub(crate) async fn await_semantic_workspace(&self) -> std::result::Result<(), String> {
        let generation = self
            .await_ready()
            .await
            .map_err(|error| error.message.into_owned())?;
        drop(generation);
        Ok(())
    }

    pub(crate) async fn ensure_builtins_loaded(&self) -> Result<()> {
        let poisoned = match self.workspace.builtins.read() {
            Ok(builtins) if !builtins.is_empty() => return Ok(()),
            Ok(_) => false,
            Err(_) => true,
        };

        if let Some(guard) = self.get_or_init_bridge().await {
            if let Some(bridge) = guard.as_ref() {
                match bridge.builtin_types().await {
                    Ok(types) => {
                        tracing::info!(count = types.len(), "Loaded built-in types via bridge");
                        let version = bridge.version().to_string();
                        crate::semantic::set_builtins(&self.workspace, types, &version);
                        return Ok(());
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Failed to load built-in types via bridge");
                        self.client
                            .show_message(
                                MessageType::WARNING,
                                format!("Failed to load AL built-in types: {error}"),
                            )
                            .await;
                    }
                }
            }
        }
        if poisoned {
            return Err(internal_error(
                "built-in type catalog is poisoned and no fresh semantic payload was available",
            ));
        }
        Ok(())
    }

    pub(crate) async fn ensure_error_codes_loaded(&self) {
        if !self.workspace.error_codes.is_empty() {
            return;
        }

        if let Some(guard) = self.get_or_init_bridge().await {
            if let Some(bridge) = guard.as_ref() {
                match bridge.error_codes().await {
                    Ok(codes) => {
                        tracing::info!(count = codes.len(), "Loaded error codes via bridge");
                        for ec in codes {
                            self.workspace
                                .error_codes
                                .insert(ec.code.clone(), ec.message.clone());
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Failed to load error codes via bridge");
                        self.client
                            .show_message(
                                MessageType::WARNING,
                                format!("Failed to load AL error codes: {error}"),
                            )
                            .await;
                    }
                }
            }
        }
    }

    /// Look up an error code description for diagnostic enrichment.
    pub(crate) fn error_code_description(&self, code: &str) -> Option<String> {
        self.workspace
            .error_codes
            .get(code)
            .map(|v| v.value().clone())
    }

    async fn runnables(&self, params: RunnablesParams) -> Result<Vec<Runnable>> {
        let _generation = self.await_ready().await?;
        let file_path = params.text_document.uri.to_file_path().map_err(|()| {
            tower_lsp::jsonrpc::Error::invalid_params("runnables require a file URI")
        })?;
        if !file_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("al"))
        {
            return Ok(Vec::new());
        }

        let project_root = self
            .workspace
            .project
            .read()
            .await
            .as_ref()
            .map(|project| project.root.clone());
        let cwd = project_root
            .clone()
            .or_else(|| file_path.parent().map(std::path::Path::to_path_buf))
            .ok_or_else(|| {
                tower_lsp::jsonrpc::Error::invalid_params("AL file has no parent directory")
            })?;
        let explorer = resolve_explorer_binary().map_err(internal_error)?;
        let discovered = al_analysis::queries::tests::discover_tests(&self.workspace)
            .map_err(|error| internal_error(error.to_string()))?;

        Ok(build_runnables(
            &params.text_document.uri,
            &file_path,
            &cwd,
            &explorer,
            project_root.is_some(),
            &discovered,
            params.position,
        ))
    }

    pub(crate) async fn get_or_init_bridge(
        &self,
    ) -> Option<tokio::sync::RwLockReadGuard<'_, Option<crate::semantic::SemanticBridge>>> {
        crate::semantic::get_or_init_bridge(&self.workspace).await
    }

    /// Recompute visible diagnostics after an atomic configuration change.
    ///
    /// Previous project publications are cleared under a short generation read
    /// lock, then open-file semantic work runs against versioned snapshots
    /// without blocking edits. Project scope finishes with a bridge-free native
    /// generation that merges only still-current semantic cache entries.
    async fn refresh_diagnostics_after_configuration(&self) {
        let generation = self.workspace.generation_lock.read().await;
        let stale: Vec<Url> = {
            let mut published = self.workspace_diagnostic_uris.lock().await;
            published.drain().collect()
        };
        for uri in stale {
            let version = self.workspace.documents.get_client_version(&uri);
            self.client
                .publish_diagnostics(uri, Vec::new(), version)
                .await;
        }
        let open_snapshots: Vec<(Url, Arc<String>, i32)> = self
            .workspace
            .documents
            .open_uris()
            .into_iter()
            .filter_map(|uri| {
                self.workspace
                    .documents
                    .get_text_and_client_version(&uri)
                    .map(|(text, version)| (uri, text, version))
            })
            .collect();
        let scope = self.workspace.config.read().await.diagnostics_scope;
        drop(generation);

        for (uri, text, version) in open_snapshots {
            diagnostics::publish_diagnostics(self, &uri, text, version).await;
        }
        if scope == al_project::config::DiagnosticsScope::Project {
            diagnostics::publish_workspace_diagnostics(self).await;
        }
    }

    /// Schedule debounced diagnostics for `uri` with the given document text.
    ///
    /// Cancels the previous pending task (if any) so that only
    /// the most recent keystroke triggers a diagnostics run. The actual diagnostics
    /// publish runs after `DIAGNOSTICS_DEBOUNCE` of silence. This prevents bridge
    /// calls (up to bridge timeout = 5s) from blocking hover/completion.
    async fn schedule_diagnostics(&self, uri: Url) {
        // skip diagnostics for virtual symbol cache files — they are not
        // workspace files and Zed logs a warning for every publishDiagnostics on them.
        if crate::server::diagnostics::is_cache_path(&uri) {
            tracing::debug!(uri = %uri, "schedule_diagnostics: skipping cache file");
            return;
        }

        // Hold the `diag_task` lock across abort → spawn → store as one
        // critical section. Releasing it between the abort and the store let two
        // interleaved did_change handlers both observe "no pending task", spawn
        // two debounce tasks, and race two publishes for the same URI — the
        // second store overwrote the first handle without aborting it. Holding
        // the guard serializes scheduling so only the most recent keystroke's
        // task survives.
        let mut guard = self.diag_task.lock().await;
        if let Some(old) = guard.take() {
            old.abort();
        }

        let workspace = Arc::clone(&self.workspace);
        let client = self.client.clone();
        let semantic_diagnostic_cache = Arc::clone(&self.semantic_diagnostic_cache);
        let workspace_diagnostic_uris = Arc::clone(&self.workspace_diagnostic_uris);
        let session = self.session.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(DIAGNOSTICS_DEBOUNCE).await;
            if session.is_cancelled() {
                return;
            }
            // Emit syntax-only diagnostics from the debounced task.
            // Bridge diagnostics (semantic) are emitted on did_open and lintFile command.
            //
            // syntax_diagnostics is CPU-bound (tree-sitter parse + lint walk).
            // Off-load to the blocking pool so concurrent async LSP requests
            // are not stalled for the duration of the parse on large files.
            //
            // Stale-document guard: between the keystroke that scheduled this task
            // and the 400 ms debounce expiry, the user may have closed the document
            // (did_close already cleared diagnostics with an empty publish). If we
            // computed and published now we'd resurrect ghost squiggles on a
            // closed document.
            let Some((document_text, document_version)) =
                workspace.documents.get_text_and_client_version(&uri)
            else {
                tracing::debug!(uri = %uri, "debounced diagnostics: document no longer open, skipping publish");
                return;
            };
            if workspace.config.read().await.diagnostics_scope
                == al_project::config::DiagnosticsScope::Project
            {
                crate::server::diagnostics::publish_workspace_diagnostics_parts(
                    workspace,
                    client,
                    semantic_diagnostic_cache,
                    workspace_diagnostic_uris,
                    Some(uri),
                    &session,
                )
                .await;
                return;
            }
            // Read config here (not at schedule time) so only the task that
            // survives the debounce pays the clone — keystrokes that abort the
            // previous task before its sleep elapses never clone AlConfig. The
            // clone is needed so per-rule lint filtering works in spawn_blocking.
            let config = workspace.config.read().await.clone();
            let project_root = workspace
                .project
                .read()
                .await
                .as_ref()
                .map(|project| project.root.clone());
            let diag_uri = uri.clone();
            let worker_workspace = Arc::clone(&workspace);
            let lsp_diags: Vec<Diagnostic> = match tokio::task::spawn_blocking(move || {
                al_analysis::queries::diagnostics::syntax_diagnostics_at_root(
                    &worker_workspace,
                    &diag_uri,
                    &config,
                    project_root.as_deref(),
                )
                .iter()
                .map(crate::server::diagnostics::syntax_diag_to_lsp)
                .collect()
            })
            .await
            {
                Ok(diags) => diags,
                Err(e) => {
                    if session.is_cancelled() {
                        return;
                    }
                    tracing::error!("debounced diagnostics worker failed: {e}");
                    client
                        .show_message(
                            MessageType::ERROR,
                            format!("AL diagnostics worker failed: {e}"),
                        )
                        .await;
                    return;
                }
            };
            let still_current = workspace
                .documents
                .get_text_and_client_version(&uri)
                .is_some_and(|(current_text, current_version)| {
                    current_version == document_version
                        && Arc::ptr_eq(&current_text, &document_text)
                });
            if !still_current {
                tracing::debug!(
                    uri = %uri,
                    document_version,
                    "debounced diagnostics: document changed during analysis, skipping stale publish"
                );
                return;
            }
            if session.is_cancelled() {
                return;
            }
            client
                .publish_diagnostics(uri, lsp_diags, Some(document_version))
                .await;
        });

        *guard = Some(handle);
    }
}

/// Extract the "al" sub-object from a settings value, or use the value as-is.
///
/// Zed sends settings nested under an "al" key; other clients may send flat objects.
/// Used by both `initialize` and `did_change_configuration` to normalise the input.
fn extract_al_settings(value: serde_json::Value) -> serde_json::Value {
    value.get("al").cloned().unwrap_or(value)
}

#[tower_lsp::async_trait]
impl LanguageServer for AlServer {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let root_uri = params
            .root_uri
            .as_ref()
            .or_else(|| {
                params
                    .workspace_folders
                    .as_ref()
                    .and_then(|f| f.first().map(|wf| &wf.uri))
            })
            .cloned();

        tracing::info!(root_uri = ?root_uri, "initialize: storing root URI");

        *self.root_uri.write().await = root_uri;

        // Only emit `LocationLink[]` from go-to-definition when the client opted
        // in via `textDocument.definition.linkSupport`; otherwise the LSP spec
        // requires a plain `Location[]`.
        let link_support = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|td| td.definition.as_ref())
            .and_then(|d| d.link_support)
            .unwrap_or(false);
        self.definition_link_support
            .store(link_support, Ordering::Relaxed);

        // Only emit the nested `DocumentSymbol[]` outline when the client opted
        // in via `textDocument.documentSymbol.hierarchicalDocumentSymbolSupport`;
        // otherwise the LSP spec requires the flat `SymbolInformation[]` form.
        let hierarchical_symbols = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|td| td.document_symbol.as_ref())
            .and_then(|ds| ds.hierarchical_document_symbol_support)
            .unwrap_or(false);
        self.document_symbol_hierarchical
            .store(hierarchical_symbols, Ordering::Relaxed);

        if let Some(init_opts) = params.initialization_options {
            let al_settings = extract_al_settings(init_opts);
            let cap = {
                let mut config = self.workspace.config.write().await;
                let unknown = config.merge(&al_settings);
                if !unknown.is_empty() {
                    tracing::warn!("Unknown settings in initializationOptions: {:?}", unknown);
                }
                config.max_document_size_bytes
            };
            // apply the per-document size cap to the store.
            self.workspace.documents.set_max_doc_bytes(cap);
            tracing::info!("Parsed initialization options into config");
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        // Request save notifications so did_save can refresh diagnostics.
                        // `include_text: false` — we already have the latest text in the
                        // document store from did_change, so there's no need to re-send it.
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(false),
                        })),
                        ..Default::default()
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                })),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: al_syntax::tokens::token_types::LEGEND
                                    .iter()
                                    .map(|s| SemanticTokenType::new(s))
                                    .collect(),
                                token_modifiers: al_syntax::tokens::token_modifiers::LEGEND
                                    .iter()
                                    .map(|s| SemanticTokenModifier::new(s))
                                    .collect(),
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    ..Default::default()
                }),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                // Pull diagnostics: Zed fetches fresh diagnostics on demand (tab switch, save).
                // When workspace_diagnostics is true, the `workspace_diagnostic` handler
                // reports parse/syntax errors across every indexed workspace file, plus
                // bridge/semantic diagnostics for open documents.
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions {
                        identifier: Some("al-lsp".to_string()),
                        inter_file_dependencies: true,
                        workspace_diagnostics: true,
                        work_done_progress_options: WorkDoneProgressOptions::default(),
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: SUPPORTED_COMMANDS.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "al-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        // guard against double-init when Zed sends `initialized` more than once
        // (e.g., when opening multiple worktrees or after a crash-restart cycle).
        if self
            .init_done
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::warn!("initialized: received duplicate `initialized` notification — ignoring");
            return;
        }

        self.client
            .log_message(MessageType::INFO, "AL Language Server initialized")
            .await;

        let root_uri = self.root_uri.read().await.clone();
        tracing::info!(root_uri = ?root_uri, "initialized: spawning workspace init in background");

        // spawn workspace initialization into a background task so this
        // notification handler returns promptly. Clients must not be kept waiting by
        // NuGet downloads, package loading, or bridge initialization.
        let ws = Arc::clone(&self.workspace);
        let client = self.client.clone();
        let init_state = self.workspace_init_state.clone();
        let diagnostic_state = self.diagnostic_publication_state();
        let handle = tokio::spawn(async move {
            // Publish Ready as soon as a complete pre-download generation
            // exists. A fatal pre-ready error is retained for every request.
            if let Err(error) = workspace::initialize_workspace(
                ws,
                client,
                root_uri,
                Some(init_state.clone()),
                Some(diagnostic_state),
            )
            .await
            {
                tracing::error!(%error, "workspace initialization failed");
                init_state.send_replace(WorkspaceInitState::Failed(error.to_string()));
            }
        });
        *self.init_task.lock().await = Some(handle);
    }

    async fn shutdown(&self) -> Result<()> {
        // Close the logical session before aborting task handles. This is the
        // fail-closed guard for a task that is already running or whose handle
        // races with shutdown: it may finish local computation, but it cannot
        // write to tower-lsp's transport after `exit` closes that transport.
        self.session.cancel();
        self.workspace_init_state
            .send_replace(WorkspaceInitState::Failed(
                "language server is shutting down".to_string(),
            ));
        if let Some(task) = self.diag_task.lock().await.take() {
            task.abort();
            if let Err(error) = task.await {
                if !error.is_cancelled() {
                    return Err(internal_error(format!(
                        "diagnostics task failed during shutdown: {error}"
                    )));
                }
            }
        }
        if let Some(task) = self.init_task.lock().await.take() {
            task.abort();
            if let Err(error) = task.await {
                if !error.is_cancelled() {
                    return Err(internal_error(format!(
                        "workspace initialization task failed during shutdown: {error}"
                    )));
                }
            }
        }
        if let Some(task) = self.reindex_task.lock().await.take() {
            task.abort();
            if let Err(error) = task.await {
                if !error.is_cancelled() {
                    return Err(internal_error(format!(
                        "workspace reindex task failed during shutdown: {error}"
                    )));
                }
            }
        }
        crate::semantic::shutdown_bridge(&self.workspace).await;
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let generation = self.workspace.generation_lock.write().await;
        let uri = params.text_document.uri.clone();
        let text = params.text_document.text.clone();
        let client_version = params.text_document.version;
        tracing::info!(uri = %uri, len = text.len(), "did_open");

        if let Err(error) = self.workspace.documents.open_with_client_version(
            uri.clone(),
            params.text_document.text,
            client_version,
        ) {
            drop(generation);
            tracing::error!(uri = %uri, %error, "did_open rejected document");
            self.client
                .show_message(
                    MessageType::ERROR,
                    format!("AL language server could not open {uri}: {error}"),
                )
                .await;
            self.client
                .publish_diagnostics(uri, Vec::new(), Some(client_version))
                .await;
            return;
        }
        self.semantic_diagnostic_cache.lock().await.remove(&uri);
        al_workspace::on_document_change(&self.workspace, &uri, &text);
        let snapshot = self.workspace.documents.get_text_and_client_version(&uri);
        drop(generation);

        // Diagnostics carry and revalidate the client version. A didChange can
        // therefore proceed while semantic analysis runs; stale results are
        // discarded instead of holding the generation lock for several seconds.
        if let Some((text, version)) = snapshot {
            diagnostics::publish_diagnostics(self, &uri, text, version).await;
        } else {
            tracing::error!(uri = %uri, "did_open document disappeared before diagnostics snapshot");
            self.client
                .show_message(
                    MessageType::ERROR,
                    format!(
                        "AL language server opened {uri}, but its document state disappeared before analysis"
                    ),
                )
                .await;
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let generation = self.workspace.generation_lock.write().await;
        let uri = params.text_document.uri.clone();
        let client_version = params.text_document.version;
        tracing::debug!(uri = %uri, version = client_version, change_count = params.content_changes.len(), "did_change");

        let changes: Vec<al_source::documents::TextChange> = params
            .content_changes
            .iter()
            .map(|c| al_source::documents::TextChange {
                range: c.range.map(|r| al_source::documents::TextRange {
                    start_line: r.start.line,
                    start_character: r.start.character,
                    end_line: r.end.line,
                    end_character: r.end.character,
                }),
                text: c.text.clone(),
            })
            .collect();
        // apply the changes and capture the resulting text under a
        // single write lock. A separate `apply_changes` + `get_text` pair would
        // leave a TOCTOU window where a concurrent `did_change` (tower-lsp can
        // interleave handlers under load) applies a later keystroke between the
        // mutation and the read, feeding a version-skewed snapshot into the
        // debounced diagnostics task.
        match self.workspace.documents.apply_versioned_changes_and_get(
            &uri,
            client_version,
            &changes,
        ) {
            Ok((text_arc, _version)) => {
                self.semantic_diagnostic_cache.lock().await.remove(&uri);
                al_workspace::on_document_change(&self.workspace, &uri, &text_arc);
                // Only schedule per-keystroke diagnostics when trigger is Continuous.
                // In OnSave mode, diagnostics are deferred to did_save to avoid per-keystroke work.
                let trigger = self.workspace.config.read().await.diagnostics_trigger;
                drop(generation);
                if trigger == al_project::config::DiagnosticsTrigger::Continuous {
                    self.schedule_diagnostics(uri).await;
                } else {
                    // Cancel any lingering debounced task from a previous Continuous session.
                    if let Some(old) = self.diag_task.lock().await.take() {
                        old.abort();
                    }
                }
            }
            Err(error) => {
                drop(generation);
                tracing::error!(uri = %uri, client_version, %error, "did_change rejected batch");
                let message_type = if matches!(
                    error,
                    al_source::documents::DocumentMutationError::StaleClientVersion { .. }
                ) {
                    MessageType::WARNING
                } else {
                    MessageType::ERROR
                };
                self.client
                    .show_message(
                        message_type,
                        format!(
                            "AL language server rejected an edit for {uri}: {error}. Reopen the file if editor and server content are no longer synchronized."
                        ),
                    )
                    .await;
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::info!(uri = %uri, "did_close");
        let generation = self.workspace.generation_lock.write().await;
        if !self.workspace.documents.close(&uri) {
            drop(generation);
            tracing::warn!(uri = %uri, "did_close received for a document that is not open");
            self.client
                .show_message(
                    MessageType::WARNING,
                    format!(
                        "AL language server received a close for a document that is not open: {uri}"
                    ),
                )
                .await;
            return;
        }
        self.semantic_diagnostic_cache.lock().await.remove(&uri);

        // Cancel any pending debounced diagnostics task. Without this, a task
        // armed by the last keystroke can wake after the close and publish
        // ghost squiggles. The in-task `contains` check is the primary guard;
        // aborting here also prevents unnecessary work.
        if let Some(old) = self.diag_task.lock().await.take() {
            old.abort();
        }

        // Evict only the composed object associated with this file.
        al_workspace::on_document_close(&self.workspace, &uri);

        if let Ok(path) = uri.to_file_path() {
            let scope = self.workspace.config.read().await.diagnostics_scope;
            // Disk I/O can be slow or blocked by an external filesystem. Do
            // not freeze didOpen/didChange while restoring the saved source.
            drop(generation);
            let read_path = path.clone();
            let disk_source = tokio::task::spawn_blocking(move || {
                al_source::file_index::read_source_file(&read_path)
            })
            .await;

            let generation = self.workspace.generation_lock.write().await;
            if self.workspace.documents.contains(&uri) {
                // The editor reopened this URI while the saved source was being
                // read. Its new overlay is authoritative; applying the stale
                // close generation would overwrite it and clear fresh diagnostics.
                drop(generation);
                tracing::debug!(uri = %uri, "did_close restore superseded by a later did_open");
                return;
            }

            let error_message = match disk_source {
                Ok(Ok(Some(source))) => {
                    // didClose discards the editor overlay. Restore the saved
                    // project generation instead of leaving unsaved text in
                    // workspace symbols, references, and diagnostics.
                    self.workspace.file_index.add_file(path, source);
                    // Invalidate the restored object's composed generation too;
                    // its identity can differ from the discarded overlay.
                    al_workspace::on_document_close(&self.workspace, &uri);
                    None
                }
                Ok(Ok(None)) => {
                    self.workspace.file_index.remove_file(&path);
                    al_workspace::on_document_close(&self.workspace, &uri);
                    None
                }
                Ok(Err(error)) => {
                    self.workspace.file_index.remove_file(&path);
                    al_workspace::on_document_close(&self.workspace, &uri);
                    tracing::error!(%error, "closed AL document could not be restored from disk");
                    Some(format!(
                        "Closed AL document was removed from the index because its saved source could not be read: {error}"
                    ))
                }
                Err(error) => {
                    self.workspace.file_index.remove_file(&path);
                    al_workspace::on_document_close(&self.workspace, &uri);
                    tracing::error!(%error, "closed-document restore worker failed");
                    Some(format!(
                        "Closed AL document was removed from the index because its restore worker failed: {error}"
                    ))
                }
            };

            // A transient URI is no longer part of the workspace, and even a
            // saved file may have become clean after discarding its overlay.
            self.client
                .publish_diagnostics(uri.clone(), vec![], None)
                .await;
            drop(generation);
            if let Some(message) = error_message {
                self.client.show_message(MessageType::ERROR, message).await;
            }
            if scope == al_project::config::DiagnosticsScope::Project {
                diagnostics::publish_workspace_diagnostics(self).await;
            }
        } else {
            drop(generation);
            self.client.publish_diagnostics(uri, vec![], None).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::debug!(uri = %uri, "did_save");

        // Always publish diagnostics on save — both Continuous and OnSave modes benefit
        // from a save-time refresh. In OnSave mode this is the *only* time diagnostics run
        // (did_change is gated by the trigger check above).
        if let Some((text, version)) = self.workspace.documents.get_text_and_client_version(&uri) {
            diagnostics::publish_diagnostics(self, &uri, text, version).await;
        } else {
            tracing::warn!(uri = %uri, "did_save: document not in store, skipping diagnostics");
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        tracing::info!("did_change_configuration");
        match self.await_ready().await {
            Ok(generation) => drop(generation),
            Err(error) => {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("AL configuration was not applied: {}", error.message),
                    )
                    .await;
                return;
            }
        }

        let al_settings = extract_al_settings(params.settings);
        let mut staged_config = self.workspace.config.read().await.clone();
        let old_cache_path = staged_config.package_cache_path.clone();
        let old_local_paths = staged_config.app_local_folder_paths.clone();
        let unknown = staged_config.merge(&al_settings);
        let symbol_paths_changed = old_cache_path != staged_config.package_cache_path
            || old_local_paths != staged_config.app_local_folder_paths;
        if !unknown.is_empty() {
            let msg = format!("Unknown AL settings: {}", unknown.join(", "));
            self.client.show_message(MessageType::WARNING, &msg).await;
        }
        if let Err(error) = self
            .workspace
            .documents
            .validate_max_doc_bytes(staged_config.max_document_size_bytes)
        {
            tracing::error!(%error, "configuration would invalidate an open document");
            self.client
                .show_message(
                    MessageType::ERROR,
                    format!(
                        "AL configuration was not applied because maxDocumentSizeBytes would invalidate an open document: {error}"
                    ),
                )
                .await;
            return;
        }

        if !symbol_paths_changed {
            let publication = self.workspace.generation_lock.write().await;
            *self.workspace.config.write().await = staged_config.clone();
            self.workspace
                .documents
                .set_max_doc_bytes(staged_config.max_document_size_bytes);
            self.semantic_diagnostic_cache.lock().await.clear();
            self.workspace.mark_generation_changed();
            drop(publication);
            tracing::info!("Configuration updated");
            self.refresh_diagnostics_after_configuration().await;
            return;
        }

        loop {
            let generation = self.workspace.generation_lock.read().await;
            let revision = self
                .workspace
                .generation_revision
                .load(std::sync::atomic::Ordering::Acquire);
            let project = self.workspace.project.read().await.clone();

            let Some(mut project) = project else {
                drop(generation);
                let publication = self.workspace.generation_lock.write().await;
                if self
                    .workspace
                    .generation_revision
                    .load(std::sync::atomic::Ordering::Acquire)
                    != revision
                {
                    drop(publication);
                    continue;
                }
                *self.workspace.config.write().await = staged_config.clone();
                self.workspace.mark_generation_changed();
                self.semantic_diagnostic_cache.lock().await.clear();
                drop(publication);
                self.workspace
                    .documents
                    .set_max_doc_bytes(staged_config.max_document_size_bytes);
                tracing::info!("Configuration updated (no active AL project)");
                self.refresh_diagnostics_after_configuration().await;
                return;
            };

            let worker_config = staged_config.clone();
            // Package parsing is staged optimistically. Release the live
            // generation before blocking work, then publish only if its
            // revision is still current.
            drop(generation);
            let staged = tokio::task::spawn_blocking(move || {
                project
                    .apply_symbol_settings(&worker_config)
                    .map_err(|error| error.to_string())?;
                let attempted = project.packages.len();
                let symbols = al_symbols::SymbolIndex::new();
                let cache = al_symbols::cache::SymbolCache::default_location();
                let loaded = symbols
                    .load_packages_cached(&project.packages, &cache)
                    .map_err(|error| error.to_string())?;
                symbols.load_runtime_enums();
                Ok::<_, String>((project, symbols, loaded, attempted))
            })
            .await;

            let (project, symbols, loaded, attempted) = match staged {
                Ok(Ok(staged)) => staged,
                Ok(Err(error)) => {
                    tracing::error!(%error, "configured symbol package generation rejected");
                    self.client
                        .show_message(
                            MessageType::ERROR,
                            format!(
                                "Failed to apply AL symbol package settings; the previous configuration and complete generation remain active: {error}"
                            ),
                        )
                        .await;
                    return;
                }
                Err(error) => {
                    tracing::error!(%error, "symbol package reload worker failed");
                    self.client
                        .show_message(
                            MessageType::ERROR,
                            format!(
                                "Failed to apply AL symbol package settings because the reload worker failed: {error}"
                            ),
                        )
                        .await;
                    return;
                }
            };

            let publication = self.workspace.generation_lock.write().await;
            if self
                .workspace
                .generation_revision
                .load(std::sync::atomic::Ordering::Acquire)
                != revision
            {
                drop(publication);
                continue;
            }

            self.workspace.symbols.replace_with(&symbols);
            *self.workspace.project.write().await = Some(project);
            *self.workspace.config.write().await = staged_config.clone();
            self.workspace.replace_package_info(
                loaded
                    .iter()
                    .map(|package| al_workspace::PackageInfo {
                        name: package.name.clone(),
                        publisher: package.publisher.clone(),
                        version: package.version.clone(),
                        object_count: package.object_count,
                    })
                    .collect(),
            );
            self.workspace.invalidate_insight_graph();
            self.workspace.mark_generation_changed();
            self.semantic_diagnostic_cache.lock().await.clear();
            let symbol_count = self.workspace.symbols.len();
            drop(publication);

            self.workspace
                .documents
                .set_max_doc_bytes(staged_config.max_document_size_bytes);
            tracing::info!(
                attempted,
                loaded = loaded.len(),
                symbols = symbol_count,
                "Configuration and symbol generation updated"
            );
            self.refresh_diagnostics_after_configuration().await;
            return;
        }
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        self.ensure_builtins_loaded().await?;
        let start = std::time::Instant::now();
        let result = hover::handle_hover(self, uri, position)
            .await
            .map_err(internal_error)?;
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "hover");
        Ok(result)
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        self.ensure_builtins_loaded().await?;
        let start = std::time::Instant::now();
        let result = completions::handle_completion(self, uri, position)
            .await
            .map_err(internal_error)?;
        let elapsed = start.elapsed();
        let count = result
            .as_ref()
            .map(|r| match r {
                CompletionResponse::Array(v) => v.len(),
                CompletionResponse::List(l) => l.items.len(),
            })
            .unwrap_or(0);
        tracing::debug!(uri = %uri, line = position.line, col = position.character, count, elapsed_us = elapsed.as_micros() as u64, "completion");
        Ok(result)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let start = std::time::Instant::now();
        let result = definition::handle_definition(self, uri, position).map_err(internal_error)?;
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "goto_definition");
        Ok(result)
    }

    async fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> Result<Option<GotoImplementationResponse>> {
        let _generation = self.await_ready().await?;
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let position = params.text_document_position_params.position;
        let workspace = Arc::clone(&self.workspace);
        let locations = tokio::task::spawn_blocking(move || {
            al_analysis::queries::implementation::find_implementations(
                &workspace,
                &uri,
                position.into(),
            )
            .into_iter()
            .map(Into::into)
            .collect::<Vec<Location>>()
        })
        .await
        .map_err(|error| internal_error(format!("go-to-implementation worker failed: {error}")))?;

        if locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(GotoImplementationResponse::Array(locations)))
        }
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let _generation = self.await_ready().await?;
        let uri = params.text_document_position.text_document.uri.clone();
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let start = std::time::Instant::now();

        // run the synchronous reference walk inside `spawn_blocking` so the
        // tokio task can be dropped (via tower-lsp's $/cancelRequest handling)
        // without waiting for the walk to finish. Without this wrapper a pending
        // references query on a large workspace blocks the async task until it
        // completes — the LSP client appears responsive (tower-lsp drops the
        // future) but the CPU is wasted.
        let workspace = Arc::clone(&self.workspace);
        let uri_for_log = uri.clone();
        let result = tokio::task::spawn_blocking(move || {
            let core_pos = position.into();
            let locations = al_analysis::queries::references::references(
                &workspace,
                &uri,
                core_pos,
                include_declaration,
            )
            .map_err(|error| error.to_string())?;
            if locations.is_empty() {
                Ok::<Option<Vec<Location>>, String>(None)
            } else {
                Ok::<Option<Vec<Location>>, String>(Some(
                    locations
                        .into_iter()
                        .map(Into::into)
                        .collect::<Vec<Location>>(),
                ))
            }
        })
        .await
        .map_err(|error| internal_error(format!("references worker failed: {error}")))?
        .map_err(internal_error)?;

        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri_for_log, line = position.line, col = position.character, count, elapsed_us = elapsed.as_micros() as u64, "references");
        Ok(result)
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let _generation = self.await_ready().await?;
        let uri = params.text_document.uri.clone();
        let start = std::time::Instant::now();
        // spawn_blocking for cancel-friendliness on large files.
        let workspace = Arc::clone(&self.workspace);
        let uri_for_log = uri.clone();
        let hierarchical = self.document_symbol_hierarchical.load(Ordering::Relaxed);
        let result = tokio::task::spawn_blocking(move || {
            al_analysis::queries::symbols::document_symbols(&workspace, &uri).map(|symbols| {
                if hierarchical {
                    DocumentSymbolResponse::Nested(symbols.into_iter().map(Into::into).collect())
                } else {
                    DocumentSymbolResponse::Flat(al_analysis::lsp::flatten_document_symbols(
                        symbols, &uri,
                    ))
                }
            })
        })
        .await
        .map_err(|error| internal_error(format!("document-symbol worker failed: {error}")))?;
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri_for_log, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "document_symbol");
        Ok(result)
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let formatting_config = self.workspace.config.read().await.formatting.clone();
        let result = formatting::handle_formatting_with_config(
            self,
            uri,
            &params.options,
            &formatting_config,
        );
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, edits = count, elapsed_us = elapsed.as_micros() as u64, "formatting");
        Ok(result)
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let formatting_config = self.workspace.config.read().await.formatting.clone();
        let result = formatting::handle_range_formatting_with_config(
            self,
            uri,
            params.range,
            &params.options,
            &formatting_config,
        );
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, edits = count, elapsed_us = elapsed.as_micros() as u64, "range_formatting");
        Ok(result)
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let result = handlers::handle_folding_range(self, uri);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, ranges = count, elapsed_us = elapsed.as_micros() as u64, "folding_range");
        Ok(result)
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let _generation = self.await_ready().await?;
        let uri = params.text_document.uri.clone();
        let start = std::time::Instant::now();
        // spawn_blocking — semantic_tokens_full traverses the entire
        // tree-sitter tree on big AL files; cancellation-friendliness matters.
        let workspace = Arc::clone(&self.workspace);
        let uri_for_log = uri.clone();
        let result = tokio::task::spawn_blocking(move || {
            let tokens =
                al_analysis::queries::semantic_tokens::semantic_tokens_full(&workspace, &uri);
            if tokens.is_empty() {
                return None;
            }
            let lsp_tokens: Vec<SemanticToken> = tokens
                .into_iter()
                .map(|t| SemanticToken {
                    delta_line: t.delta_line,
                    delta_start: t.delta_start,
                    length: t.length,
                    token_type: t.token_type,
                    token_modifiers_bitset: t.token_modifiers,
                })
                .collect();
            Some(SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data: lsp_tokens,
            }))
        })
        .await
        .map_err(|error| internal_error(format!("semantic-tokens worker failed: {error}")))?;
        let count = result
            .as_ref()
            .map(|r| match r {
                SemanticTokensResult::Tokens(t) => t.data.len(),
                SemanticTokensResult::Partial(t) => t.data.len(),
            })
            .unwrap_or(0);
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri_for_log, tokens = count, elapsed_us = elapsed.as_micros() as u64, "semantic_tokens_full");
        Ok(result)
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let start = std::time::Instant::now();
        let result =
            handlers::handle_signature_help(self, uri, position).map_err(internal_error)?;
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "signature_help");
        Ok(result)
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document.uri;
        let range = params.range;
        let diagnostics = &params.context.diagnostics;
        let start = std::time::Instant::now();
        let result = handlers::handle_code_action(self, uri, range, diagnostics);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, actions = count, elapsed_us = elapsed.as_micros() as u64, "code_action");
        Ok(result)
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> Result<DocumentDiagnosticReportResult> {
        let generation = self.await_ready().await?;
        let revision = self.workspace.generation_revision();
        let uri = &params.text_document.uri;

        // skip diagnostics for virtual symbol cache files.
        if diagnostics::is_cache_path(uri) {
            tracing::debug!(uri = %uri, "diagnostic (pull): skipping cache file");
            return Ok(diagnostics::full_diagnostic_report(vec![]));
        }

        let (text, client_version) = match self.workspace.documents.get_text_and_client_version(uri)
        {
            Some(snapshot) => snapshot,
            None => {
                tracing::debug!(uri = %uri, "diagnostic (pull): document not open, returning empty");
                return Ok(diagnostics::full_diagnostic_report(vec![]));
            }
        };
        drop(generation);

        let start = std::time::Instant::now();
        let diags = diagnostics::compute_diagnostics(self, uri, &text).await;
        let elapsed = start.elapsed();
        let generation = self.workspace.generation_lock.read().await;
        let snapshot_current = self
            .workspace
            .documents
            .get_text_and_client_version(uri)
            .is_some_and(|(current_text, current_version)| {
                current_version == client_version && Arc::ptr_eq(&current_text, &text)
            });
        if self.workspace.generation_revision() != revision || !snapshot_current {
            drop(generation);
            return Err(content_modified_error());
        }
        drop(generation);
        tracing::debug!(uri = %uri, count = diags.len(), elapsed_us = elapsed.as_micros() as u64, "diagnostic (pull)");

        Ok(diagnostics::full_diagnostic_report(diags))
    }

    async fn workspace_diagnostic(
        &self,
        _params: WorkspaceDiagnosticParams,
    ) -> Result<WorkspaceDiagnosticReportResult> {
        // Project-scope pull diagnostics aggregate parse and syntax errors
        // across every indexed file plus bridge diagnostics for open documents.
        let generation = self.await_ready().await?;
        let revision = self.workspace.generation_revision();
        drop(generation);
        let start = std::time::Instant::now();
        let reports = diagnostics::compute_workspace_diagnostics(self)
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let generation = self.workspace.generation_lock.read().await;
        if self.workspace.generation_revision() != revision {
            drop(generation);
            return Err(content_modified_error());
        }
        drop(generation);
        let file_count = reports.len();
        let items = reports
            .into_iter()
            .map(|(uri, version, items)| {
                WorkspaceDocumentDiagnosticReport::Full(WorkspaceFullDocumentDiagnosticReport {
                    uri,
                    version,
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id: None,
                        items,
                    },
                })
            })
            .collect();
        let elapsed = start.elapsed();
        tracing::debug!(
            file_count,
            elapsed_us = elapsed.as_micros() as u64,
            "workspace_diagnostic (pull)"
        );
        Ok(WorkspaceDiagnosticReportResult::Report(
            WorkspaceDiagnosticReport { items },
        ))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name.clone();
        let start = std::time::Instant::now();
        let result = definition::handle_rename(self, uri, position, params.new_name)
            .map_err(internal_error)?;
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, new_name = %new_name, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "rename");
        Ok(result)
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document.uri;
        let position = params.position;
        let start = std::time::Instant::now();
        let result = definition::handle_prepare_rename(self, uri, position);
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "prepare_rename");
        Ok(result)
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let _generation = self.await_ready().await?;
        let start = std::time::Instant::now();
        let result = workspace::handle_workspace_symbol(self, &params.query);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(query = %params.query, count, elapsed_us = elapsed.as_micros() as u64, "workspace_symbol");
        Ok(result)
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document.uri;
        let range = params.range;
        self.ensure_builtins_loaded().await?;
        let start = std::time::Instant::now();
        let result = handlers::handle_inlay_hint(self, uri, range).map_err(internal_error)?;
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, hints = count, elapsed_us = elapsed.as_micros() as u64, "inlay_hint");
        Ok(result)
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let _generation = self.await_ready().await?;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let entries = al_analysis::queries::code_lens::code_lens(&self.workspace, uri)
            .map_err(internal_error)?;
        let elapsed = start.elapsed();
        let count = entries.len();
        tracing::debug!(uri = %uri, lenses = count, elapsed_us = elapsed.as_micros() as u64, "code_lens");
        if entries.is_empty() {
            return Ok(None);
        }
        use al_analysis::queries::code_lens::CodeLensKind;
        let lenses: Vec<CodeLens> = entries
            .into_iter()
            .map(|e| {
                let command_id = e.kind.command_id();
                let data = match &e.kind {
                    CodeLensKind::Reference(count) => {
                        serde_json::json!({ "kind": "reference", "count": count })
                    }
                    CodeLensKind::Profiler(label) => {
                        serde_json::json!({ "kind": "profiler", "label": label })
                    }
                    CodeLensKind::Test(status) => {
                        serde_json::json!({ "kind": "test", "status": status })
                    }
                };
                let lsp_range: Range = e.range.into();
                let arguments = match &e.kind {
                    // Position the handler at the lens's symbol so it can run
                    // the reference search / resolve the profiled procedure.
                    CodeLensKind::Reference(_) | CodeLensKind::Profiler(_) => Some(vec![
                        serde_json::json!({ "uri": uri, "position": lsp_range.start }),
                    ]),
                    // `al.runTest` needs the codeunit/method, not a position.
                    CodeLensKind::Test(_) => e
                        .test_target
                        .as_ref()
                        .and_then(|t| serde_json::to_value(t).ok())
                        .map(|v| vec![v]),
                };
                CodeLens {
                    range: lsp_range,
                    command: Some(Command {
                        title: e.title,
                        command: command_id.to_string(),
                        arguments,
                    }),
                    data: Some(data),
                }
            })
            .collect();
        Ok(Some(lenses))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        tracing::info!(command = %params.command, "execute_command");
        let mut generation = Some(self.await_ready().await?);
        if matches!(
            params.command.as_str(),
            "al.downloadSymbols"
                | "al.downloadSymbolsNuget"
                | "al.downloadSymbolsServer"
                | "al.reindex"
        ) {
            // These commands acquire their own read/write guards while staging
            // and publishing a replacement generation.
            drop(generation.take());
        }

        let start = std::time::Instant::now();
        let result = match params.command.as_str() {
            "al.downloadSymbols" | "al.downloadSymbolsNuget" => {
                workspace::download_symbols_command(self, workspace::DownloadSource::NuGet).await;
                Ok(None)
            }
            "al.downloadSymbolsServer" => {
                workspace::download_symbols_command(self, workspace::DownloadSource::Server).await;
                Ok(None)
            }
            "al.clearSymbolCache" => {
                commands::clear_symbol_cache(self).await;
                Ok(None)
            }
            "al.formatFile" => {
                commands::format_file(self, &params.arguments).await;
                Ok(None)
            }
            "al.lintFile" => {
                commands::lint_file(self, &params.arguments).await;
                Ok(None)
            }
            "al.getStatus" => Ok(Some(
                commands::get_status(self).await.map_err(internal_error)?,
            )),
            "al.reindex" => {
                commands::reindex(self).await;
                Ok(None)
            }
            "al.compile" => {
                commands::compile(self).await;
                Ok(None)
            }
            "al.applyRecommendedSettings" => {
                commands::apply_recommended_settings(self).await;
                Ok(None)
            }
            // Each CodeLens-backed command returns `Some(..)` so a click
            // performs the action instead of silently hitting the catch-all.
            "al.findReferences" => Ok(Some(
                commands::find_references(self, &params.arguments).map_err(internal_error)?,
            )),
            "al.showProfiler" => Ok(Some(
                commands::show_profiler(self, &params.arguments).map_err(internal_error)?,
            )),
            "al.runTest" => Ok(Some(commands::run_test(self, &params.arguments).await)),
            _ => {
                tracing::warn!(command = %params.command, "Unknown command");
                Ok(None)
            }
        };
        let elapsed = start.elapsed();
        tracing::debug!(command = %params.command, elapsed_us = elapsed.as_micros() as u64, "execute_command");
        result
    }
}

pub async fn run_lsp() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::build(AlServer::new)
        .custom_method("experimental/runnables", AlServer::runnables)
        .finish();
    Server::new(stdin, stdout, socket).serve(service).await;
}

fn resolve_explorer_binary() -> std::result::Result<std::path::PathBuf, String> {
    if let Some(explicit) = std::env::var_os("AL_EXPLORER_PATH") {
        let path = std::path::PathBuf::from(explicit);
        if path.as_os_str().is_empty() {
            return Err("AL_EXPLORER_PATH is empty".to_string());
        }
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            return Ok(path);
        }
        return Err(format!(
            "AL_EXPLORER_PATH does not identify a file: {}",
            path.display()
        ));
    }

    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate the running al-lsp executable: {error}"))?;
    let binary_name = if cfg!(windows) {
        "al-explorer.exe"
    } else {
        "al-explorer"
    };
    let parent = executable
        .parent()
        .ok_or_else(|| format!("al-lsp executable has no parent: {}", executable.display()))?;
    let candidates = [
        parent.join(binary_name),
        parent
            .parent()
            .map(|directory| directory.join(binary_name))
            .unwrap_or_default(),
    ];
    candidates
        .into_iter()
        .find(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()))
        .ok_or_else(|| {
            format!(
                "the al-explorer sidecar was not found beside {} (set AL_EXPLORER_PATH explicitly)",
                executable.display()
            )
        })
}

fn build_runnables(
    uri: &Url,
    file_path: &std::path::Path,
    cwd: &std::path::Path,
    explorer: &std::path::Path,
    has_project: bool,
    test_codeunits: &[al_analysis::queries::tests::TestCodeunit],
    _position: Option<Position>,
) -> Vec<Runnable> {
    let program = explorer.to_string_lossy().into_owned();
    let shell = |label: String, args: Vec<String>, location: Option<LocationLink>| Runnable {
        label,
        location,
        kind: "shell",
        args: ShellRunnableArgs {
            environment: std::collections::HashMap::new(),
            cwd: cwd.to_path_buf(),
            program: program.clone(),
            args,
        },
    };

    let mut runnables = Vec::new();
    if has_project {
        runnables.push(shell(
            "AL: Compile project".to_string(),
            vec![
                "compile".to_string(),
                "--project".to_string(),
                cwd.display().to_string(),
            ],
            None,
        ));
    }
    runnables.push(shell(
        "AL: Lint current file".to_string(),
        vec!["lint".to_string(), file_path.display().to_string()],
        None,
    ));

    for codeunit in test_codeunits
        .iter()
        .filter(|codeunit| paths_equivalent(std::path::Path::new(&codeunit.file), file_path))
    {
        for test in &codeunit.tests {
            // Test discovery reports human-readable 1-based lines. LSP ranges
            // are zero-based, including the custom Zed runnable location.
            let line = test.line.saturating_sub(1);
            let range = Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 0 },
            };
            runnables.push(shell(
                format!("AL: Test {}.{}", codeunit.name, test.name),
                vec![
                    "test-run".to_string(),
                    codeunit.id.to_string(),
                    "--name".to_string(),
                    codeunit.name.clone(),
                    "--method".to_string(),
                    test.name.clone(),
                ],
                Some(LocationLink {
                    origin_selection_range: None,
                    target_uri: uri.clone(),
                    target_range: range,
                    target_selection_range: range,
                }),
            ));
        }
    }
    runnables
}

fn paths_equivalent(left: &std::path::Path, right: &std::path::Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod runnables_tests {
    use super::*;
    use al_analysis::queries::tests::{TestCodeunit, TestProcedure};

    #[test]
    fn emits_zed_shell_runnables_for_project_file_and_discovered_tests() {
        let root = tempfile::tempdir().expect("temporary project");
        let file = root.path().join("CustomerTests.Codeunit.AL");
        std::fs::write(&file, "codeunit 50100 CustomerTests {}").expect("test file");
        let explorer = root.path().join(if cfg!(windows) {
            "al-explorer.exe"
        } else {
            "al-explorer"
        });
        let uri = Url::from_file_path(&file).expect("file URI");
        let tests = vec![
            TestCodeunit {
                name: "Customer Tests".to_string(),
                id: 50100,
                file: file.to_string_lossy().into_owned(),
                tests: vec![TestProcedure {
                    name: "CreatesCustomer".to_string(),
                    line: 7,
                    handler_functions: Vec::new(),
                }],
                test_initializers: Vec::new(),
                test_cleanups: Vec::new(),
            },
            TestCodeunit {
                name: "Other Tests".to_string(),
                id: 50101,
                file: root
                    .path()
                    .join("OtherTests.Codeunit.al")
                    .to_string_lossy()
                    .into_owned(),
                tests: vec![TestProcedure {
                    name: "DoesNotBelongToThisFile".to_string(),
                    line: 3,
                    handler_functions: Vec::new(),
                }],
                test_initializers: Vec::new(),
                test_cleanups: Vec::new(),
            },
        ];

        let runnables = build_runnables(&uri, &file, root.path(), &explorer, true, &tests, None);
        assert_eq!(runnables.len(), 3);
        assert_eq!(runnables[0].label, "AL: Compile project");
        assert_eq!(
            runnables[0].args.args,
            [
                "compile",
                "--project",
                root.path().to_string_lossy().as_ref()
            ]
        );
        assert_eq!(runnables[1].label, "AL: Lint current file");
        assert_eq!(
            runnables[1].args.args,
            ["lint", file.to_string_lossy().as_ref()]
        );
        assert_eq!(
            runnables[2].label,
            "AL: Test Customer Tests.CreatesCustomer"
        );
        assert_eq!(
            runnables[2].args.args,
            [
                "test-run",
                "50100",
                "--name",
                "Customer Tests",
                "--method",
                "CreatesCustomer"
            ]
        );
        let location = runnables[2].location.as_ref().expect("test location");
        assert_eq!(location.target_selection_range.start.line, 6);

        let json = serde_json::to_value(&runnables[2]).expect("runnable JSON");
        assert_eq!(json["kind"], "shell");
        assert_eq!(json["args"]["environment"], serde_json::json!({}));
        assert_eq!(json["args"]["program"], explorer.to_string_lossy().as_ref());
        assert_eq!(json["args"]["cwd"], root.path().to_string_lossy().as_ref());
    }

    #[test]
    fn omits_project_compile_runnable_without_a_project() {
        let runnables = build_runnables(
            &Url::parse("file:///tmp/Standalone.al").unwrap(),
            std::path::Path::new("/tmp/Standalone.al"),
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tools/al-explorer"),
            false,
            &[],
            None,
        );
        assert_eq!(runnables.len(), 1);
        assert_eq!(runnables[0].label, "AL: Lint current file");
    }
}

#[cfg(test)]
mod workspace_init_state_tests {
    use super::*;

    #[tokio::test]
    async fn ready_state_releases_requests() {
        let (service, _socket) = LspService::new(AlServer::new);
        service
            .inner()
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let _generation = service
            .inner()
            .await_ready()
            .await
            .expect("ready workspace");
    }

    #[tokio::test]
    async fn failed_state_returns_the_retained_initialization_error() {
        let (service, _socket) = LspService::new(AlServer::new);
        service
            .inner()
            .workspace_init_state
            .send_replace(WorkspaceInitState::Failed(
                "configured package directory is unreadable".to_string(),
            ));

        let error = service
            .inner()
            .await_ready()
            .await
            .expect_err("failed initialization must fail requests");
        assert_eq!(error.code, tower_lsp::jsonrpc::ErrorCode::InternalError);
        assert!(error
            .message
            .contains("configured package directory is unreadable"));
    }

    #[tokio::test]
    async fn semantic_phase_waits_for_the_initialized_workspace_generation() {
        let (service, _socket) = LspService::new(AlServer::new);
        let state = service.inner().workspace_init_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            state.send_replace(WorkspaceInitState::Ready);
        });

        service
            .inner()
            .await_semantic_workspace()
            .await
            .expect("semantic phase should resume after workspace readiness");
    }
}

#[cfg(test)]
mod implementation_capability_tests {
    use super::*;

    #[tokio::test]
    async fn native_lsp_advertises_and_serves_go_to_implementation() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        let interface_uri = Url::parse("file:///proj/IFoo.Interface.al").unwrap();
        server
            .workspace
            .documents
            .open(
                interface_uri.clone(),
                "interface 50100 IFoo\n{\n    procedure Run();\n}\n".to_string(),
            )
            .unwrap();
        server.workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/FooImpl.Codeunit.al"),
            "codeunit 50101 FooImpl implements IFoo\n{\n    procedure Run()\n    begin\n    end;\n}\n"
                .to_string(),
        );

        let initialized = server
            .initialize(InitializeParams::default())
            .await
            .expect("initialize succeeds");
        assert_eq!(
            initialized.capabilities.implementation_provider,
            Some(ImplementationProviderCapability::Simple(true))
        );

        let response = server
            .goto_implementation(GotoImplementationParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: interface_uri },
                    position: Position {
                        line: 0,
                        character: "interface 50100 ".len() as u32,
                    },
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .expect("go-to-implementation succeeds")
            .expect("implementation exists");

        let locations = match response {
            GotoImplementationResponse::Scalar(location) => vec![location],
            GotoImplementationResponse::Array(locations) => locations,
            GotoImplementationResponse::Link(_) => {
                panic!("the native handler returns locations, not location links")
            }
        };
        assert_eq!(locations.len(), 1);
        assert!(locations[0].uri.path().ends_with("/FooImpl.Codeunit.al"));
    }
}

#[cfg(test)]
mod document_symbol_capability_tests {
    //! `document_symbol` must honour the client's
    //! `hierarchicalDocumentSymbolSupport` capability: nested `DocumentSymbol[]`
    //! when advertised, flat `SymbolInformation[]` otherwise. These drive the
    //! real handler in-process (no transport) through `initialize`, so they
    //! cover both the capability capture and the response-shape branch.

    use super::*;

    const SRC: &str =
        "codeunit 50100 \"Outline CU\"\n{\n    procedure DoWork()\n    begin\n    end;\n}\n";

    /// Build an in-process server with the document open and `await_ready`
    /// short-circuited, then run `initialize` with the given client capabilities.
    async fn server_after_initialize(caps: ClientCapabilities) -> (LspService<AlServer>, Url) {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        // Skip the 30s workspace-init wait in `await_ready`.
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let uri = Url::parse("file:///proj/Outline.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string())
            .unwrap();
        server
            .initialize(InitializeParams {
                capabilities: caps,
                ..Default::default()
            })
            .await
            .expect("initialize succeeds");
        (service, uri)
    }

    fn ds_params(uri: &Url) -> DocumentSymbolParams {
        DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[tokio::test]
    async fn nested_when_client_advertises_hierarchical_support() {
        let caps = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                document_symbol: Some(DocumentSymbolClientCapabilities {
                    hierarchical_document_symbol_support: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (service, uri) = server_after_initialize(caps).await;

        match service.inner().document_symbol(ds_params(&uri)).await {
            Ok(Some(DocumentSymbolResponse::Nested(syms))) => {
                let object = &syms[0];
                let children = object
                    .children
                    .as_ref()
                    .expect("object with a procedure has nested children");
                assert!(
                    children.iter().any(|c| c.name.contains("DoWork")),
                    "nested child procedure should be present, got {children:?}"
                );
            }
            other => panic!("expected Nested response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn flat_when_client_omits_hierarchical_support() {
        // Empty `documentSymbol` capability == no hierarchical support advertised.
        let caps = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                document_symbol: Some(DocumentSymbolClientCapabilities::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (service, uri) = server_after_initialize(caps).await;

        match service.inner().document_symbol(ds_params(&uri)).await {
            Ok(Some(DocumentSymbolResponse::Flat(infos))) => {
                // The procedure is flattened out with its object as container.
                let proc = infos
                    .iter()
                    .find(|s| s.name.contains("DoWork"))
                    .expect("procedure present in flat list");
                assert_eq!(
                    proc.container_name.as_deref(),
                    Some("Outline CU"),
                    "flattened child records its parent object as container_name"
                );
            }
            other => panic!("expected Flat response, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod definition_link_support_tests {
    //! `goto_definition` must honour the client's `definition.linkSupport`
    //! capability: `LocationLink[]` when advertised, plain `Location[]`
    //! otherwise. Driven in-process through `initialize`, so they cover both the
    //! capability capture and the response-shape branch. (The integration
    //! harness only exercises the `Location[]` side.)

    use super::*;

    // A local variable used after its declaration — go-to-definition on the use
    // resolves intra-file to the declaration, needing only an open document.
    const SRC: &str = "codeunit 50100 \"Test\"\n{\n    procedure Foo()\n    var\n        MyVar: Integer;\n    begin\n        MyVar := 42;\n    end;\n}\n";

    async fn server_after_initialize(caps: ClientCapabilities) -> (LspService<AlServer>, Url) {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let uri = Url::parse("file:///proj/Def.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string())
            .unwrap();
        server
            .initialize(InitializeParams {
                capabilities: caps,
                ..Default::default()
            })
            .await
            .expect("initialize succeeds");
        (service, uri)
    }

    fn def_params(uri: &Url) -> GotoDefinitionParams {
        // The `MyVar` use on line 6 (0-based), column 8.
        GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position {
                    line: 6,
                    character: 8,
                },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[tokio::test]
    async fn link_response_when_client_advertises_link_support() {
        let caps = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                definition: Some(GotoCapability {
                    link_support: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (service, uri) = server_after_initialize(caps).await;

        match service.inner().goto_definition(def_params(&uri)).await {
            Ok(Some(GotoDefinitionResponse::Link(links))) => {
                assert!(!links.is_empty(), "expected at least one LocationLink");
            }
            other => panic!("expected Link response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn array_response_when_client_omits_link_support() {
        // Empty `definition` capability == no linkSupport advertised.
        let caps = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                definition: Some(GotoCapability::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (service, uri) = server_after_initialize(caps).await;

        match service.inner().goto_definition(def_params(&uri)).await {
            Ok(Some(GotoDefinitionResponse::Array(locs))) => {
                assert!(!locs.is_empty(), "expected at least one Location");
            }
            other => panic!("expected Array response, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod code_lens_command_wiring_tests {
    //! every CodeLens the server emits must resolve to an
    //! `executeCommand` handler — a clicked lens must perform its action, never
    //! a silent no-op. These tests drive the real `code_lens` + `execute_command`
    //! handlers in-process (no transport) and assert both the structural
    //! invariant (`LENS_COMMAND_IDS ⊆ SUPPORTED_COMMANDS`) and end-to-end that
    //! each emitted lens is dispatched (returns `Some`, not the catch-all's
    //! `None`).

    use super::*;
    use al_analysis::queries::code_lens::LENS_COMMAND_IDS;
    use al_analysis::queries::profiler_hints::{ProfilerHint, ProfilerSession};

    // A test codeunit exercising all three lens kinds: TestBeta is called once
    // (reference lens), both methods are `[Test]` (test lenses), and a profiler
    // session below adds a profiler lens for TestAlpha.
    const SRC: &str = "codeunit 50200 \"My Tests\"\n{\n    Subtype = Test;\n\n    [Test]\n    procedure TestAlpha()\n    begin\n        TestBeta();\n    end;\n\n    [Test]\n    procedure TestBeta()\n    begin\n    end;\n}\n";

    fn build_server() -> (LspService<AlServer>, Url) {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        // Skip the workspace-init wait in `await_ready`.
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let uri = Url::parse("file:///proj/MyTests.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string())
            .unwrap();
        (service, uri)
    }

    fn add_profiler_session(server: &AlServer, uri: &Url, procedure: &str) {
        let file_path = uri.to_file_path().unwrap().to_string_lossy().to_string();
        let hint = ProfilerHint {
            procedure: procedure.to_string(),
            object: "My Tests".to_string(),
            self_time_ms: 42.0,
            total_time_ms: 42.0,
            hit_count: 3,
            file: Some(file_path.clone()),
            line: None,
        };
        *server
            .workspace
            .profiler_session
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(ProfilerSession::new(file_path, vec![hint]));
    }

    fn lens_params(uri: &Url) -> CodeLensParams {
        CodeLensParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    fn exec_params(cmd: &Command) -> ExecuteCommandParams {
        ExecuteCommandParams {
            command: cmd.command.clone(),
            arguments: cmd.arguments.clone().unwrap_or_default(),
            work_done_progress_params: Default::default(),
        }
    }

    #[test]
    fn lens_command_ids_are_all_supported() {
        for id in LENS_COMMAND_IDS {
            assert!(
                SUPPORTED_COMMANDS.contains(id),
                "lens command id {id:?} is not advertised or handled in SUPPORTED_COMMANDS"
            );
        }
    }

    #[tokio::test]
    async fn every_emitted_lens_is_dispatched() {
        let (service, uri) = build_server();
        let server = service.inner();
        add_profiler_session(server, &uri, "TestAlpha");

        let lenses = server
            .code_lens(lens_params(&uri))
            .await
            .expect("code_lens ok")
            .expect("expected lenses");
        assert!(!lenses.is_empty(), "fixture should emit lenses");

        // All three lens kinds must be represented so this exercises every arm.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for lens in &lenses {
            let cmd = lens.command.as_ref().expect("lens carries a command");
            assert!(
                SUPPORTED_COMMANDS.contains(&cmd.command.as_str()),
                "emitted lens command {:?} is not handled",
                cmd.command
            );
            seen.insert(match cmd.command.as_str() {
                "al.findReferences" => "ref",
                "al.showProfiler" => "prof",
                "al.runTest" => "test",
                other => panic!("unexpected lens command {other}"),
            });
            // The dispatcher must return Some(..) for every emitted lens; the
            // unknown-command catch-all returns None.
            let result = server
                .execute_command(exec_params(cmd))
                .await
                .expect("execute_command ok");
            assert!(
                result.is_some(),
                "command {:?} fell through to the no-op catch-all",
                cmd.command
            );
        }
        assert!(seen.contains("ref"), "expected a reference lens");
        assert!(seen.contains("prof"), "expected a profiler lens");
        assert!(seen.contains("test"), "expected a test lens");
    }

    #[tokio::test]
    async fn find_references_command_returns_locations() {
        let (service, uri) = build_server();
        let server = service.inner();

        // Grab the reference lens for TestBeta (called once) and run its command.
        let lenses = server
            .code_lens(lens_params(&uri))
            .await
            .expect("ok")
            .expect("lenses");
        let ref_cmd = lenses
            .iter()
            .filter_map(|l| l.command.as_ref())
            .find(|c| c.command == "al.findReferences" && c.title.contains("1 reference"))
            .expect("a '1 reference' lens for TestBeta");

        let result = server
            .execute_command(exec_params(ref_cmd))
            .await
            .expect("ok")
            .expect("findReferences returns a value");
        let arr = result.as_array().expect("locations array");
        assert!(
            !arr.is_empty(),
            "expected at least one reference location, got {result:?}"
        );
        // Each entry must be a real LSP Location (has uri + range).
        assert!(arr[0].get("uri").is_some() && arr[0].get("range").is_some());
    }

    #[tokio::test]
    async fn show_profiler_command_returns_active_session() {
        let (service, uri) = build_server();
        let server = service.inner();
        add_profiler_session(server, &uri, "TestAlpha");

        let params = ExecuteCommandParams {
            command: "al.showProfiler".to_string(),
            arguments: vec![serde_json::json!({ "uri": uri })],
            work_done_progress_params: Default::default(),
        };
        let result = server
            .execute_command(params)
            .await
            .expect("ok")
            .expect("showProfiler returns a value");
        assert_eq!(result["active"], serde_json::json!(true));
        let hints = result["hints"].as_array().expect("hints array");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0]["procedure"], serde_json::json!("TestAlpha"));
    }

    #[tokio::test]
    async fn run_test_without_server_reports_no_server() {
        let (service, _uri) = build_server();
        let server = service.inner();
        // No project / launch config loaded → routing must report `noServer`
        // (not a silent no-op) and never attempt a BC round-trip.
        let params = ExecuteCommandParams {
            command: "al.runTest".to_string(),
            arguments: vec![serde_json::json!({
                "codeunitId": 50200,
                "methodName": "TestAlpha",
            })],
            work_done_progress_params: Default::default(),
        };
        let result = server
            .execute_command(params)
            .await
            .expect("ok")
            .expect("runTest returns a value");
        assert_eq!(result["status"], serde_json::json!("noServer"));
        assert_eq!(
            result["target"]["methodName"],
            serde_json::json!("TestAlpha")
        );
    }

    #[tokio::test]
    async fn run_test_with_no_target_reports_invalid_args() {
        let (service, _uri) = build_server();
        let server = service.inner();
        let params = ExecuteCommandParams {
            command: "al.runTest".to_string(),
            arguments: vec![],
            work_done_progress_params: Default::default(),
        };
        let result = server
            .execute_command(params)
            .await
            .expect("ok")
            .expect("runTest returns a value");
        assert_eq!(result["status"], serde_json::json!("invalidArgs"));
    }

    #[tokio::test]
    async fn unknown_command_falls_through_to_none() {
        // Confirms the `Some(..)` assertions above are meaningful: a genuinely
        // unhandled command still hits the catch-all and returns None.
        let (service, _uri) = build_server();
        let server = service.inner();
        let params = ExecuteCommandParams {
            command: "al.thisDoesNotExist".to_string(),
            arguments: vec![],
            work_done_progress_params: Default::default(),
        };
        let result = server.execute_command(params).await.expect("ok");
        assert!(result.is_none(), "unknown command must return None");
    }
}

#[cfg(test)]
mod workspace_diagnostic_tests {
    //! `workspace/diagnostic` must report parse/syntax errors across the
    //! whole workspace — both background (never-opened) files and open documents
    //! — and report nothing for a clean workspace. Driven in-process against the
    //! real `AlServer` handler (no transport, no toolchain ⇒ bridge is a no-op,
    //! so these assert the syntax-pass aggregation).

    use super::*;

    const BAD_SRC: &str = "codeunit 50100 Test\n{\n    procedure Broken(\n    begin\n    end;\n}\n";
    const GOOD_SRC: &str =
        "codeunit 50100 MyCodeunit\n{\n    trigger OnRun()\n    begin\n    end;\n}\n";
    const GOOD_BG_SRC: &str =
        "codeunit 50101 OtherCodeunit\n{\n    trigger OnRun()\n    begin\n    end;\n}\n";

    fn new_ready_server() -> LspService<AlServer> {
        let (service, _socket) = LspService::new(AlServer::new);
        // Skip the 30s workspace-init wait in `await_ready`.
        service
            .inner()
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        service
    }

    fn ws_diag_params() -> WorkspaceDiagnosticParams {
        WorkspaceDiagnosticParams {
            identifier: None,
            previous_result_ids: vec![],
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    /// Pull the per-file reports out of a workspace diagnostic result.
    fn reports(
        result: WorkspaceDiagnosticReportResult,
    ) -> Vec<WorkspaceFullDocumentDiagnosticReport> {
        match result {
            WorkspaceDiagnosticReportResult::Report(r) => r
                .items
                .into_iter()
                .map(|item| match item {
                    WorkspaceDocumentDiagnosticReport::Full(f) => f,
                    WorkspaceDocumentDiagnosticReport::Unchanged(_) => {
                        panic!("did not expect an Unchanged report")
                    }
                })
                .collect(),
            WorkspaceDiagnosticReportResult::Partial(_) => {
                panic!("did not expect a Partial result")
            }
        }
    }

    #[tokio::test]
    async fn reports_background_file_syntax_error() {
        // A file present only in the FileIndex (never opened) with a syntax error
        // must be reported via the workspace-scope path.
        let service = new_ready_server();
        let server = service.inner();
        let uri = Url::parse("file:///proj/Bad.al").expect("valid uri");
        // Mirror an indexed-but-unopened file: on_document_change parses + indexes
        // without inserting into the open-document set.
        al_workspace::on_document_change(&server.workspace, &uri, BAD_SRC);

        let result = server
            .workspace_diagnostic(ws_diag_params())
            .await
            .expect("workspace_diagnostic succeeds");
        let files = reports(result);
        let report = files
            .iter()
            .find(|f| f.uri == uri)
            .expect("the bad background file should be reported");
        assert!(
            !report.full_document_diagnostic_report.items.is_empty(),
            "expected at least one diagnostic for the bad file"
        );
        assert_eq!(
            report.version, None,
            "an unopened workspace file has no document version"
        );
    }

    #[tokio::test]
    async fn reports_open_document_syntax_error() {
        let service = new_ready_server();
        let server = service.inner();
        let uri = Url::parse("file:///proj/OpenBad.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), BAD_SRC.to_string())
            .unwrap();
        al_workspace::on_document_change(&server.workspace, &uri, BAD_SRC);

        let result = server
            .workspace_diagnostic(ws_diag_params())
            .await
            .expect("workspace_diagnostic succeeds");
        let files = reports(result);
        // The open document must appear exactly once (no double-report from the
        // file-index pass).
        let matches: Vec<_> = files.iter().filter(|f| f.uri == uri).collect();
        assert_eq!(
            matches.len(),
            1,
            "open document should be reported exactly once, got {}",
            matches.len()
        );
        assert!(
            !matches[0].full_document_diagnostic_report.items.is_empty(),
            "expected diagnostics for the open bad document"
        );
    }

    #[tokio::test]
    async fn clean_workspace_reports_none() {
        let service = new_ready_server();
        let server = service.inner();
        let open_uri = Url::parse("file:///proj/OpenGood.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(open_uri.clone(), GOOD_SRC.to_string())
            .unwrap();
        al_workspace::on_document_change(&server.workspace, &open_uri, GOOD_SRC);
        // A second clean file that is only indexed, never opened.
        let bg_uri = Url::parse("file:///proj/BgGood.al").expect("valid uri");
        al_workspace::on_document_change(&server.workspace, &bg_uri, GOOD_BG_SRC);

        let result = server
            .workspace_diagnostic(ws_diag_params())
            .await
            .expect("workspace_diagnostic succeeds");
        let files = reports(result);
        assert!(
            files.is_empty(),
            "clean workspace must report no files, got: {:?}",
            files.iter().map(|f| f.uri.as_str()).collect::<Vec<_>>()
        );
    }
}
