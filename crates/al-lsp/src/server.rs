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
}

impl AlServer {
    fn new(client: Client) -> Self {
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
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for AlServer {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Kick off workspace initialization in background
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

        // Spawn workspace init (non-blocking)
        let client = self.client.clone();
        // We can't move `self` into a task, so we do init in `initialized` instead.
        // Store the root URI for later use.
        let _ = (root_uri, client);

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ".".to_string(),
                        ":".to_string(),
                    ]),
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

        // Perform workspace initialization
        // We need to discover the root from the initialize params.
        // Since we can't easily pass data between initialize and initialized,
        // use the current directory as fallback.
        workspace::initialize_workspace(self, None).await;
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

        self.documents.open(uri.clone(), params.text_document.text);

        // Update workspace files map and object name index
        if let Ok(path) = uri.to_file_path() {
            self.workspace_files.insert(path.clone(), text.clone());
            // Update workspace object name index
            let mut parser = self.parser.lock().unwrap();
            let result = parser.parse(&text);
            if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &text) {
                self.workspace_objects.insert(obj_info.name.to_lowercase(), path);
            }
        }

        // Publish diagnostics
        diagnostics::publish_diagnostics(self, &uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();

        self.documents
            .apply_changes(&uri, &params.content_changes);

        // Get updated text for diagnostics
        if let Some(text) = self.documents.get_text(&uri) {
            // Update workspace files map and object name index
            if let Ok(path) = uri.to_file_path() {
                self.workspace_files.insert(path.clone(), text.clone());
                // Update workspace object name index
                let mut parser = self.parser.lock().unwrap();
                let result = parser.parse(&text);
                if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &text) {
                    self.workspace_objects.insert(obj_info.name.to_lowercase(), path);
                }
            }

            // Re-publish diagnostics
            diagnostics::publish_diagnostics(self, &uri, &text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.close(&uri);

        // Remove from workspace_files to free memory (will be re-read if needed)
        if let Ok(path) = uri.to_file_path() {
            self.workspace_files.remove(&path);
            // Remove from workspace_objects index (retain entries that don't point to this path)
            self.workspace_objects.retain(|_, v| *v != path);
        }

        // Clear diagnostics for the closed file
        self.client
            .publish_diagnostics(uri, vec![], None)
            .await;
    }

    // -- Hover --

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(hover::handle_hover(self, uri, position))
    }

    // -- Completion --

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        Ok(completions::handle_completion(self, uri, position))
    }

    // -- Go-to-definition --

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(definition::handle_definition(self, uri, position))
    }

    // -- References --

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        Ok(definition::handle_references(
            self,
            uri,
            position,
            include_declaration,
        ))
    }

    // -- Document symbols --

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        Ok(handlers::handle_document_symbol(self, uri))
    }

    // -- Formatting --

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        Ok(formatting::handle_formatting(self, uri, &params.options))
    }

    // -- Folding ranges --

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        Ok(handlers::handle_folding_range(self, uri))
    }

    // -- Semantic tokens --

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        Ok(handlers::handle_semantic_tokens(self, uri))
    }

    // -- Signature help --

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(handlers::handle_signature_help(self, uri, position))
    }

    // -- Code actions --

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let range = params.range;
        let diagnostics = &params.context.diagnostics;
        Ok(handlers::handle_code_action(self, uri, range, diagnostics))
    }

    // -- Rename --

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;
        Ok(definition::handle_rename(self, uri, position, new_name))
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
        Ok(workspace::handle_workspace_symbol(self, &params.query))
    }

    // -- Inlay hints --

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let range = params.range;
        Ok(handlers::handle_inlay_hint(self, uri, range))
    }
}

/// Run the LSP server on stdin/stdout.
pub async fn run_lsp() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(AlServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
