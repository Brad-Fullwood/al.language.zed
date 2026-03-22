//! Completion handler — thin wrapper over al-core::queries::completions.

use tower_lsp::lsp_types::*;

use crate::server::AlServer;

/// Handle textDocument/completion.
pub(crate) async fn handle_completion(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<CompletionResponse> {
    let core_pos = al_core::queries::Position { line: position.line, character: position.character };
    let entries = al_core::queries::completions::completions_full(&server.workspace, uri, core_pos).await;
    if entries.is_empty() {
        return None;
    }
    let items: Vec<CompletionItem> = entries.into_iter().map(|e| {
        let kind = match e.kind {
            al_core::queries::completions::CompletionKind::Keyword => CompletionItemKind::KEYWORD,
            al_core::queries::completions::CompletionKind::Snippet => CompletionItemKind::SNIPPET,
            al_core::queries::completions::CompletionKind::Field => CompletionItemKind::FIELD,
            al_core::queries::completions::CompletionKind::Property => CompletionItemKind::PROPERTY,
            al_core::queries::completions::CompletionKind::Method => CompletionItemKind::METHOD,
            al_core::queries::completions::CompletionKind::Function => CompletionItemKind::FUNCTION,
            al_core::queries::completions::CompletionKind::Variable => CompletionItemKind::VARIABLE,
            al_core::queries::completions::CompletionKind::Class => CompletionItemKind::CLASS,
            al_core::queries::completions::CompletionKind::Module => CompletionItemKind::MODULE,
            al_core::queries::completions::CompletionKind::Enum => CompletionItemKind::ENUM,
            al_core::queries::completions::CompletionKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
            al_core::queries::completions::CompletionKind::Value => CompletionItemKind::VALUE,
            al_core::queries::completions::CompletionKind::Struct => CompletionItemKind::STRUCT,
            al_core::queries::completions::CompletionKind::Reference => CompletionItemKind::REFERENCE,
            al_core::queries::completions::CompletionKind::Text => CompletionItemKind::TEXT,
        };
        let documentation = e.documentation.map(Documentation::String);
        CompletionItem {
            label: e.label,
            kind: Some(kind),
            detail: e.detail,
            documentation,
            insert_text: e.insert_text,
            sort_text: e.sort_text,
            ..Default::default()
        }
    }).collect();
    Some(CompletionResponse::Array(items))
}
