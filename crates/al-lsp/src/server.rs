//! AlServer state and LSP lifecycle.

use std::sync::Arc;

use al_discovery::AlToolchain;
use al_symbols::SymbolIndex;
use al_syntax::AlParser;
use dashmap::DashMap;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::document::DocumentStore;

/// The AL language server.
pub struct AlServer {
    pub(crate) client: Client,
    pub(crate) parser: std::sync::Mutex<AlParser>,
    pub(crate) symbols: Arc<SymbolIndex>,
    pub(crate) toolchain: Option<AlToolchain>,
    pub(crate) documents: DocumentStore,
    pub(crate) workspace_files: DashMap<Url, String>,
}

impl AlServer {
    fn new(client: Client) -> Self {
        Self {
            client,
            parser: std::sync::Mutex::new(AlParser::new()),
            symbols: Arc::new(SymbolIndex::new()),
            toolchain: None,
            documents: DocumentStore::new(),
            workspace_files: DashMap::new(),
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for AlServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions::default()),
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
                            full: Some(SemanticTokensFullOptions::Bool(true)),
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
            ..Default::default()
        })
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.documents.open(
            params.text_document.uri.clone(),
            params.text_document.text,
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        self.documents
            .apply_changes(&params.text_document.uri, &params.content_changes);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.close(&params.text_document.uri);
    }
}

/// Run the LSP server on stdin/stdout.
pub async fn run_lsp() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(AlServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
