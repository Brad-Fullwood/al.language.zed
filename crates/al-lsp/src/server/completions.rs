use tower_lsp::lsp_types::*;

use super::AlServer;

/// Whether an insert text uses LSP snippet syntax (`$0`, `$1`, `${1:name}`).
///
/// Only snippet-kind items are treated as snippets: a plain identifier that
/// happens to contain a `$` must still be inserted verbatim.
fn contains_snippet_placeholder(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'$'
            && bytes
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_digit() || *next == b'{')
    })
}

pub(crate) async fn handle_completion(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Result<Option<CompletionResponse>, String> {
    let core_pos = position.into();
    let entries =
        al_analysis::queries::completions::completions_full(&server.workspace, uri, core_pos)
            .await?;
    if entries.is_empty() {
        return Ok(None);
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
            // Snippet-kind items (and any insert text carrying `$1`/`${…}`
            // placeholders) must declare the snippet format, or the client
            // inserts the placeholder syntax literally.
            let insert_text_format = e
                .insert_text
                .as_deref()
                .filter(|text| {
                    kind == CompletionItemKind::SNIPPET && contains_snippet_placeholder(text)
                })
                .map(|_| InsertTextFormat::SNIPPET);
            CompletionItem {
                label: e.label,
                kind: Some(kind),
                detail: e.detail,
                documentation,
                insert_text: e.insert_text,
                insert_text_format,
                sort_text: e.sort_text,
                ..Default::default()
            }
        })
        .collect();
    Ok(Some(CompletionResponse::Array(items)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_placeholders_are_detected() {
        assert!(contains_snippet_placeholder("begin\n\t$0\nend"));
        assert!(contains_snippet_placeholder("Message(${1:text})"));
        assert!(contains_snippet_placeholder("$1"));
    }

    #[test]
    fn plain_text_is_not_a_snippet() {
        assert!(!contains_snippet_placeholder("MyProcedure()"));
        assert!(!contains_snippet_placeholder("cost$"));
        assert!(!contains_snippet_placeholder("a $ b"));
    }
}
