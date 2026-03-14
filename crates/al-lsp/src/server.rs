//! AlServer state and LSP lifecycle.

use std::sync::Arc;

use al_core::workspace::Workspace;
use al_syntax::AlParser;
use dashmap::DashMap;
use tokio::sync::RwLock;
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

/// Timeout for interactive bridge calls (hover, completions).
/// Shorter than the default 30s bridge timeout to keep UX snappy.
const BRIDGE_INTERACTIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Call a bridge method with timeout, converting errors to `None`.
///
/// `$call` receives `(bridge, path, pos)` as parameters, keeping borrows
/// in the caller's scope and avoiding the lifetime issues that prevent
/// a generic async wrapper.
macro_rules! bridge_call {
    ($self:expr, $uri:expr, $position:expr, $label:literal, |$b:ident, $p:ident, $ps:ident| $call:expr) => {{
        let guard = $self.get_or_init_bridge().await;
        let guard = match guard {
            Some(g) => g,
            None => return None,
        };
        let $b = match guard.as_ref() {
            Some(b) => b,
            None => return None,
        };
        let $p = match $uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return None,
        };
        let $ps = ($position.line + 1, $position.character + 1);
        match tokio::time::timeout(BRIDGE_INTERACTIVE_TIMEOUT, $call).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, concat!($label, ": error"));
                None
            }
            Err(_) => {
                tracing::debug!(concat!($label, ": timed out"));
                None
            }
        }
    }};
}

/// Map a CodeAnalysis completion kind string to an LSP CompletionItemKind.
fn completion_kind_from_str(s: &str) -> CompletionItemKind {
    match s {
        "Method" | "Function" => CompletionItemKind::METHOD,
        "Property" | "Field" => CompletionItemKind::FIELD,
        "Variable" => CompletionItemKind::VARIABLE,
        "Enum" | "EnumMember" => CompletionItemKind::ENUM_MEMBER,
        "Class" | "Struct" => CompletionItemKind::CLASS,
        "Module" | "Namespace" => CompletionItemKind::MODULE,
        "Keyword" => CompletionItemKind::KEYWORD,
        "Snippet" => CompletionItemKind::SNIPPET,
        _ => CompletionItemKind::TEXT,
    }
}

/// The AL language server.
pub struct AlServer {
    pub(crate) client: Client,
    pub(crate) workspace: Workspace,
    /// Root URI from initialize params, used in initialized().
    pub(crate) root_uri: RwLock<Option<Url>>,
}

impl AlServer {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            workspace: Workspace::new(),
            root_uri: RwLock::new(None),
        }
    }

    /// Update workspace index (object name mapping) for a file.
    /// Also caches the parse tree to avoid double-parsing in diagnostics.
    pub(crate) fn update_workspace_index(&self, uri: &Url, text: &str) {
        let result = AlParser::parse_quick(text);

        // Cache the tree so publish_diagnostics can reuse it
        let version = self.workspace.documents.get_version(uri).unwrap_or(0);
        self.workspace.documents.cache_tree(uri, version, result.tree.clone());

        if let Ok(path) = uri.to_file_path() {
            self.workspace.workspace_files.insert(path.clone(), text.to_string());
            if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, text) {
                let obj_name = obj_info.name.to_lowercase();
                self.workspace.workspace_objects.insert(obj_name.clone(), path.clone());
                self.workspace.file_to_object.insert(path, obj_name);
            }
        }
    }

    /// Load builtins and error codes from disk cache (fast path, no bridge needed).
    pub(crate) async fn load_caches_from_disk(&self, version: &str) {
        if self.workspace.builtins.read().unwrap().is_empty() {
            if let Some(cached) = al_semantic::cache::read_builtins(version) {
                tracing::info!(count = cached.len(), "Loaded built-in types from disk cache");
                *self.workspace.builtins.write().unwrap() = Arc::new(cached);
            }
        }
        if self.workspace.error_codes.read().unwrap().is_empty() {
            if let Some(cached) = al_semantic::cache::read_error_codes(version) {
                tracing::info!(count = cached.len(), "Loaded error codes from disk cache");
                let map = DashMap::new();
                for ec in cached {
                    map.insert(ec.code.clone(), ec.message.clone());
                }
                *self.workspace.error_codes.write().unwrap() = Arc::new(map);
            }
        }
    }

    /// Ensure builtins are loaded. Tries disk cache first, then bridge.
    pub(crate) async fn ensure_builtins_loaded(&self) {
        if !self.workspace.builtins.read().unwrap().is_empty() {
            return;
        }

        // Try to get/init bridge and load builtins
        if let Some(guard) = self.get_or_init_bridge().await {
            if let Some(bridge) = guard.as_ref() {
                match bridge.builtin_types().await {
                    Ok(types) => {
                        tracing::info!(count = types.len(), "Loaded built-in types via bridge");
                        *self.workspace.builtins.write().unwrap() = Arc::new(types);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Failed to load built-in types via bridge");
                    }
                }
            }
        }
    }

    /// Ensure error codes are loaded. Tries disk cache first, then bridge.
    pub(crate) async fn ensure_error_codes_loaded(&self) {
        if !self.workspace.error_codes.read().unwrap().is_empty() {
            return;
        }

        if let Some(guard) = self.get_or_init_bridge().await {
            if let Some(bridge) = guard.as_ref() {
                match bridge.error_codes().await {
                    Ok(codes) => {
                        tracing::info!(count = codes.len(), "Loaded error codes via bridge");
                        let map = DashMap::new();
                        for ec in codes {
                            map.insert(ec.code.clone(), ec.message.clone());
                        }
                        *self.workspace.error_codes.write().unwrap() = Arc::new(map);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Failed to load error codes via bridge");
                    }
                }
            }
        }
    }

    /// Look up an error code description for diagnostic enrichment.
    pub(crate) fn error_code_description(&self, code: &str) -> Option<String> {
        let map = self.workspace.error_codes.read().unwrap();
        map.get(code).map(|v| v.value().clone())
    }

    /// Get the semantic bridge, initializing it lazily if needed.
    ///
    /// Returns None if no toolchain is available or bridge init fails.
    pub(crate) async fn get_or_init_bridge(
        &self,
    ) -> Option<tokio::sync::RwLockReadGuard<'_, Option<al_semantic::SemanticBridge>>> {
        // Fast path: bridge already initialized
        {
            let guard = self.workspace.semantic.read().await;
            if guard.is_some() {
                return Some(guard);
            }
        }

        // Slow path: initialize the bridge
        let toolchain = self.workspace.toolchain.read().await.clone()?;
        let mut write_guard = self.workspace.semantic.write().await;

        // Double-check after acquiring write lock (another task may have init'd)
        if write_guard.is_some() {
            return Some(write_guard.downgrade());
        }

        match al_semantic::SemanticBridge::new(&toolchain) {
            Ok(bridge) => {
                tracing::info!("Semantic bridge initialized (lazy)");
                *write_guard = Some(bridge);
                Some(write_guard.downgrade())
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to initialize semantic bridge");
                None
            }
        }
    }

    /// Bridge fallback for completions — calls CodeAnalysis completions_at.
    async fn bridge_completions(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<Vec<CompletionItem>> {
        let items: Vec<al_semantic::CompletionItem> = bridge_call!(
            self, uri, position, "bridge_completions",
            |bridge, path, pos| bridge.completions_at(&path, pos))?;
        if items.is_empty() {
            return None;
        }

        tracing::debug!(count = items.len(), "bridge_completions: got results from CodeAnalysis");
        let lsp_items = items
            .into_iter()
            .map(|item| {
                let kind = completion_kind_from_str(&item.kind);
                let sort_text = format!("2_{}", item.label.to_ascii_lowercase());
                CompletionItem {
                    label: item.label,
                    kind: Some(kind),
                    detail: item.detail,
                    documentation: item.documentation.map(Documentation::String),
                    sort_text: Some(sort_text),
                    ..Default::default()
                }
            })
            .collect();
        Some(lsp_items)
    }

    /// Bridge fallback for hover — calls CodeAnalysis type_at.
    async fn bridge_hover(&self, uri: &Url, position: Position) -> Option<Hover> {
        let info = bridge_call!(self, uri, position, "bridge_hover",
            |bridge, path, pos| bridge.type_at(&path, pos))?;
        let info = info?; // type_at returns Option<TypeAtInfo>

        tracing::debug!(name = %info.name, kind = %info.kind, "bridge_hover: got type info from CodeAnalysis");
        let mut value = format!("```al\n{}\n```\n*({} — CodeAnalysis)*", info.name, info.kind);
        if let Some(doc) = &info.documentation {
            value.push_str("\n\n");
            value.push_str(doc);
        }
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: None,
        })
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

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
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
                                token_modifiers: vec![],
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
                inlay_hint_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    ..Default::default()
                }),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
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
        self.client
            .log_message(MessageType::INFO, "AL Language Server initialized")
            .await;

        // Use the root URI stored during initialize()
        let root_uri = self.root_uri.read().await.clone();
        tracing::info!(root_uri = ?root_uri, "initialized: starting workspace init");
        workspace::initialize_workspace(self, root_uri.as_ref()).await;
    }

    async fn shutdown(&self) -> Result<()> {
        // Drop the semantic bridge (CLR shuts down with it)
        let _ = self.workspace.semantic.write().await.take();
        Ok(())
    }

    // -- Text document synchronization --

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let text = params.text_document.text.clone();
        tracing::info!(uri = %uri, len = text.len(), "did_open");

        self.workspace.documents.open(uri.clone(), params.text_document.text);
        self.update_workspace_index(&uri, &text);

        diagnostics::publish_diagnostics(self, &uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        tracing::debug!(uri = %uri, change_count = params.content_changes.len(), "did_change");

        // Convert LSP types → al-core types at the boundary
        let changes: Vec<al_core::documents::TextChange> = params.content_changes.iter().map(|c| {
            al_core::documents::TextChange {
                range: c.range.map(|r| al_core::documents::TextRange {
                    start_line: r.start.line,
                    start_character: r.start.character,
                    end_line: r.end.line,
                    end_character: r.end.character,
                }),
                text: c.text.clone(),
            }
        }).collect();
        self.workspace.documents.apply_changes(&uri, &changes);

        if let Some(text) = self.workspace.documents.get_text(&uri) {
            self.update_workspace_index(&uri, &text);
            diagnostics::publish_diagnostics(self, &uri, &text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::info!(uri = %uri, "did_close");
        self.workspace.documents.close(&uri);

        if let Ok(path) = uri.to_file_path() {
            self.workspace.workspace_files.remove(&path);
            // O(1) removal via reverse index instead of O(N) retain
            if let Some((_, obj_name)) = self.workspace.file_to_object.remove(&path) {
                self.workspace.workspace_objects.remove(&obj_name);
            }
        }

        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    // -- Hover --

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        self.ensure_builtins_loaded().await;
        let start = std::time::Instant::now();
        let mut result = hover::handle_hover(self, uri, position);

        // Bridge fallback: if native resolution found nothing, try .NET type_at
        if result.is_none() {
            if let Some(hover) = self.bridge_hover(uri, position).await {
                result = Some(hover);
            }
        }

        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "hover");
        Ok(result)
    }

    // -- Completion --

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        self.ensure_builtins_loaded().await;
        let start = std::time::Instant::now();
        let mut result = completions::handle_completion(self, uri, position);

        // Bridge fallback: if native returned nothing for a member access context,
        // try CodeAnalysis completions
        if result.is_none() {
            if let Some(text) = self.workspace.documents.get_text(uri) {
                let ctx = al_syntax::context::detect_context(&text, position);
                if matches!(ctx, al_syntax::context::CompletionContext::MemberAccess) {
                    if let Some(items) = self.bridge_completions(uri, position).await {
                        result = Some(CompletionResponse::Array(items));
                    }
                }
            }
        }

        let elapsed = start.elapsed();
        let count = result.as_ref().map(|r| match r {
            CompletionResponse::Array(v) => v.len(),
            CompletionResponse::List(l) => l.items.len(),
        }).unwrap_or(0);
        tracing::debug!(uri = %uri, line = position.line, col = position.character, count, elapsed_us = elapsed.as_micros() as u64, "completion");
        Ok(result)
    }

    // -- Go-to-definition --

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
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
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let result = handlers::handle_document_symbol(self, uri);
        let elapsed = start.elapsed();
        tracing::debug!(uri = %uri, found = result.is_some(), elapsed_us = elapsed.as_micros() as u64, "document_symbol");
        Ok(result)
    }

    // -- Formatting --

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let result = formatting::handle_formatting(self, uri, &params.options);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, edits = count, elapsed_us = elapsed.as_micros() as u64, "formatting");
        Ok(result)
    }

    // -- Folding ranges --

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
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
        let uri = &params.text_document.uri;
        let start = std::time::Instant::now();
        let result = handlers::handle_semantic_tokens(self, uri);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|r| match r {
            SemanticTokensResult::Tokens(t) => t.data.len(),
            SemanticTokensResult::Partial(t) => t.data.len(),
        }).unwrap_or(0);
        tracing::debug!(uri = %uri, tokens = count, elapsed_us = elapsed.as_micros() as u64, "semantic_tokens_full");
        Ok(result)
    }

    // -- Signature help --

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
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

    // -- Rename --

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
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
        let start = std::time::Instant::now();
        let result = workspace::handle_workspace_symbol(self, &params.query);
        let elapsed = start.elapsed();
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(query = %params.query, count, elapsed_us = elapsed.as_micros() as u64, "workspace_symbol");
        Ok(result)
    }

    // -- Inlay hints --

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
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

    // -- Execute command --

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<serde_json::Value>> {
        tracing::info!(command = %params.command, "execute_command");

        let start = std::time::Instant::now();
        let result = match params.command.as_str() {
            "al.downloadSymbols" => {
                workspace::download_symbols_command(self, workspace::DownloadSource::Server).await;
                Ok(None)
            }
            "al.downloadSymbolsServer" => {
                workspace::download_symbols_command(self, workspace::DownloadSource::Server).await;
                Ok(None)
            }
            "al.downloadSymbolsNuget" => {
                workspace::download_symbols_command(self, workspace::DownloadSource::NuGet).await;
                Ok(None)
            }
            "al.clearSymbolCache" => {
                let cache_dir = al_symbols::virtual_file::cache_dir();
                match std::fs::remove_dir_all(&cache_dir) {
                    Ok(()) => {
                        tracing::info!(path = ?cache_dir, "Cleared symbol cache");
                        self.client
                            .show_message(MessageType::INFO, "Symbol cache cleared")
                            .await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to clear symbol cache");
                        self.client
                            .show_message(MessageType::WARNING, format!("Failed to clear cache: {e}"))
                            .await;
                    }
                }
                Ok(None)
            }
            "al.formatFile" => {
                // Formatting is now handled as a CodeAction with WorkspaceEdit directly in handlers.rs.
                // This command is kept for backward compatibility or direct calls.
                if let Some(uri) = params.arguments.first().and_then(|v| serde_json::from_value::<Url>(v.clone()).ok()) {
                    if let Some(edits) = formatting::handle_formatting(self, &uri, &FormattingOptions {
                        tab_size: 4,
                        insert_spaces: true,
                        ..Default::default()
                    }) {
                        let mut changes = std::collections::HashMap::new();
                        changes.insert(uri.clone(), edits);
                        self.client.apply_edit(WorkspaceEdit {
                            changes: Some(changes),
                            ..Default::default()
                        }).await.ok();
                    }
                }
                Ok(None)
            }
            "al.lintFile" => {
                if let Some(uri) = params.arguments.first().and_then(|v| serde_json::from_value::<Url>(v.clone()).ok()) {
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
                let workspace_files = self.workspace.workspace_files.len();
                let workspace_objects = self.workspace.workspace_objects.len();
                let builtins = self.workspace.builtins.read().unwrap().len();

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
                    tracing::info!(root = %uri, "Reindexing workspace");
                    self.client
                        .log_message(MessageType::INFO, "Reindexing workspace...")
                        .await;
                    workspace::initialize_workspace(self, Some(uri)).await;
                    self.client
                        .show_message(MessageType::INFO, "Workspace reindex complete")
                        .await;
                } else {
                    self.client
                        .show_message(MessageType::WARNING, "No workspace root — cannot reindex")
                        .await;
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
/// Uses `LspService::new` to get a real `Client` without starting I/O.
#[cfg(test)]
pub(crate) fn test_server() -> Arc<AlServer> {
    let (service, _socket) = LspService::new(AlServer::new);
    Arc::new(AlServer::new(service.inner().client.clone()))
}

/// Run the LSP server on stdin/stdout.
pub async fn run_lsp() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(AlServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
