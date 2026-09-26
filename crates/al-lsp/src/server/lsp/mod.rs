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

/// Debounce delay for the whole-workspace republish used by
/// `diagnosticsScope: "project"`.
///
/// That pass recomputes syntax diagnostics for *every* indexed file, so running
/// it on the per-keystroke debounce made each typing pause O(workspace). The
/// changed file is still refreshed on the 400 ms debounce; the project-wide
/// generation follows a typing burst (and every save).
const WORKSPACE_DIAGNOSTICS_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(5);

/// How many times a read request recomputes its answer after the workspace
/// generation moved under it. See [`AlServer::offload_after_ready`].
const MAX_OFFLOAD_ATTEMPTS: u32 = 3;

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

pub struct AlServer {
    pub(crate) client: Client,
    pub(crate) workspace: Arc<Workspace>,
    /// Explicit lifetime shared by request handlers and detached/background
    /// tasks. Once cancelled, no task may write to the LSP client transport.
    pub(crate) session: LspSessionState,
    /// Root URI from initialize params, used in initialized().
    pub(crate) root_uri: RwLock<Option<Url>>,
    /// Pending debounced diagnostics tasks, keyed by document URI.
    ///
    /// The debounce is per document: a keystroke in file B must not cancel the
    /// pending diagnostics run for file A. A single shared slot did exactly
    /// that, leaving A with stale squiggles until its next edit or save.
    pub(crate) diag_tasks: Mutex<std::collections::HashMap<Url, tokio::task::JoinHandle<()>>>,
    /// Pending workspace-wide (`diagnosticsScope: "project"`) republish.
    ///
    /// The whole-workspace pass is inherently global, so it keeps a single slot
    /// — but on a much longer debounce than the per-file pass, so a typing
    /// burst no longer recomputes every indexed file per keystroke.
    pub(crate) workspace_diag_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
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
    /// Whether the client can register `workspace/didChangeWatchedFiles`
    /// dynamically, so files changed outside the editor reach the index.
    pub(crate) watched_files_registration: AtomicBool,
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

impl AlServer {
    /// Construct a server bound to `client`.
    ///
    /// Public so black-box transport tests can build a real
    /// `LspService`/`Server` pair (see `tests/lsp_transport.rs`).
    pub fn new(client: Client) -> Self {
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
            diag_tasks: Mutex::new(std::collections::HashMap::new()),
            workspace_diag_task: Mutex::new(None),
            init_task: Mutex::new(None),
            reindex_task: Mutex::new(None),
            init_done: AtomicBool::new(false),
            workspace_init_state,
            semantic_failure_reported: AtomicBool::new(false),
            definition_link_support: AtomicBool::new(false),
            watched_files_registration: AtomicBool::new(false),
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

        // The bridge read guard ends with this match arm, before the warning
        // below waits on the client: a bridge restart must not queue behind a
        // user notification.
        let fetched = match self.get_or_init_bridge().await {
            Some(guard) => match guard.as_ref() {
                Some(bridge) => Some(
                    bridge
                        .builtin_types()
                        .await
                        .map(|types| (types, bridge.version().to_string())),
                ),
                None => None,
            },
            None => None,
        };
        match fetched {
            Some(Ok((types, version))) => {
                tracing::info!(count = types.len(), "Loaded built-in types via bridge");
                crate::semantic::set_builtins(&self.workspace, types, &version);
                return Ok(());
            }
            Some(Err(error)) => {
                tracing::warn!(%error, "Failed to load built-in types via bridge");
                self.client
                    .show_message(
                        MessageType::WARNING,
                        format!("Failed to load AL built-in types: {error}"),
                    )
                    .await;
            }
            None => {}
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

        // As in `ensure_builtins_loaded`, the bridge read guard ends with the
        // match arm, before the warning waits on the client.
        let fetched = match self.get_or_init_bridge().await {
            Some(guard) => match guard.as_ref() {
                Some(bridge) => Some(bridge.error_codes().await),
                None => None,
            },
            None => None,
        };
        match fetched {
            Some(Ok(codes)) => {
                tracing::info!(count = codes.len(), "Loaded error codes via bridge");
                for ec in codes {
                    self.workspace
                        .error_codes
                        .insert(ec.code.clone(), ec.message.clone());
                }
            }
            Some(Err(error)) => {
                tracing::warn!(%error, "Failed to load error codes via bridge");
                self.client
                    .show_message(
                        MessageType::WARNING,
                        format!("Failed to load AL error codes: {error}"),
                    )
                    .await;
            }
            None => {}
        }
    }

    /// Look up an error code description for diagnostic enrichment.
    pub(crate) fn error_code_description(&self, code: &str) -> Option<String> {
        self.workspace
            .error_codes
            .get(code)
            .map(|v| v.value().clone())
    }

    /// Wait for a usable workspace generation, then release the read guard and
    /// return the document snapshot the request will be served from.
    ///
    /// Holding the generation read guard across an awaited semantic-bridge call
    /// (up to the 5 s bridge timeout) queues a `did_change` writer behind it on
    /// tokio's fair `RwLock`, which in turn blocks every later reader — one slow
    /// bridge hover stalled typing and every other request. The snapshot lets
    /// the caller detect a document that moved on under it instead.
    async fn snapshot_after_ready(&self, uri: &Url) -> Result<Option<(Arc<String>, i32)>> {
        let generation = self.await_ready().await?;
        let snapshot = self.workspace.documents.get_text_and_client_version(uri);
        drop(generation);
        Ok(snapshot)
    }

    /// Wait for a usable workspace generation, run `work` on the blocking pool
    /// with the generation read guard released, and recompute it if the
    /// workspace moved on while it ran.
    ///
    /// `tokio::sync::RwLock` is fair: a read guard held across an await queues
    /// `did_change`'s writer behind it, and every later reader behind that
    /// writer. A whole-workspace walk under the guard therefore froze typing
    /// for as long as the walk took, and the editor's text and the server's
    /// diverged meanwhile. The guard is released first and
    /// `generation_revision` compared afterwards, so a result computed across a
    /// generation swap is never returned as current.
    ///
    /// Answering such a pass with `ContentModified` made a read request fail
    /// for a swap that had nothing to do with it: `did_close` bumps the
    /// generation twice (once on close, once when the saved file has been read
    /// back), and a `documentSymbol` or `workspace/symbol` issued right after a
    /// close landed on the second bump. The invalidated pass is recomputed
    /// against the new generation instead. The retry is bounded, and once the
    /// bound is reached the newest computed result is returned: a read request
    /// must produce an answer even while the workspace keeps churning.
    async fn offload_after_ready<T, F>(&self, what: &'static str, work: F) -> Result<T>
    where
        F: Fn() -> T + Send + Sync + 'static,
        T: Send + 'static,
    {
        let work = Arc::new(work);
        let mut newest = None;
        for attempt in 1..=MAX_OFFLOAD_ATTEMPTS {
            let generation = self.await_ready().await?;
            let revision = self.workspace.generation_revision();
            drop(generation);

            let pass = Arc::clone(&work);
            let result = tokio::task::spawn_blocking(move || pass())
                .await
                .map_err(|error| internal_error(format!("{what} worker failed: {error}")))?;

            let generation = self.workspace.generation_lock.read().await;
            let moved = self.workspace.generation_revision() != revision;
            drop(generation);
            newest = Some(result);
            if !moved {
                break;
            }
            tracing::debug!(
                request = what,
                attempt,
                "workspace generation moved under a read request, recomputing"
            );
        }
        Ok(newest.expect("the offload loop runs at least one pass"))
    }

    /// Whether `uri` still holds the exact snapshot a request started from.
    fn snapshot_is_current(&self, uri: &Url, snapshot: &Option<(Arc<String>, i32)>) -> bool {
        let Some((text, version)) = snapshot else {
            // Nothing was open when the request started; nothing to invalidate.
            return true;
        };
        self.workspace
            .documents
            .get_text_and_client_version(uri)
            .is_some_and(|(current_text, current_version)| {
                current_version == *version && Arc::ptr_eq(&current_text, text)
            })
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
        // Held through the clearing publishes, as in `publish_if_current`.
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

        // Hold the `diag_tasks` lock across abort → spawn → store as one
        // critical section. Releasing it between the abort and the store let two
        // interleaved did_change handlers both observe "no pending task", spawn
        // two debounce tasks, and race two publishes for the same URI — the
        // second store overwrote the first handle without aborting it. Holding
        // the guard serializes scheduling so only the most recent keystroke's
        // task survives — *for this URI*; other documents keep their pending
        // runs.
        let mut guard = self.diag_tasks.lock().await;
        guard.retain(|_, handle| !handle.is_finished());
        if let Some(old) = guard.remove(&uri) {
            old.abort();
        }

        let workspace = Arc::clone(&self.workspace);
        let client = self.client.clone();
        let workspace_diagnostic_uris = Arc::clone(&self.workspace_diagnostic_uris);
        let session = self.session.clone();
        let uri_key = uri.clone();
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
            // Read config here (not at schedule time) so only the task that
            // survives the debounce pays the clone — keystrokes that abort the
            // previous task before its sleep elapses never clone AlConfig. The
            // clone is needed so per-rule lint filtering works in spawn_blocking.
            let config = workspace.config.read().await.clone();
            // Project scope recomputes only the file that changed here; the
            // whole-workspace generation is republished on its own (much
            // longer) debounce and on save.
            let project_scope =
                config.diagnostics_scope == al_project::config::DiagnosticsScope::Project;
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
            if session.is_cancelled() {
                return;
            }
            // Hold the generation read lock from the currency check through the
            // publish so a didClose on another worker cannot clear the document
            // in between; see `diagnostics::publish_if_current`.
            let generation = workspace.generation_lock.read().await;
            let still_current = crate::server::diagnostics::snapshot_is_current(
                &workspace,
                &uri,
                &document_text,
                document_version,
            );
            if !still_current {
                tracing::debug!(
                    uri = %uri,
                    document_version,
                    "debounced diagnostics: document changed during analysis, skipping stale publish"
                );
                return;
            }
            if project_scope {
                // Keep the project-scope bookkeeping consistent: the next
                // whole-workspace pass clears only URIs it previously
                // published, so record (or drop) this one accordingly.
                let mut published = workspace_diagnostic_uris.lock().await;
                if lsp_diags.is_empty() {
                    published.remove(&uri);
                } else {
                    published.insert(uri.clone());
                }
            }
            client
                .publish_diagnostics(uri, lsp_diags, Some(document_version))
                .await;
            drop(generation);
        });

        guard.insert(uri_key, handle);
    }

    /// Schedule the debounced whole-workspace diagnostics republish used by
    /// `diagnosticsScope: "project"`.
    ///
    /// Only one is ever pending: the pass is global, and re-arming it on each
    /// keystroke is exactly what keeps a typing burst from paying O(workspace)
    /// per pause.
    async fn schedule_workspace_diagnostics(&self) {
        // The awaits below run in the spawned task, not under this guard.
        let mut guard = self.workspace_diag_task.lock().await;
        if let Some(old) = guard.take() {
            old.abort();
        }
        let workspace = Arc::clone(&self.workspace);
        let client = self.client.clone();
        let semantic_diagnostic_cache = Arc::clone(&self.semantic_diagnostic_cache);
        let workspace_diagnostic_uris = Arc::clone(&self.workspace_diagnostic_uris);
        let session = self.session.clone();
        *guard = Some(tokio::spawn(async move {
            tokio::time::sleep(WORKSPACE_DIAGNOSTICS_DEBOUNCE).await;
            if session.is_cancelled() {
                return;
            }
            crate::server::diagnostics::publish_workspace_diagnostics_parts(
                workspace,
                client,
                semantic_diagnostic_cache,
                workspace_diagnostic_uris,
                None,
                &session,
            )
            .await;
        }));
    }

    /// Cancel a pending project-scope republish (a save or close supersedes it).
    async fn cancel_workspace_diagnostics(&self) {
        if let Some(task) = self.workspace_diag_task.lock().await.take() {
            task.abort();
        }
    }
}

/// Extract the "al" sub-object from a settings value, or use the value as-is.
///
/// Zed sends settings nested under an "al" key; other clients may send flat objects.
/// Used by both `initialize` and `did_change_configuration` to normalise the input.
fn extract_al_settings(value: serde_json::Value) -> serde_json::Value {
    value.get("al").cloned().unwrap_or(value)
}

/// Remove from `config` the privileged settings this project's own files ask
/// for, unless the project is trusted, and return the message for the user.
///
/// The editor merges `.zed/settings.json` from the worktree into the user's
/// own settings before sending them, so what arrives here does not say where
/// each value came from. `al_project::trust::gate` asks the repository files
/// and takes back exactly what they contribute.
/// With no root, or a root that is not a local file, there is no repository to
/// ask, so every privileged value goes: the settings that arrived already carry
/// whatever `.zed/settings.json` contributed, and nothing here can tell which
/// ones those are. Containment answers the same situation the same way, with
/// "No project is loaded, so no file path can be authorised".
fn gate_repository_settings(
    root_uri: Option<&Url>,
    config: &mut al_project::config::AlConfig,
) -> Option<String> {
    let root = match root_uri.and_then(|uri| uri.to_file_path().ok()) {
        Some(root) => root,
        None => {
            al_project::trust::deny_privileged(config);
            return Some(
                "AL settings that can run code were not applied: this session names no local \
                 project directory, so the settings a repository contributed cannot be told \
                 apart from your own."
                    .to_string(),
            );
        }
    };
    let dotnet_advisory = al_project::trust::enforce_dotnet_path(&root);
    match al_project::trust::gate(&root, config) {
        Ok(decision) => match (decision.advisory(), dotnet_advisory) {
            (Some(settings), Some(dotnet)) => Some(format!("{settings}\n{dotnet}")),
            (settings, dotnet) => settings.or(dotnet),
        },
        Err(error) => {
            tracing::warn!(%error, "cannot read this project's settings files for the trust check");
            // Unreadable repository settings cannot be subtracted one value at
            // a time, so every privileged field goes instead.
            al_project::trust::deny_privileged(config);
            Some(format!(
                "AL settings that can run code were not applied: this project's settings files \
                 could not be read ({error})"
            ))
        }
    }
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

        let watched_files = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.did_change_watched_files.as_ref())
            .and_then(|watched| watched.dynamic_registration)
            .unwrap_or(false);
        self.watched_files_registration
            .store(watched_files, Ordering::Relaxed);

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
            let root_uri = self.root_uri.read().await.clone();
            let (cap, advisory) = {
                let mut config = self.workspace.config.write().await;
                let report = config.merge_reporting(&al_settings);
                if !report.is_empty() {
                    tracing::warn!(
                        unknown = ?report.unknown_keys,
                        invalid = ?report.invalid_values,
                        "settings in initializationOptions were not applied"
                    );
                }
                let advisory = gate_repository_settings(root_uri.as_ref(), &mut config);
                (config.max_document_size_bytes, advisory)
            };
            if let Some(advisory) = advisory {
                self.client
                    .show_message(MessageType::WARNING, advisory)
                    .await;
            }
            // apply the per-document size cap to the store.
            self.workspace.documents.set_max_doc_bytes(cap);
            tracing::info!("Parsed initialization options into config");
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        // INCREMENTAL: `did_change` applies ranged edits through
                        // `al_source::documents` with UTF-16 column handling, so
                        // there is no reason to make every keystroke re-send and
                        // re-ingest the whole document. A change notification
                        // without a range (full replacement) is still accepted by
                        // the same path, so clients that send full text keep
                        // working.
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
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

        // Without a watcher the index only learns about files the editor has
        // open, so a `git checkout` or a generator left every other file stale
        // until it was opened. The registration is a request to the client;
        // it runs on its own so `initialized` does not wait on the reply.
        if self.watched_files_registration.load(Ordering::Relaxed) {
            let client = self.client.clone();
            tokio::spawn(async move {
                let options = DidChangeWatchedFilesRegistrationOptions {
                    watchers: vec![FileSystemWatcher {
                        glob_pattern: GlobPattern::String("**/*.al".to_string()),
                        kind: None,
                    }],
                };
                let registration = Registration {
                    id: "al-lsp-watched-al-files".to_string(),
                    method: "workspace/didChangeWatchedFiles".to_string(),
                    register_options: serde_json::to_value(options).ok(),
                };
                if let Err(error) = client.register_capability(vec![registration]).await {
                    tracing::warn!(%error, "could not register the .al file watcher");
                }
            });
        }
    }

    /// Re-read `.al` files changed outside the editor. An open document is
    /// skipped: its editor text is newer than the disk.
    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        match self.await_ready().await {
            Ok(generation) => drop(generation),
            Err(_) => return,
        }
        // The root the startup scan indexed: the project, or without one the
        // editor's workspace folder.
        let project_root = self
            .workspace
            .project
            .read()
            .await
            .as_ref()
            .map(|project| project.root.clone());
        let scan_root = match project_root {
            Some(root) => root,
            None => match self
                .root_uri
                .read()
                .await
                .as_ref()
                .and_then(|uri| uri.to_file_path().ok())
            {
                Some(root) => root,
                None => return,
            },
        };
        let mut events: Vec<(std::path::PathBuf, bool)> = params
            .changes
            .into_iter()
            .filter_map(|event| {
                let path = event.uri.to_file_path().ok()?;
                al_source::file_index::is_scanned_path(&scan_root, &path)
                    .then_some((path, event.typ == FileChangeType::DELETED))
            })
            .collect();
        if events.is_empty() {
            return;
        }
        // Read under the write guard, so two notifications for one file apply
        // in order: a read taken before the guard could lose to an older one.
        let generation = self.workspace.generation_lock.write().await;
        // Open documents belong to the editor, which may have opened one
        // while this waited for the guard.
        events.retain(|(path, _)| {
            Url::from_file_path(path).map_or(true, |uri| !self.workspace.documents.contains(&uri))
        });
        let changes = match tokio::task::spawn_blocking(move || {
            events
                .into_iter()
                .filter_map(|(path, deleted)| {
                    if deleted {
                        return Some((path, None));
                    }
                    match al_source::file_index::read_source_file(&path) {
                        Ok(text) => Some((path, text)),
                        Err(error) => {
                            tracing::warn!(path = %path.display(), %error, "watched file could not be read");
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
        })
        .await
        {
            Ok(changes) => changes,
            Err(error) => {
                tracing::warn!(%error, "watched file read worker failed");
                return;
            }
        };
        if changes.is_empty() {
            return;
        }
        tracing::info!(files = changes.len(), "files changed on disk");
        al_workspace::apply_disk_changes(&self.workspace, changes);
        let scope = self.workspace.config.read().await.diagnostics_scope;
        drop(generation);
        if scope == al_project::config::DiagnosticsScope::Project {
            self.schedule_workspace_diagnostics().await;
        }
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
        // Each handle is taken out of its slot before it is awaited. A guard
        // in the `for` or `if let` head would otherwise stay held until the
        // aborted task has finished.
        let pending_diagnostics: Vec<tokio::task::JoinHandle<()>> = {
            let mut guard = self.diag_tasks.lock().await;
            guard.drain().map(|(_, task)| task).collect()
        };
        let workspace_diagnostics = self.workspace_diag_task.lock().await.take();
        for task in pending_diagnostics.into_iter().chain(workspace_diagnostics) {
            task.abort();
            if let Err(error) = task.await {
                if !error.is_cancelled() {
                    return Err(internal_error(format!(
                        "diagnostics task failed during shutdown: {error}"
                    )));
                }
            }
        }
        let init_task = self.init_task.lock().await.take();
        if let Some(task) = init_task {
            task.abort();
            if let Err(error) = task.await {
                if !error.is_cancelled() {
                    return Err(internal_error(format!(
                        "workspace initialization task failed during shutdown: {error}"
                    )));
                }
            }
        }
        let reindex_task = self.reindex_task.lock().await.take();
        if let Some(task) = reindex_task {
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
                // `on_document_change` reparses the document, clones its text
                // and rebuilds this file's index entries. On a 10k-line AL file
                // that is milliseconds of CPU per keystroke, and running it
                // here occupied the tokio worker that also drives every other
                // connection's futures. The generation write guard still spans
                // the whole mutation, so the document store and the file index
                // stay in step; only the CPU moves to the blocking pool.
                let reindex_workspace = Arc::clone(&self.workspace);
                let reindex_uri = uri.clone();
                let reindex_text = Arc::clone(&text_arc);
                if let Err(error) = tokio::task::spawn_blocking(move || {
                    al_workspace::on_document_change(
                        &reindex_workspace,
                        &reindex_uri,
                        &reindex_text,
                    );
                })
                .await
                {
                    // The index is now behind the document store. Say so
                    // rather than serving stale results silently.
                    tracing::error!(uri = %uri, %error, "did_change: re-index worker failed");
                    drop(generation);
                    self.client
                        .show_message(
                            MessageType::ERROR,
                            format!(
                                "AL language server could not re-index {uri} after an edit. \
                                 Run al.reindex to resynchronize."
                            ),
                        )
                        .await;
                    return;
                }
                // Only schedule per-keystroke diagnostics when trigger is Continuous.
                // In OnSave mode, diagnostics are deferred to did_save to avoid per-keystroke work.
                let (trigger, scope) = {
                    let config = self.workspace.config.read().await;
                    (config.diagnostics_trigger, config.diagnostics_scope)
                };
                drop(generation);
                if trigger == al_project::config::DiagnosticsTrigger::Continuous {
                    self.schedule_diagnostics(uri.clone()).await;
                    if scope == al_project::config::DiagnosticsScope::Project {
                        self.schedule_workspace_diagnostics().await;
                    }
                } else {
                    // Cancel any lingering debounced task for THIS document from
                    // a previous Continuous session. Other documents' pending
                    // runs are untouched.
                    if let Some(old) = self.diag_tasks.lock().await.remove(&uri) {
                        old.abort();
                    }
                    self.cancel_workspace_diagnostics().await;
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

        // Cancel this document's pending debounced diagnostics task. Without
        // this, a task armed by the last keystroke can wake after the close and
        // publish ghost squiggles. The in-task `contains` check is the primary
        // guard; aborting here also prevents unnecessary work. Other documents
        // keep their pending runs.
        if let Some(old) = self.diag_tasks.lock().await.remove(&uri) {
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
            // The clear is sent under the write guard so that no
            // `publish_if_current` can land after it.
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

        // Project scope republishes the whole workspace generation on save.
        // Per-keystroke edits only refresh the changed file (see
        // `schedule_diagnostics`), so save is the point where cross-file
        // consequences of the edit become visible.
        if self.workspace.config.read().await.diagnostics_scope
            == al_project::config::DiagnosticsScope::Project
        {
            self.cancel_workspace_diagnostics().await;
            diagnostics::publish_workspace_diagnostics(self).await;
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
        let report = staged_config.merge_reporting(&al_settings);
        let root_uri = self.root_uri.read().await.clone();
        if let Some(advisory) = gate_repository_settings(root_uri.as_ref(), &mut staged_config) {
            self.client
                .show_message(MessageType::WARNING, advisory)
                .await;
        }
        let symbol_paths_changed = old_cache_path != staged_config.package_cache_path
            || old_local_paths != staged_config.app_local_folder_paths;
        if !report.unknown_keys.is_empty() {
            tracing::warn!(
                keys = ?report.unknown_keys,
                "ignoring AL settings this server does not model"
            );
        }
        if !report.invalid_values.is_empty() {
            let msg = format!("Invalid AL settings: {}", report.invalid_values.join(", "));
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
            // Keyed on the package revision: staging re-reads every `.app` in
            // the cache, and a retry keyed on `generation_revision` restarted
            // on every keystroke and never published the new settings.
            let revision = self.workspace.package_revision();
            let project = self.workspace.project.read().await.clone();

            let Some(mut project) = project else {
                drop(generation);
                let publication = self.workspace.generation_lock.write().await;
                if self.workspace.package_revision() != revision {
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
            if self.workspace.package_revision() != revision {
                drop(publication);
                continue;
            }

            // Both guards are taken before the first swap, so the publication
            // sequence below has no await point that a cancelled handler could
            // unwind from with the indexes and the project disagreeing. The
            // order is project, then config: a task that holds a config guard
            // while it waits for the project deadlocks against this one.
            let mut published_project = self.workspace.project.write().await;
            let mut published_config = self.workspace.config.write().await;
            self.workspace.symbols.replace_with(&symbols);
            *published_project = Some(project);
            *published_config = staged_config.clone();
            self.workspace
                .replace_package_info(loaded.iter().map(al_workspace::PackageInfo::from).collect());
            self.workspace.invalidate_insight_graph();
            self.workspace.mark_package_generation_changed();
            self.workspace.mark_generation_changed();
            drop(published_config);
            drop(published_project);
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
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        // The generation read guard is released here, before the awaited bridge
        // calls below, so a concurrent edit is never queued behind this request.
        let snapshot = self.snapshot_after_ready(uri).await?;
        self.ensure_builtins_loaded().await?;
        let start = std::time::Instant::now();
        let result = hover::handle_hover(self, uri, position)
            .await
            .map_err(internal_error)?;
        if !self.snapshot_is_current(uri, &snapshot) {
            tracing::debug!(uri = %uri, "hover: document changed during analysis, discarding stale result");
            return Ok(None);
        }
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "hover");
        Ok(result)
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        // See `snapshot_after_ready`: the guard must not span the bridge await.
        let snapshot = self.snapshot_after_ready(uri).await?;
        self.ensure_builtins_loaded().await?;
        let start = std::time::Instant::now();
        let result = completions::handle_completion(self, uri, position)
            .await
            .map_err(internal_error)?;
        if !self.snapshot_is_current(uri, &snapshot) {
            tracing::debug!(uri = %uri, "completion: document changed during analysis, discarding stale result");
            return Ok(None);
        }
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
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let position = params.text_document_position_params.position;
        let workspace = Arc::clone(&self.workspace);
        let locations = self
            .offload_after_ready("go-to-implementation", move || {
                al_analysis::queries::implementation::find_implementations(
                    &workspace,
                    &uri,
                    position.into(),
                )
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect::<Vec<Location>>()
            })
            .await?;

        if locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(GotoImplementationResponse::Array(locations)))
        }
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
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
        let result = self
            .offload_after_ready("references", move || {
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
            .await?
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
        let uri = params.text_document.uri.clone();
        let start = std::time::Instant::now();
        // spawn_blocking for cancel-friendliness on large files.
        let workspace = Arc::clone(&self.workspace);
        let uri_for_log = uri.clone();
        let hierarchical = self.document_symbol_hierarchical.load(Ordering::Relaxed);
        let result = self
            .offload_after_ready("document-symbol", move || {
                al_analysis::queries::symbols::document_symbols(&workspace, &uri).map(|symbols| {
                    if hierarchical {
                        DocumentSymbolResponse::Nested(
                            symbols.into_iter().map(Into::into).collect(),
                        )
                    } else {
                        DocumentSymbolResponse::Flat(al_analysis::lsp::flatten_document_symbols(
                            symbols, &uri,
                        ))
                    }
                })
            })
            .await?;
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
        let uri = params.text_document.uri.clone();
        let start = std::time::Instant::now();
        // spawn_blocking — semantic_tokens_full traverses the entire
        // tree-sitter tree on big AL files; cancellation-friendliness matters.
        let workspace = Arc::clone(&self.workspace);
        let uri_for_log = uri.clone();
        let result = self
            .offload_after_ready("semantic-tokens", move || {
                // A document the server has not loaded and a document with no
                // tokens are both "no result" to the editor.
                let tokens =
                    al_analysis::queries::semantic_tokens::semantic_tokens_full(&workspace, &uri)?;
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
            .await?;
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
        // Honour the client's `only` filter: a request for `quickfix` must not
        // come back with the `source` actions.
        let only = params.context.only.as_ref();
        let start = std::time::Instant::now();
        let result = handlers::handle_code_action(self, uri, range, diagnostics, only);
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
        // Only this request's own document invalidates it. An edit elsewhere in
        // the workspace, or the generation bump `did_close` publishes when it
        // has read the saved file back, says nothing about these diagnostics,
        // and answering those with ContentModified made a read request fail for
        // an unrelated change.
        let snapshot_current = self.snapshot_is_current(uri, &Some((text, client_version)));
        drop(generation);
        if !snapshot_current {
            return Err(content_modified_error());
        }
        tracing::debug!(uri = %uri, count = diags.len(), elapsed_us = elapsed.as_micros() as u64, "diagnostic (pull)");

        Ok(diagnostics::full_diagnostic_report(diags))
    }

    async fn workspace_diagnostic(
        &self,
        _params: WorkspaceDiagnosticParams,
    ) -> Result<WorkspaceDiagnosticReportResult> {
        // Project-scope pull diagnostics aggregate parse and syntax errors
        // across every indexed file plus bridge diagnostics for open documents.
        let start = std::time::Instant::now();
        // A pass invalidated by a concurrent edit is recomputed against the new
        // generation rather than answered with ContentModified. After
        // `MAX_OFFLOAD_ATTEMPTS` the newest computed set is reported: a client
        // that polls project diagnostics while the user types has to get an
        // answer, and the debounced push pass corrects whatever moved since.
        let mut reports = Vec::new();
        for attempt in 1..=MAX_OFFLOAD_ATTEMPTS {
            let generation = self.await_ready().await?;
            let revision = self.workspace.generation_revision();
            drop(generation);
            reports = diagnostics::compute_workspace_diagnostics(self)
                .await
                .map_err(|error| internal_error(error.to_string()))?;
            let generation = self.workspace.generation_lock.read().await;
            let moved = self.workspace.generation_revision() != revision;
            drop(generation);
            if !moved {
                break;
            }
            tracing::debug!(
                attempt,
                "workspace generation moved under workspace_diagnostic, recomputing"
            );
        }
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
        let start = std::time::Instant::now();
        // Up to 10 000 results from a workspace-wide scan: run it on the
        // blocking pool like `references`/`documentSymbol` already do, so a
        // broad query cannot stall the executor driving every other request.
        let workspace_handle = Arc::clone(&self.workspace);
        let query = params.query.clone();
        let result = self
            .offload_after_ready("workspace-symbol", move || {
                workspace::handle_workspace_symbol(&workspace_handle, &query)
            })
            .await?;
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(query = %params.query, count, elapsed_us = elapsed.as_micros() as u64, "workspace_symbol");
        Ok(result)
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let range = params.range;
        // `ensure_builtins_loaded` awaits the semantic bridge; the generation
        // guard is released before that await (see `snapshot_after_ready`).
        let _snapshot = self.snapshot_after_ready(uri).await?;
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
        // Wait for a usable workspace, then release the generation read guard
        // before running the command. `al.compile` spawns `dotnet alc` and
        // `al.runTest` drives a whole test run; holding the guard across either
        // queued `did_change`'s writer behind it, so the editor stopped
        // accepting AL edits until the build finished. Every command clones
        // the project state it needs (`commands::compile` takes root, package
        // cache, packages and config up front) or takes its own guard while
        // publishing a replacement generation.
        drop(self.await_ready().await?);

        // An optional `config` argument names the launch configuration to act
        // on, following the convention `al_debug` already established.
        let requested_config = params
            .arguments
            .first()
            .and_then(|value| value.get("config"))
            .and_then(|value| value.as_str());
        let start = std::time::Instant::now();
        let result = match params.command.as_str() {
            "al.downloadSymbols" | "al.downloadSymbolsNuget" => {
                workspace::download_symbols_command(
                    self,
                    workspace::DownloadSource::NuGet,
                    requested_config,
                )
                .await;
                Ok(None)
            }
            "al.downloadSymbolsServer" => {
                workspace::download_symbols_command(
                    self,
                    workspace::DownloadSource::Server,
                    requested_config,
                )
                .await;
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
                commands::find_references(self, &params.arguments)
                    .await
                    .map_err(internal_error)?,
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
    position: Option<Position>,
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

    // "Runnable at cursor": when the client sends a position, only the test
    // whose declaration the cursor sits on (or inside, up to the next test)
    // is returned. Without a position the whole file's tests are listed.
    let cursor_line = position.map(|position| position.line);
    let file_tests: Vec<(&al_analysis::queries::tests::TestCodeunit, u32)> = test_codeunits
        .iter()
        .filter(|codeunit| paths_equivalent(std::path::Path::new(&codeunit.file), file_path))
        .flat_map(|codeunit| {
            codeunit
                .tests
                .iter()
                .map(move |test| (codeunit, test.line.saturating_sub(1)))
        })
        .collect();
    // The innermost test declaration at or above the cursor.
    let selected_line = cursor_line.and_then(|cursor| {
        file_tests
            .iter()
            .map(|(_, line)| *line)
            .filter(|line| *line <= cursor)
            .max()
    });

    for codeunit in test_codeunits
        .iter()
        .filter(|codeunit| paths_equivalent(std::path::Path::new(&codeunit.file), file_path))
    {
        for test in &codeunit.tests {
            // Test discovery reports human-readable 1-based lines. LSP ranges
            // are zero-based, including the custom Zed runnable location.
            let line = test.line.saturating_sub(1);
            if cursor_line.is_some() && selected_line != Some(line) {
                continue;
            }
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
mod tests;
