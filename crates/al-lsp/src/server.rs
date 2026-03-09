//! AlServer state and LSP lifecycle.

use std::path::PathBuf;
use std::sync::Arc;

use al_discovery::{AlProject, AlToolchain};
use al_semantic::BuiltinType;
use al_symbols::SymbolIndex;
use al_syntax::AlParser;
use dashmap::DashMap;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::completions;
use crate::definition;
use crate::diagnostics;
use crate::document::DocumentStore;
use crate::formatting;
use crate::handlers;
use crate::hover;
use crate::workspace;

/// The AL language server.
pub struct AlServer {
    pub(crate) client: Client,
    pub(crate) parser: std::sync::Mutex<AlParser>,
    pub(crate) symbols: Arc<SymbolIndex>,
    pub(crate) semantic: RwLock<Option<al_semantic::SemanticBridge>>,
    pub(crate) toolchain: RwLock<Option<AlToolchain>>,
    pub(crate) project: RwLock<Option<AlProject>>,
    pub(crate) documents: DocumentStore,
    pub(crate) workspace_files: DashMap<PathBuf, String>,
    /// Builtins loaded once at init, read-only afterward. Arc for cheap cloning.
    pub(crate) builtins: std::sync::RwLock<Arc<Vec<BuiltinType>>>,
    /// Object name -> file path index for fast workspace lookups.
    pub(crate) workspace_objects: DashMap<String, PathBuf>,
    /// Root URI from initialize params, used in initialized().
    pub(crate) root_uri: RwLock<Option<Url>>,
}

impl AlServer {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            parser: std::sync::Mutex::new(AlParser::new()),
            symbols: Arc::new(SymbolIndex::new()),
            semantic: RwLock::new(None),
            toolchain: RwLock::new(None),
            project: RwLock::new(None),
            documents: DocumentStore::new(),
            workspace_files: DashMap::new(),
            builtins: std::sync::RwLock::new(Arc::new(Vec::new())),
            workspace_objects: DashMap::new(),
            root_uri: RwLock::new(None),
        }
    }

    pub(crate) async fn ensure_builtins_loaded(&self) {
        if !self.builtins.read().unwrap().is_empty() {
            return;
        }

        let semantic = self.semantic.read().await;
        let Some(bridge) = semantic.as_ref() else {
            return;
        };

        match bridge.builtin_types().await {
            Ok(types) => {
                tracing::info!(count = types.len(), "Loaded built-in types on demand");
                *self.builtins.write().unwrap() = Arc::new(types);
            }
            Err(error) => {
                tracing::warn!(%error, "Failed to load built-in types on demand");
            }
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
                    commands: vec!["al.downloadSymbols".to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "al-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
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
        // Shut down the semantic bridge if running
        let bridge = self.semantic.write().await.take();
        if let Some(bridge) = bridge {
            bridge.shutdown().await;
        }
        Ok(())
    }

    // -- Text document synchronization --

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let text = params.text_document.text.clone();
        tracing::info!(uri = %uri, len = text.len(), "did_open");

        self.documents.open(uri.clone(), params.text_document.text);

        // Update workspace files map and object name index
        if let Ok(path) = uri.to_file_path() {
            self.workspace_files.insert(path.clone(), text.clone());
            // Update workspace object name index
            let mut parser = self.parser.lock().unwrap();
            let result = parser.parse(&text);
            if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &text) {
                self.workspace_objects
                    .insert(obj_info.name.to_lowercase(), path);
            }
        }

        // Publish diagnostics
        diagnostics::publish_diagnostics(self, &uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        tracing::debug!(uri = %uri, changes = params.content_changes.len(), "did_change");

        self.documents.apply_changes(&uri, &params.content_changes);

        // Get updated text for diagnostics
        if let Some(text) = self.documents.get_text(&uri) {
            // Update workspace files map and object name index
            if let Ok(path) = uri.to_file_path() {
                self.workspace_files.insert(path.clone(), text.clone());
                // Update workspace object name index
                let mut parser = self.parser.lock().unwrap();
                let result = parser.parse(&text);
                if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &text) {
                    self.workspace_objects
                        .insert(obj_info.name.to_lowercase(), path);
                }
            }

            // Re-publish diagnostics
            diagnostics::publish_diagnostics(self, &uri, &text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::info!(uri = %uri, "did_close");
        self.documents.close(&uri);

        // Remove from workspace_files to free memory (will be re-read if needed)
        if let Ok(path) = uri.to_file_path() {
            self.workspace_files.remove(&path);
            // Remove from workspace_objects index (retain entries that don't point to this path)
            self.workspace_objects.retain(|_, v| *v != path);
        }

        // Clear diagnostics for the closed file
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    // -- Hover --

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        self.ensure_builtins_loaded().await;
        let result = hover::handle_hover(self, uri, position);
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), "hover");
        Ok(result)
    }

    // -- Completion --

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        self.ensure_builtins_loaded().await;
        let result = completions::handle_completion(self, uri, position);
        let count = result.as_ref().map(|r| match r {
            CompletionResponse::Array(v) => v.len(),
            CompletionResponse::List(l) => l.items.len(),
        }).unwrap_or(0);
        tracing::debug!(uri = %uri, line = position.line, col = position.character, count, "completion");
        Ok(result)
    }

    // -- Go-to-definition --

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let result = definition::handle_definition(self, uri, position);
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), "goto_definition");
        Ok(result)
    }

    // -- References --

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let result = definition::handle_references(self, uri, position, include_declaration);
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, line = position.line, col = position.character, count, "references");
        Ok(result)
    }

    // -- Document symbols --

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let result = handlers::handle_document_symbol(self, uri);
        tracing::debug!(uri = %uri, found = result.is_some(), "document_symbol");
        Ok(result)
    }

    // -- Formatting --

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let result = formatting::handle_formatting(self, uri, &params.options);
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, edits = count, "formatting");
        Ok(result)
    }

    // -- Folding ranges --

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        let result = handlers::handle_folding_range(self, uri);
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, ranges = count, "folding_range");
        Ok(result)
    }

    // -- Semantic tokens --

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let result = handlers::handle_semantic_tokens(self, uri);
        let count = result.as_ref().map(|r| match r {
            SemanticTokensResult::Tokens(t) => t.data.len(),
            SemanticTokensResult::Partial(t) => t.data.len(),
        }).unwrap_or(0);
        tracing::debug!(uri = %uri, tokens = count, "semantic_tokens_full");
        Ok(result)
    }

    // -- Signature help --

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let result = handlers::handle_signature_help(self, uri, position);
        tracing::debug!(uri = %uri, line = position.line, col = position.character, found = result.is_some(), "signature_help");
        Ok(result)
    }

    // -- Code actions --

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let range = params.range;
        let diagnostics = &params.context.diagnostics;
        let result = handlers::handle_code_action(self, uri, range, diagnostics);
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, actions = count, "code_action");
        Ok(result)
    }

    // -- Rename --

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name.clone();
        let result = definition::handle_rename(self, uri, position, params.new_name);
        tracing::debug!(uri = %uri, new_name = %new_name, found = result.is_some(), "rename");
        Ok(result)
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = &params.text_document.uri;
        let position = params.position;
        Ok(definition::handle_prepare_rename(self, uri, position))
    }

    // -- Workspace symbols --

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let result = workspace::handle_workspace_symbol(self, &params.query);
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(query = %params.query, count, "workspace_symbol");
        Ok(result)
    }

    // -- Inlay hints --

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let range = params.range;
        let result = handlers::handle_inlay_hint(self, uri, range);
        let count = result.as_ref().map(|v| v.len()).unwrap_or(0);
        tracing::debug!(uri = %uri, hints = count, "inlay_hint");
        Ok(result)
    }

    // -- Execute command --

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<serde_json::Value>> {
        tracing::info!(command = %params.command, "execute_command");

        match params.command.as_str() {
            "al.downloadSymbols" => {
                workspace::download_symbols_command(self).await;
                Ok(None)
            }
            _ => {
                tracing::warn!(command = %params.command, "Unknown command");
                Ok(None)
            }
        }
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
