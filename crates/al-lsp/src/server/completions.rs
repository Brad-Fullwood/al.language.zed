use tower_lsp::lsp_types::*;

use super::AlServer;

pub(crate) async fn handle_completion(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<CompletionResponse> {
    let core_pos = position.into();
    let entries =
        al_analysis::queries::completions::completions_full(&server.workspace, uri, core_pos).await;
    if entries.is_empty() {
        return None;
    }
    let items: Vec<CompletionItem> = entries
        .into_iter()
        .map(|e| {
            let kind = match e.kind {
                al_analysis::queries::completions::CompletionKind::Keyword => {
                    CompletionItemKind::KEYWORD
                }
                al_analysis::queries::completions::CompletionKind::Snippet => {
                    CompletionItemKind::SNIPPET
                }
                al_analysis::queries::completions::CompletionKind::Field => {
                    CompletionItemKind::FIELD
                }
                al_analysis::queries::completions::CompletionKind::Property => {
                    CompletionItemKind::PROPERTY
                }
                al_analysis::queries::completions::CompletionKind::Method => {
                    CompletionItemKind::METHOD
                }
                al_analysis::queries::completions::CompletionKind::Function => {
                    CompletionItemKind::FUNCTION
                }
                al_analysis::queries::completions::CompletionKind::Variable => {
                    CompletionItemKind::VARIABLE
                }
                al_analysis::queries::completions::CompletionKind::Class => {
                    CompletionItemKind::CLASS
                }
                al_analysis::queries::completions::CompletionKind::Module => {
                    CompletionItemKind::MODULE
                }
                al_analysis::queries::completions::CompletionKind::Enum => CompletionItemKind::ENUM,
                al_analysis::queries::completions::CompletionKind::EnumMember => {
                    CompletionItemKind::ENUM_MEMBER
                }
                al_analysis::queries::completions::CompletionKind::Value => {
                    CompletionItemKind::VALUE
                }
                al_analysis::queries::completions::CompletionKind::Struct => {
                    CompletionItemKind::STRUCT
                }
                al_analysis::queries::completions::CompletionKind::Reference => {
                    CompletionItemKind::REFERENCE
                }
                al_analysis::queries::completions::CompletionKind::Text => CompletionItemKind::TEXT,
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
