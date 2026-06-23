use tower_lsp::lsp_types::*;

use super::AlServer;

pub(crate) async fn handle_completion(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<CompletionResponse> {
    let core_pos = position.into();
    let entries =
        crate::queries::completions::completions_full(&server.workspace, uri, core_pos).await;
    if entries.is_empty() {
        return None;
    }
    let items: Vec<CompletionItem> = entries
        .into_iter()
        .map(|e| {
            let kind = match e.kind {
                crate::queries::completions::CompletionKind::Keyword => CompletionItemKind::KEYWORD,
                crate::queries::completions::CompletionKind::Snippet => CompletionItemKind::SNIPPET,
                crate::queries::completions::CompletionKind::Field => CompletionItemKind::FIELD,
                crate::queries::completions::CompletionKind::Property => {
                    CompletionItemKind::PROPERTY
                }
                crate::queries::completions::CompletionKind::Method => CompletionItemKind::METHOD,
                crate::queries::completions::CompletionKind::Function => {
                    CompletionItemKind::FUNCTION
                }
                crate::queries::completions::CompletionKind::Variable => {
                    CompletionItemKind::VARIABLE
                }
                crate::queries::completions::CompletionKind::Class => CompletionItemKind::CLASS,
                crate::queries::completions::CompletionKind::Module => CompletionItemKind::MODULE,
                crate::queries::completions::CompletionKind::Enum => CompletionItemKind::ENUM,
                crate::queries::completions::CompletionKind::EnumMember => {
                    CompletionItemKind::ENUM_MEMBER
                }
                crate::queries::completions::CompletionKind::Value => CompletionItemKind::VALUE,
                crate::queries::completions::CompletionKind::Struct => CompletionItemKind::STRUCT,
                crate::queries::completions::CompletionKind::Reference => {
                    CompletionItemKind::REFERENCE
                }
                crate::queries::completions::CompletionKind::Text => CompletionItemKind::TEXT,
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
        })
        .collect();
    Some(CompletionResponse::Array(items))
}
