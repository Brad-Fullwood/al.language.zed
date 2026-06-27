//! AlServer state and LSP lifecycle.

use crate::workspace::Workspace;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};
use tower_lsp::jsonrpc::Result;
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

/// Debounce delay for diagnostics: wait this long after the last keystroke before running.
/// ISSUE-025 fix: prevents bridge calls (up to 5s) from blocking hover/completion.
const DIAGNOSTICS_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

/// Every `al.*` command the server advertises in `executeCommandProvider` and
/// handles in [`AlServer::execute_command`]. Single source of truth: the
/// capability list and the dispatch both derive from this slice, and the
/// CodeLens commands (`al.findReferences`, `al.showProfiler`, `al.runTest`)
/// must all appear here so no clickable lens is a dead no-op (gap A8). The
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
    // CodeLens-backed commands (A8): keep in sync with
    // `al_analysis::queries::code_lens::LENS_COMMAND_IDS`.
    "al.findReferences",
    "al.showProfiler",
    "al.runTest",
];

pub struct AlServer {
    pub(crate) client: Client,
    pub(crate) workspace: Arc<Workspace>,
    /// Root URI from initialize params, used in initialized().
    pub(crate) root_uri: RwLock<Option<Url>>,
    /// Handle to the currently-pending debounced diagnostics task.
    /// Replaced (and thus cancelled) on every new keystroke.
    /// ISSUE-025 fix: diagnostics run async, not inline in did_change.
    pub(crate) diag_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Handle to the background workspace initialisation task.
    /// ISSUE-026 fix: workspace init runs async so initialized() returns promptly.
    pub(crate) init_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// JoinHandle for the most recent al.reindex background task.
    /// Stored so a second al.reindex can abort an in-flight previous run.
    pub(crate) reindex_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Guard against double-initialization (ISSUE-073).
    /// Zed may send `initialized` twice when opening multiple worktrees.
    /// CAS ensures workspace init runs only once per server instance.
    pub(crate) init_done: AtomicBool,
    /// Set to `true` inside the spawned task, after `initialize_workspace` completes.
    /// `await_ready` checks this flag, NOT `init_done`, so it only returns once the
    /// background work has actually finished (not just been scheduled).
    pub(crate) workspace_ready: Arc<AtomicBool>,
    pub(crate) init_notify: Arc<Notify>,
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
}

impl AlServer {
    pub(crate) fn new(client: Client) -> Self {
        let workspace = Arc::new(Workspace::new());

        // Register a notify sink so al-core can surface bridge failures to the user.
        let sink_client = client.clone();
        let _ = workspace
            .notify_sink
            .set(std::sync::Arc::new(move |msg: &str| {
                let c = sink_client.clone();
                let m = msg.to_owned();
                tokio::spawn(async move {
                    c.show_message(tower_lsp::lsp_types::MessageType::WARNING, m)
                        .await;
                });
            }));

        Self {
            client,
            workspace,
            root_uri: RwLock::new(None),
            diag_task: Mutex::new(None),
            init_task: Mutex::new(None),
            reindex_task: Mutex::new(None),
            init_done: AtomicBool::new(false),
            workspace_ready: Arc::new(AtomicBool::new(false)),
            init_notify: Arc::new(Notify::new()),
            semantic_failure_reported: AtomicBool::new(false),
            definition_link_support: AtomicBool::new(false),
            document_symbol_hierarchical: AtomicBool::new(false),
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

    /// If initialization has already completed (`workspace_ready` is true) this returns
    /// immediately. Otherwise it waits for the `init_notify` signal with a 30s
    /// timeout so that handlers opened immediately after server startup receive
    /// full workspace data rather than empty results.
    ///
    /// IMPORTANT: we subscribe to the Notify *before* checking the flag to avoid the
    /// lost-wakeup race where the background task completes and fires `notify_waiters()`
    /// between the flag check and the `.await`. By calling `notified()` first we pin
    /// a permit that survives that window.
    async fn await_ready(&self) {
        let notified = self.init_notify.notified();
        if self.workspace_ready.load(Ordering::Acquire) {
            return;
        }
        if tokio::time::timeout(std::time::Duration::from_secs(30), notified)
            .await
            .is_err()
        {
            tracing::warn!("await_ready: timed out after 30s waiting for workspace initialization");
        }
    }

    pub(crate) async fn ensure_builtins_loaded(&self) {
        if !self
            .workspace
            .builtins
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            return;
        }

        if let Some(guard) = self.get_or_init_bridge().await {
            if let Some(bridge) = guard.as_ref() {
                match bridge.builtin_types().await {
                    Ok(types) => {
                        tracing::info!(count = types.len(), "Loaded built-in types via bridge");
                        let version = bridge.version().to_string();
                        crate::semantic::set_builtins(&self.workspace, types, &version);
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

    pub(crate) async fn get_or_init_bridge(
        &self,
    ) -> Option<tokio::sync::RwLockReadGuard<'_, Option<crate::semantic::SemanticBridge>>> {
        crate::semantic::get_or_init_bridge(&self.workspace).await
    }

    /// Schedule debounced diagnostics for `uri` with the given document text.
    ///
    /// ISSUE-025 fix: Cancels the previous pending task (if any) so that only
    /// the most recent keystroke triggers a diagnostics run. The actual diagnostics
    /// publish runs after `DIAGNOSTICS_DEBOUNCE` of silence. This prevents bridge
    /// calls (up to bridge timeout = 5s) from blocking hover/completion.
    async fn schedule_diagnostics(&self, uri: Url) {
        // ISSUE-072: skip diagnostics for virtual symbol cache files — they are not
        // workspace files and Zed logs a warning for every publishDiagnostics on them.
        if crate::server::diagnostics::is_cache_path(&uri) {
            tracing::debug!(uri = %uri, "schedule_diagnostics: skipping cache file");
            return;
        }

        if let Some(old) = self.diag_task.lock().await.take() {
            old.abort();
        }

        let workspace = Arc::clone(&self.workspace);
        let client = self.client.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(DIAGNOSTICS_DEBOUNCE).await;
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
            if !workspace.documents.contains(&uri) {
                tracing::debug!(uri = %uri, "debounced diagnostics: document no longer open, skipping publish");
                return;
            }
            // Read config here (not at schedule time) so only the task that
            // survives the debounce pays the clone — keystrokes that abort the
            // previous task before its sleep elapses never clone AlConfig. The
            // clone is needed so per-rule lint filtering works in spawn_blocking.
            let config = workspace.config.read().await.clone();
            let diag_uri = uri.clone();
            let lsp_diags: Vec<Diagnostic> = match tokio::task::spawn_blocking(move || {
                crate::queries::diagnostics::syntax_diagnostics(&workspace, &diag_uri, &config)
                    .iter()
                    .map(crate::server::diagnostics::syntax_diag_to_lsp)
                    .collect()
            })
            .await
            {
                Ok(diags) => diags,
                Err(e) => {
                    tracing::warn!("debounced diagnostics task panicked: {e}");
                    return;
                }
            };
            client.publish_diagnostics(uri, lsp_diags, None).await;
        });

        *self.diag_task.lock().await = Some(handle);
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
            // F-OPEN-042: apply the per-document size cap to the store.
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
                                token_types: crate::syntax::tokens::token_types::LEGEND
                                    .iter()
                                    .map(|s| SemanticTokenType::new(s))
                                    .collect(),
                                token_modifiers: crate::syntax::tokens::token_modifiers::LEGEND
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
                // workspace_diagnostics is true (B11): the `workspace_diagnostic` handler
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
        // ISSUE-073: guard against double-init when Zed sends `initialized` more than once
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

        // ISSUE-026 fix: spawn workspace initialization into a background task so this
        // notification handler returns promptly. Clients must not be kept waiting by
        // NuGet downloads, package loading, or bridge initialization.
        let ws = Arc::clone(&self.workspace);
        let client = self.client.clone();
        let notify = Arc::clone(&self.init_notify);
        let ready_flag = Arc::clone(&self.workspace_ready);
        let handle = tokio::spawn(async move {
            // initialize_workspace signals ready_flag + init_notify internally
            // as soon as the file scan is done — before any blocking
            // package-download prompt — so workspace queries don't deadlock.
            workspace::initialize_workspace(ws, client, root_uri, ready_flag, notify).await;
        });
        *self.init_task.lock().await = Some(handle);
    }

    async fn shutdown(&self) -> Result<()> {
        if let Some(task) = self.diag_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = self.init_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = self.reindex_task.lock().await.take() {
            task.abort();
        }
        crate::semantic::shutdown_bridge(&self.workspace).await;
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let text = params.text_document.text.clone();
        tracing::info!(uri = %uri, len = text.len(), "did_open");

        self.workspace
            .documents
            .open(uri.clone(), params.text_document.text);
        crate::workspace::on_document_change(&self.workspace, &uri, &text);

        diagnostics::publish_diagnostics(self, &uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let client_version = params.text_document.version;
        tracing::debug!(uri = %uri, version = client_version, change_count = params.content_changes.len(), "did_change");

        // F-OPEN-053: LSP requires client `version` to be monotonically
        // increasing for a given document. tower-lsp can in theory interleave
        // notifications under load; an out-of-order delivery would otherwise
        // silently corrupt the rope. Compare against our stored version and
        // warn (not error) on a stale delivery — the editor will likely
        // re-sync on the next keystroke, and rejecting would create a
        // visible divergence between client and server text.
        if let Some(server_version) = self.workspace.documents.get_version(&uri) {
            if client_version < server_version {
                tracing::warn!(
                    uri = %uri,
                    client_version,
                    server_version,
                    "did_change: client version went backwards — applying anyway; editor should resync"
                );
            }
        }

        let changes: Vec<crate::documents::TextChange> = params
            .content_changes
            .iter()
            .map(|c| crate::documents::TextChange {
                range: c.range.map(|r| crate::documents::TextRange {
                    start_line: r.start.line,
                    start_character: r.start.character,
                    end_line: r.end.line,
                    end_character: r.end.character,
                }),
                text: c.text.clone(),
            })
            .collect();
        // F-OPEN-054: apply the changes and capture the resulting text under a
        // single write lock. A separate `apply_changes` + `get_text` pair would
        // leave a TOCTOU window where a concurrent `did_change` (tower-lsp can
        // interleave handlers under load) applies a later keystroke between the
        // mutation and the read, feeding a version-skewed snapshot into the
        // debounced diagnostics task.
        if let Some((text_arc, _version)) = self
            .workspace
            .documents
            .apply_changes_and_get(&uri, &changes)
        {
            crate::workspace::on_document_change(&self.workspace, &uri, &text_arc);
            // Only schedule per-keystroke diagnostics when trigger is Continuous.
            // In OnSave mode, diagnostics are deferred to did_save to avoid per-keystroke work.
            let trigger = self.workspace.config.read().await.diagnostics_trigger;
            if trigger == crate::config::DiagnosticsTrigger::Continuous {
                self.schedule_diagnostics(uri).await;
            } else {
                // Cancel any lingering debounced task from a previous Continuous session.
                if let Some(old) = self.diag_task.lock().await.take() {
                    old.abort();
                }
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::info!(uri = %uri, "did_close");
        self.workspace.documents.close(&uri);

        // Cancel any pending debounced diagnostics task. Without this, a task
        // armed by the last keystroke can wake after the close and publish
        // ghost squiggles. The in-task `contains` check is the primary guard;
        // this abort is the belt to its braces.
        if let Some(old) = self.diag_task.lock().await.take() {
            old.abort();
        }

        // Targeted composed invalidation — only evict the object from this file (ISSUE-146)
        crate::workspace::on_document_close(&self.workspace, &uri);

        if let Ok(path) = uri.to_file_path() {
            // Don't remove from file_index if project-scoped diagnostics — the file still exists
            let scope = self.workspace.config.read().await.diagnostics_scope;
            if scope != crate::config::DiagnosticsScope::Project {
                self.workspace.file_index.remove_file(&path);
                self.client.publish_diagnostics(uri, vec![], None).await;
            }
        } else {
            self.client.publish_diagnostics(uri, vec![], None).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::debug!(uri = %uri, "did_save");

        // Always publish diagnostics on save — both Continuous and OnSave modes benefit
        // from a save-time refresh. In OnSave mode this is the *only* time diagnostics run
        // (did_change is gated by the trigger check above).
        if let Some(text) = self.workspace.documents.get_text(&uri) {
            diagnostics::publish_diagnostics(self, &uri, &text).await;
        } else {
            tracing::warn!(uri = %uri, "did_save: document not in store, skipping diagnostics");
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        tracing::info!("did_change_configuration");
        let al_settings = extract_al_settings(params.settings);
        let (unknown, cap) = {
            let mut config = self.workspace.config.write().await;
            let unknown = config.merge(&al_settings);
            (unknown, config.max_document_size_bytes)
        };
        if !unknown.is_empty() {
            let msg = format!("Unknown AL settings: {}", unknown.join(", "));
            self.client.show_message(MessageType::WARNING, &msg).await;
        }
        // F-OPEN-042: re-apply the per-document size cap after a config change.
        self.workspace.documents.set_max_doc_bytes(cap);
        tracing::info!("Configuration updated");
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        self.await_ready().await;
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        self.ensure_builtins_loaded().await;
        let start = std::time::Instant::now();
        let result = hover::handle_hover(self, uri, position).await;
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "hover");
        Ok(result)
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        self.await_ready().await;
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        self.ensure_builtins_loaded().await;
        let start = std::time::Instant::now();
        let result = completions::handle_completion(self, uri, position).await;
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
        self.await_ready().await;
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let start = std::time::Instant::now();
        let result = definition::handle_definition(self, uri, position);
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "goto_definition");
        Ok(result)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        self.await_ready().await;
        let uri = params.text_document_position.text_document.uri.clone();
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let start = std::time::Instant::now();

        // T028: run the synchronous reference walk inside `spawn_blocking` so the
        // tokio task can be dropped (via tower-lsp's $/cancelRequest handling)
        // without waiting for the walk to finish. Without this wrapper a pending
        // references query on a large workspace blocks the async task until it
        // completes — the LSP client appears responsive (tower-lsp drops the
        // future) but the CPU is wasted.
        let workspace = Arc::clone(&self.workspace);
        let uri_for_log = uri.clone();
        let result = tokio::task::spawn_blocking(move || {
            let core_pos = position.into();
            let locations = crate::queries::references::references(
                &workspace,
                &uri,
                core_pos,
                include_declaration,
            );
            if locations.is_empty() {
                None
            } else {
                Some(
                    locations
                        .into_iter()
                        .map(Into::into)
                        .collect::<Vec<Location>>(),
                )
            }
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "references spawn_blocking join failed");
            None
        });

        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri_for_log, line = position.line, col = position.character, count, elapsed_us = elapsed.as_micros() as u64, "references");
        Ok(result)
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        self.await_ready().await;
        let uri = params.text_document.uri.clone();
        let start = std::time::Instant::now();
        // T028: spawn_blocking for cancel-friendliness on large files.
        let workspace = Arc::clone(&self.workspace);
        let uri_for_log = uri.clone();
        let hierarchical = self.document_symbol_hierarchical.load(Ordering::Relaxed);
        let result = tokio::task::spawn_blocking(move || {
            crate::queries::symbols::document_symbols(&workspace, &uri).map(|symbols| {
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
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "document_symbol spawn_blocking join failed");
            None
        });
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri_for_log, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "document_symbol");
        Ok(result)
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        self.await_ready().await;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let result = formatting::handle_formatting(self, uri, &params.options);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, edits = count, elapsed_us = elapsed.as_micros() as u64, "formatting");
        Ok(result)
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        self.await_ready().await;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let result = formatting::handle_range_formatting(self, uri, params.range, &params.options);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, edits = count, elapsed_us = elapsed.as_micros() as u64, "range_formatting");
        Ok(result)
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        self.await_ready().await;
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
        self.await_ready().await;
        let uri = params.text_document.uri.clone();
        let start = std::time::Instant::now();
        // T028: spawn_blocking — semantic_tokens_full traverses the entire
        // tree-sitter tree on big AL files; cancellation-friendliness matters.
        let workspace = Arc::clone(&self.workspace);
        let uri_for_log = uri.clone();
        let result = tokio::task::spawn_blocking(move || {
            let tokens = crate::queries::semantic_tokens::semantic_tokens_full(&workspace, &uri);
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
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "semantic_tokens_full spawn_blocking join failed");
            None
        });
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
        self.await_ready().await;
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let start = std::time::Instant::now();
        let result = handlers::handle_signature_help(self, uri, position);
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "signature_help");
        Ok(result)
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        self.await_ready().await;
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
        self.await_ready().await;
        let uri = &params.text_document.uri;

        // ISSUE-072: skip diagnostics for virtual symbol cache files.
        if diagnostics::is_cache_path(uri) {
            tracing::debug!(uri = %uri, "diagnostic (pull): skipping cache file");
            return Ok(diagnostics::full_diagnostic_report(vec![]));
        }

        let text = match self.workspace.documents.get_text_arc(uri) {
            Some(t) => t,
            None => {
                tracing::debug!(uri = %uri, "diagnostic (pull): document not open, returning empty");
                return Ok(diagnostics::full_diagnostic_report(vec![]));
            }
        };

        let start = std::time::Instant::now();
        let diags = diagnostics::compute_diagnostics(self, uri, &text).await;
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, count = diags.len(), elapsed_us = elapsed.as_micros() as u64, "diagnostic (pull)");

        Ok(diagnostics::full_diagnostic_report(diags))
    }

    async fn workspace_diagnostic(
        &self,
        _params: WorkspaceDiagnosticParams,
    ) -> Result<WorkspaceDiagnosticReportResult> {
        // B11: project-scope pull diagnostics. Aggregates parse/syntax errors
        // across every indexed file plus bridge diagnostics for open documents.
        self.await_ready().await;
        let start = std::time::Instant::now();
        let reports = diagnostics::compute_workspace_diagnostics(self).await;
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
        self.await_ready().await;
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name.clone();
        let start = std::time::Instant::now();
        let result = definition::handle_rename(self, uri, position, params.new_name);
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, new_name = %new_name, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "rename");
        Ok(result)
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        self.await_ready().await;
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
        self.await_ready().await;
        let start = std::time::Instant::now();
        let result = workspace::handle_workspace_symbol(self, &params.query);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(query = %params.query, count, elapsed_us = elapsed.as_micros() as u64, "workspace_symbol");
        Ok(result)
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        self.await_ready().await;
        let uri = &params.text_document.uri;
        let range = params.range;
        self.ensure_builtins_loaded().await;
        let start = std::time::Instant::now();
        let result = handlers::handle_inlay_hint(self, uri, range);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, hints = count, elapsed_us = elapsed.as_micros() as u64, "inlay_hint");
        Ok(result)
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        self.await_ready().await;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let entries = crate::queries::code_lens::code_lens(&self.workspace, uri);
        let elapsed = start.elapsed();
        let count = entries.len();
        tracing::debug!(uri = %uri, lenses = count, elapsed_us = elapsed.as_micros() as u64, "code_lens");
        if entries.is_empty() {
            return Ok(None);
        }
        use crate::queries::code_lens::CodeLensKind;
        let lenses: Vec<CodeLens> = entries
            .into_iter()
            .map(|e| {
                // The command id is owned by the lens kind (single source of
                // truth shared with `SUPPORTED_COMMANDS`), so a lens can never
                // emit an id the `execute_command` dispatch doesn't handle (A8).
                let command_id = e.kind.command_id();
                // `data` carries the lens kind + payload so clients can
                // distinguish test lenses; `arguments` makes every lens
                // actionable. Reference/profiler lenses pass `{uri, position}`
                // so the handler can locate the symbol; test lenses pass the
                // codeunit/method to run. Both were previously dropped at this
                // boundary (F-OPEN-270). The payload shape is deliberate — the
                // internal enums are both internally tagged with "kind" and
                // would collide if serialized directly.
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
            "al.getStatus" => Ok(Some(commands::get_status(self).await)),
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
            // CodeLens-backed commands (A8). Each returns `Some(..)` so a click
            // performs the action instead of silently hitting the catch-all.
            "al.findReferences" => Ok(Some(commands::find_references(self, &params.arguments))),
            "al.showProfiler" => Ok(Some(commands::show_profiler(self, &params.arguments))),
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

    let (service, socket) = LspService::new(AlServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
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
        // Skip the 30s workspace-init wait in `await_ready`. `Release` matches
        // the production store and the flag's documented happens-before contract
        // (paired with the `Acquire` load in `await_ready`).
        server.workspace_ready.store(true, Ordering::Release);
        let uri = Url::parse("file:///proj/Outline.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string());
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
        // `Release` pairs with the `Acquire` load in `await_ready`, matching the
        // production store and the flag's documented happens-before contract.
        server.workspace_ready.store(true, Ordering::Release);
        let uri = Url::parse("file:///proj/Def.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string());
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
    //! Gap A8: every CodeLens the server emits must resolve to an
    //! `executeCommand` handler — a clicked lens must perform its action, never
    //! a silent no-op. These tests drive the real `code_lens` + `execute_command`
    //! handlers in-process (no transport) and assert both the structural
    //! invariant (`LENS_COMMAND_IDS ⊆ SUPPORTED_COMMANDS`) and end-to-end that
    //! each emitted lens is dispatched (returns `Some`, not the catch-all's
    //! `None`).

    use super::*;
    use crate::queries::code_lens::LENS_COMMAND_IDS;
    use crate::queries::profiler_hints::{ProfilerHint, ProfilerSession};

    // A test codeunit exercising all three lens kinds: TestBeta is called once
    // (reference lens), both methods are `[Test]` (test lenses), and a profiler
    // session below adds a profiler lens for TestAlpha.
    const SRC: &str = "codeunit 50200 \"My Tests\"\n{\n    Subtype = Test;\n\n    [Test]\n    procedure TestAlpha()\n    begin\n        TestBeta();\n    end;\n\n    [Test]\n    procedure TestBeta()\n    begin\n    end;\n}\n";

    fn build_server() -> (LspService<AlServer>, Url) {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        // Skip the workspace-init wait in `await_ready`.
        server.workspace_ready.store(true, Ordering::Release);
        let uri = Url::parse("file:///proj/MyTests.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string());
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
                "lens command id {id:?} is not advertised/handled in SUPPORTED_COMMANDS — dead lens (A8)"
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
                "emitted lens command {:?} is not handled — dead lens (A8)",
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
                "command {:?} fell through to the no-op catch-all (dead lens, A8)",
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
    //! `workspace/diagnostic` (B11) must report parse/syntax errors across the
    //! whole workspace — both background (never-opened) files and open documents
    //! — and report nothing for a clean workspace. Driven in-process against the
    //! real `AlServer` handler (no transport, no toolchain ⇒ bridge is a no-op,
    //! so these assert the syntax-pass aggregation).

    use super::*;

    const BAD_SRC: &str =
        "codeunit 50100 Test\n{\n    procedure Broken(\n    begin\n    end;\n}\n";
    const GOOD_SRC: &str =
        "codeunit 50100 MyCodeunit\n{\n    trigger OnRun()\n    begin\n    end;\n}\n";

    fn new_ready_server() -> LspService<AlServer> {
        let (service, _socket) = LspService::new(AlServer::new);
        // Skip the 30s workspace-init wait in `await_ready`.
        service.inner().workspace_ready.store(true, Ordering::Release);
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
        crate::workspace::on_document_change(&server.workspace, &uri, BAD_SRC);

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
            .open(uri.clone(), BAD_SRC.to_string());
        crate::workspace::on_document_change(&server.workspace, &uri, BAD_SRC);

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
            .open(open_uri.clone(), GOOD_SRC.to_string());
        crate::workspace::on_document_change(&server.workspace, &open_uri, GOOD_SRC);
        // A second clean file that is only indexed, never opened.
        let bg_uri = Url::parse("file:///proj/BgGood.al").expect("valid uri");
        crate::workspace::on_document_change(&server.workspace, &bg_uri, GOOD_SRC);

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
