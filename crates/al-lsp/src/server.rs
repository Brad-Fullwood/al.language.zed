//! AlServer state and LSP lifecycle.

use al_core::syntax::AlParser;
use al_core::workspace::Workspace;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::completions;
use crate::definition;
use crate::diagnostics;
use crate::formatting;
use crate::handlers;
use crate::hover;
use crate::workspace;

/// Debounce delay for diagnostics: wait this long after the last keystroke before running.
/// ISSUE-025 fix: prevents bridge calls (up to 5s) from blocking hover/completion.
const DIAGNOSTICS_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

/// The AL language server.
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
    /// Guard against double-initialization (ISSUE-073).
    /// Zed may send `initialized` twice when opening multiple worktrees.
    /// CAS ensures workspace init runs only once per server instance.
    pub(crate) init_done: AtomicBool,
    /// Set to `true` inside the spawned task, after `initialize_workspace` completes.
    /// `await_ready` checks this flag, NOT `init_done`, so it only returns once the
    /// background work has actually finished (not just been scheduled).
    pub(crate) workspace_ready: Arc<AtomicBool>,
    /// Notified when workspace initialization completes.
    /// Handlers that need the workspace ready await this before proceeding.
    pub(crate) init_notify: Arc<Notify>,
}

impl AlServer {
    pub(crate) fn new(client: Client) -> Self {
        let workspace = Arc::new(Workspace::new());

        // Register a notify sink so al-core can surface bridge failures to the user.
        // The closure spawns a task to fire-and-forget the async show_message call.
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
            init_done: AtomicBool::new(false),
            workspace_ready: Arc::new(AtomicBool::new(false)),
            init_notify: Arc::new(Notify::new()),
        }
    }

    /// Await workspace initialization.
    ///
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
        // Subscribe *before* the flag check so we cannot miss a wakeup fired
        // between the check and the await.
        let notified = self.init_notify.notified();
        if self.workspace_ready.load(Ordering::Acquire) {
            return; // Already initialized
        }
        if tokio::time::timeout(std::time::Duration::from_secs(30), notified)
            .await
            .is_err()
        {
            tracing::warn!("await_ready: timed out after 30s waiting for workspace initialization");
        }
    }

    /// Update workspace index (object name mapping) for a file.
    /// Also caches the parse tree to avoid double-parsing in diagnostics.
    pub(crate) fn update_workspace_index(&self, uri: &Url, text: &str) {
        let result = AlParser::parse_quick(text);

        // Cache the tree so publish_diagnostics can reuse it
        let version = self.workspace.documents.get_version(uri).unwrap_or(0);
        self.workspace
            .documents
            .cache_tree(uri, version, result.tree.clone());

        if let Ok(path) = uri.to_file_path() {
            self.workspace
                .file_index
                .add_file(path.clone(), text.to_string());
            // Invalidate only the composed view for the object in this file (ISSUE-146).
            // add_file already updated object_info, so we can read the name immediately.
            invalidate_composed_for_file(&self.workspace, &path);
        } else {
            self.workspace.symbols.invalidate_all_composed();
        }
    }

    /// Ensure builtins are loaded. Tries disk cache first, then bridge.
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

        // Try to get/init bridge and load builtins
        if let Some(guard) = self.get_or_init_bridge().await {
            if let Some(bridge) = guard.as_ref() {
                match bridge.builtin_types().await {
                    Ok(types) => {
                        tracing::info!(count = types.len(), "Loaded built-in types via bridge");
                        let version = bridge.version().to_string();
                        al_core::semantic::set_builtins(&self.workspace, types, &version);
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

    /// Ensure error codes are loaded. Tries disk cache first, then bridge.
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

    /// Get the semantic bridge, initializing it lazily if needed.
    ///
    /// Delegates to `al_core::semantic::get_or_init_bridge`.
    pub(crate) async fn get_or_init_bridge(
        &self,
    ) -> Option<tokio::sync::RwLockReadGuard<'_, Option<al_core::semantic_types::SemanticBridge>>>
    {
        al_core::semantic::get_or_init_bridge(&self.workspace).await
    }

    /// Schedule debounced diagnostics for `uri` with the given document text.
    ///
    /// ISSUE-025 fix: Cancels the previous pending task (if any) so that only
    /// the most recent keystroke triggers a diagnostics run. The actual diagnostics
    /// publish runs after `DIAGNOSTICS_DEBOUNCE` of silence. This prevents bridge
    /// calls (up to bridge timeout = 5s) from blocking hover/completion.
    async fn schedule_diagnostics(&self, uri: Url, text: String) {
        // ISSUE-072: skip diagnostics for virtual symbol cache files — they are not
        // workspace files and Zed logs a warning for every publishDiagnostics on them.
        if crate::diagnostics::is_cache_path(&uri) {
            tracing::debug!(uri = %uri, "schedule_diagnostics: skipping cache file");
            return;
        }

        // Cancel previous pending task
        if let Some(old) = self.diag_task.lock().await.take() {
            old.abort();
        }

        // Read config before spawning so per-rule lint filtering works inside the closure.
        let config = self.workspace.config.read().await.clone();

        let client = self.client.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(DIAGNOSTICS_DEBOUNCE).await;
            // Emit syntax-only diagnostics from the debounced task.
            // Bridge diagnostics (semantic) are emitted on did_open and lintFile command.
            let parse_result = al_core::syntax::AlParser::parse_quick(&text);
            let mut lsp_diags: Vec<Diagnostic> = Vec::new();
            let source = text.as_bytes();
            for err in &parse_result.errors {
                lsp_diags.push(crate::diagnostics::syntax_error_to_diagnostic(err, source));
            }
            let lint_result = al_core::syntax::lint(&parse_result.tree, &text);
            for lint in &lint_result {
                if config.is_lint_rule_enabled(&lint.code) {
                    lsp_diags.push(crate::diagnostics::lint_to_diagnostic(lint, source));
                }
            }
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

/// Invalidate the composed symbol cache for the object declared in `path`.
///
/// If `path` maps to a known object, only that object's composed entry is evicted;
/// otherwise the full composed cache is cleared as a safe fallback (ISSUE-146).
///
/// Called by both `update_workspace_index` (on edit) and `did_close` (on close)
/// so the logic is defined in one place.
fn invalidate_composed_for_file(workspace: &al_core::workspace::Workspace, path: &std::path::Path) {
    if let Some(info) = workspace.file_index.object_info.get(path) {
        workspace.symbols.invalidate_composed(&info.name);
    } else {
        workspace.symbols.invalidate_all_composed();
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

        // Store root URI for use in initialized()
        *self.root_uri.write().await = root_uri;

        // Parse initialization options into config
        if let Some(init_opts) = params.initialization_options {
            let al_settings = extract_al_settings(init_opts);
            let unknown = self.workspace.config.write().await.merge(&al_settings);
            if !unknown.is_empty() {
                tracing::warn!("Unknown settings in initializationOptions: {:?}", unknown);
            }
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
                                token_types: al_core::syntax::tokens::token_types::LEGEND
                                    .iter()
                                    .map(|s| SemanticTokenType::new(s))
                                    .collect(),
                                token_modifiers: al_core::syntax::tokens::token_modifiers::LEGEND
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
                // workspace_diagnostics is false because we only support per-document pull;
                // a workspace/diagnostic handler is not yet implemented.
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions {
                        identifier: Some("al-lsp".to_string()),
                        inter_file_dependencies: true,
                        workspace_diagnostics: false,
                        work_done_progress_options: WorkDoneProgressOptions::default(),
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "al.downloadSymbols".to_string(),
                        "al.downloadSymbolsServer".to_string(),
                        "al.downloadSymbolsNuget".to_string(),
                        "al.clearSymbolCache".to_string(),
                        "al.formatFile".to_string(),
                        "al.lintFile".to_string(),
                        "al.getStatus".to_string(),
                        "al.reindex".to_string(),
                        "al.compile".to_string(),
                        "al.applyRecommendedSettings".to_string(),
                    ],
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

        // Use the root URI stored during initialize()
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
            workspace::initialize_workspace(ws, client, root_uri).await;
            // Set workspace_ready BEFORE notify_waiters so that any waiter that
            // re-checks the flag after waking always sees true.
            ready_flag.store(true, Ordering::Release);
            notify.notify_waiters();
        });
        *self.init_task.lock().await = Some(handle);
    }

    async fn shutdown(&self) -> Result<()> {
        // Abort any pending diagnostics task
        if let Some(task) = self.diag_task.lock().await.take() {
            task.abort();
        }
        // Abort background workspace init if still running
        if let Some(task) = self.init_task.lock().await.take() {
            task.abort();
        }
        al_core::semantic::shutdown_bridge(&self.workspace).await;
        Ok(())
    }

    // -- Text document synchronization --

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let text = params.text_document.text.clone();
        tracing::info!(uri = %uri, len = text.len(), "did_open");

        self.workspace
            .documents
            .open(uri.clone(), params.text_document.text);
        self.update_workspace_index(&uri, &text);

        diagnostics::publish_diagnostics(self, &uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        tracing::debug!(uri = %uri, change_count = params.content_changes.len(), "did_change");

        // Convert LSP types → al-core types at the boundary
        let changes: Vec<al_core::documents::TextChange> = params
            .content_changes
            .iter()
            .map(|c| al_core::documents::TextChange {
                range: c.range.map(|r| al_core::documents::TextRange {
                    start_line: r.start.line,
                    start_character: r.start.character,
                    end_line: r.end.line,
                    end_character: r.end.character,
                }),
                text: c.text.clone(),
            })
            .collect();
        self.workspace.documents.apply_changes(&uri, &changes);

        if let Some(text) = self.workspace.documents.get_text(&uri) {
            self.update_workspace_index(&uri, &text);
            // ISSUE-025 fix: diagnostics are debounced and run async.
            // Each keystroke cancels the previous pending task to avoid bridge calls
            // (up to bridge timeout = 5s) blocking hover/completion.
            //
            // Only schedule per-keystroke diagnostics when trigger is Continuous.
            // In OnSave mode, diagnostics are deferred to did_save to avoid per-keystroke work.
            let trigger = self.workspace.config.read().await.diagnostics_trigger;
            if trigger == al_core::config::DiagnosticsTrigger::Continuous {
                self.schedule_diagnostics(uri, text).await;
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

        if let Ok(path) = uri.to_file_path() {
            // Targeted composed invalidation — only evict the object from this file (ISSUE-146)
            invalidate_composed_for_file(&self.workspace, &path);
            // Don't remove from file_index if project-scoped diagnostics — the file still exists
            let scope = self.workspace.config.read().await.diagnostics_scope;
            if scope != al_core::config::DiagnosticsScope::Project {
                self.workspace.file_index.remove_file(&path);
                self.client.publish_diagnostics(uri, vec![], None).await;
            }
            // If project-scoped, diagnostics persist (file is still in the project)
        } else {
            self.workspace.symbols.invalidate_all_composed();
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
        // Zed sends settings nested under "al" key, or as a flat object
        let al_settings = extract_al_settings(params.settings);
        let unknown = self.workspace.config.write().await.merge(&al_settings);
        if !unknown.is_empty() {
            let msg = format!("Unknown AL settings: {}", unknown.join(", "));
            self.client.show_message(MessageType::WARNING, &msg).await;
        }
        tracing::info!("Configuration updated");
    }

    // -- Hover --

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

    // -- Completion --

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

    // -- Go-to-definition --

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

    // -- References --

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        self.await_ready().await;
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let start = std::time::Instant::now();
        let result = definition::handle_references(self, uri, position, include_declaration);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, line = position.line, col = position.character, count, elapsed_us = elapsed.as_micros() as u64, "references");
        Ok(result)
    }

    // -- Document symbols --

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        self.await_ready().await;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let result = handlers::handle_document_symbol(self, uri);
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "document_symbol");
        Ok(result)
    }

    // -- Formatting --

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

    // -- Folding ranges --

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

    // -- Semantic tokens --

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        self.await_ready().await;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let result = handlers::handle_semantic_tokens(self, uri);
        let elapsed = start.elapsed();
        let count = result
            .as_ref()
            .map(|r| match r {
                SemanticTokensResult::Tokens(t) => t.data.len(),
                SemanticTokensResult::Partial(t) => t.data.len(),
            })
            .unwrap_or(0);
        tracing::debug!(uri = %uri, tokens = count, elapsed_us = elapsed.as_micros() as u64, "semantic_tokens_full");
        Ok(result)
    }

    // -- Signature help --

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

    // -- Code actions --

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

    // -- Pull diagnostics --

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

    // -- Rename --

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

    // -- Workspace symbols --

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

    // -- Inlay hints --

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

    // -- Code lens --

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        self.await_ready().await;
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let entries = al_core::queries::code_lens::code_lens(&self.workspace, uri);
        let elapsed = start.elapsed();
        let count = entries.len();
        tracing::debug!(uri = %uri, lenses = count, elapsed_us = elapsed.as_micros() as u64, "code_lens");
        if entries.is_empty() {
            return Ok(None);
        }
        let lenses: Vec<CodeLens> = entries
            .into_iter()
            .map(|e| CodeLens {
                range: e.range.into(),
                command: Some(Command {
                    title: e.title,
                    command: "al.findReferences".to_string(),
                    arguments: None,
                }),
                data: None,
            })
            .collect();
        Ok(Some(lenses))
    }

    // -- Execute command --

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
                let cache_dir = al_core::symbols::virtual_file::cache_dir();
                match tokio::fs::remove_dir_all(&cache_dir).await {
                    Ok(()) => {
                        tracing::info!(path = ?cache_dir, "Cleared symbol cache");
                        self.client
                            .show_message(MessageType::INFO, "Symbol cache cleared")
                            .await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to clear symbol cache");
                        self.client
                            .show_message(
                                MessageType::WARNING,
                                format!("Failed to clear cache: {e}"),
                            )
                            .await;
                    }
                }
                Ok(None)
            }
            "al.formatFile" => {
                // Formatting is now handled as a CodeAction with WorkspaceEdit directly in handlers.rs.
                // This command is kept for backward compatibility or direct calls.
                // SILENT: .ok() on from_value — invalid argument from client is not user-affecting
                if let Some(uri) = params
                    .arguments
                    .first()
                    .and_then(|v| serde_json::from_value::<Url>(v.clone()).ok())
                {
                    if let Some(edits) = formatting::handle_formatting(
                        self,
                        &uri,
                        &FormattingOptions {
                            tab_size: 4,
                            insert_spaces: true,
                            ..Default::default()
                        },
                    ) {
                        let mut changes = std::collections::HashMap::new();
                        changes.insert(uri.clone(), edits);
                        // SILENT: apply_edit failure is logged by tower-lsp internally
                        self.client
                            .apply_edit(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            })
                            .await
                            .ok();
                    }
                }
                Ok(None)
            }
            "al.lintFile" => {
                // SILENT: .ok() on from_value — invalid argument from client is not user-affecting
                if let Some(uri) = params
                    .arguments
                    .first()
                    .and_then(|v| serde_json::from_value::<Url>(v.clone()).ok())
                {
                    if let Some(text) = self.workspace.documents.get_text(&uri) {
                        diagnostics::publish_diagnostics(self, &uri, &text).await;
                    }
                }
                Ok(None)
            }
            "al.getStatus" => {
                let has_bridge = self.workspace.semantic.read().await.is_some();
                let has_toolchain = self.workspace.toolchain.read().await.is_some();
                let indexed_symbols = self.workspace.symbols.len();
                let workspace_files = self.workspace.file_index.len();
                let workspace_objects = self.workspace.file_index.objects.len();
                let builtins = self
                    .workspace
                    .builtins
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .len(); // SILENT: recover from RwLock poison

                Ok(Some(serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "pid": std::process::id(),
                    "semanticBridge": has_bridge,
                    "toolchain": has_toolchain,
                    "indexedSymbols": indexed_symbols,
                    "workspaceFiles": workspace_files,
                    "workspaceObjects": workspace_objects,
                    "builtinTypes": builtins,
                })))
            }
            "al.reindex" => {
                let root_uri = self.root_uri.read().await.clone();
                if let Some(uri) = &root_uri {
                    tracing::info!(root = %uri, "Reindexing workspace (background)");
                    let ws = Arc::clone(&self.workspace);
                    let client = self.client.clone();
                    let uri_cloned = uri.clone();
                    tokio::spawn(async move {
                        workspace::initialize_workspace(ws, client.clone(), Some(uri_cloned)).await;
                        client
                            .show_message(MessageType::INFO, "Workspace reindex complete")
                            .await;
                    });
                } else {
                    self.client
                        .show_message(MessageType::WARNING, "No workspace root — cannot reindex")
                        .await;
                }
                Ok(None)
            }
            // al.compile — run alc and publish per-file diagnostics as publishDiagnostics.
            // ISSUE-075 fix: compile errors now show as Zed editor squiggles, not only terminal output.
            "al.compile" => {
                let toolchain_guard = self.workspace.toolchain.read().await;
                let project_guard = self.workspace.project.read().await;
                let toolchain = toolchain_guard.clone();
                let project_root = project_guard.as_ref().map(|p| p.root.clone());
                drop(toolchain_guard);
                drop(project_guard);

                match (toolchain, project_root) {
                    (Some(tc), Some(root)) => {
                        match al_core::build::compile_project(&tc, &root, None).await {
                            Ok(result) => {
                                // Group compile diagnostics by file and publish per-file.
                                let mut by_file: std::collections::HashMap<
                                    String,
                                    Vec<Diagnostic>,
                                > = std::collections::HashMap::new();
                                for d in &result.diagnostics {
                                    let severity = match d.severity {
                                        al_core::build::DiagnosticSeverity::Error => {
                                            DiagnosticSeverity::ERROR
                                        }
                                        al_core::build::DiagnosticSeverity::Warning => {
                                            DiagnosticSeverity::WARNING
                                        }
                                        al_core::build::DiagnosticSeverity::Info => {
                                            DiagnosticSeverity::INFORMATION
                                        }
                                    };
                                    let start_line = d.line.saturating_sub(1);
                                    let start_char = d.column.saturating_sub(1);
                                    let lsp_diag = Diagnostic {
                                        range: Range {
                                            start: Position {
                                                line: start_line,
                                                character: start_char,
                                            },
                                            // alc only reports start position; extend to end of line
                                            // so editors show a visible underline (u32::MAX → EOL).
                                            end: Position {
                                                line: start_line,
                                                character: u32::MAX,
                                            },
                                        },
                                        severity: Some(severity),
                                        code: Some(NumberOrString::String(d.code.clone())),
                                        source: Some("al-compiler".to_string()),
                                        message: d.message.clone(),
                                        ..Default::default()
                                    };
                                    by_file.entry(d.file.clone()).or_default().push(lsp_diag);
                                }
                                for (file, diags) in by_file {
                                    if let Ok(uri) = Url::from_file_path(&file) {
                                        self.client.publish_diagnostics(uri, diags, None).await;
                                    }
                                }
                                if result.success {
                                    self.client
                                        .show_message(MessageType::INFO, "Compilation succeeded")
                                        .await;
                                }
                            }
                            Err(e) => {
                                self.client
                                    .show_message(
                                        MessageType::ERROR,
                                        format!("Compilation error: {e}"),
                                    )
                                    .await;
                            }
                        }
                    }
                    (None, _) => {
                        self.client
                            .show_message(
                                MessageType::WARNING,
                                "No AL toolchain configured — run 'al setup' first",
                            )
                            .await;
                    }
                    (_, None) => {
                        self.client
                            .show_message(
                                MessageType::WARNING,
                                "No AL project loaded — open an AL workspace first",
                            )
                            .await;
                    }
                }
                Ok(None)
            }
            "al.applyRecommendedSettings" => {
                let result =
                    tokio::task::spawn_blocking(crate::workspace::apply_recommended_settings)
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r.map_err(|e| e.to_string()));
                match result {
                    Ok(()) => {
                        self.client
                            .show_message(
                                MessageType::INFO,
                                "Applied recommended AL settings. Reload Zed to activate.",
                            )
                            .await;
                    }
                    Err(e) => {
                        self.client
                            .show_message(
                                MessageType::WARNING,
                                format!("Failed to apply settings: {e}"),
                            )
                            .await;
                    }
                }
                Ok(None)
            }
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

/// Create a test server instance (available only in tests).
///
/// Returns the `LspService` and `ClientSocket` directly so callers access the
/// server via `service.inner()` — no second `AlServer` constructed and discarded.
#[cfg(test)]
pub(crate) fn test_server() -> (LspService<AlServer>, tower_lsp::ClientSocket) {
    LspService::new(AlServer::new)
}

/// Run the LSP server on stdin/stdout.
pub async fn run_lsp() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(AlServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
