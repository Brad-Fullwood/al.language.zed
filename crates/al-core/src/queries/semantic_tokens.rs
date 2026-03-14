//! Semantic tokens query.

use url::Url;

use crate::workspace::Workspace;

/// A semantic token (delta-encoded position + type + modifiers).
#[derive(Debug, Clone)]
pub struct SemanticToken {
    pub delta_line: u32,
    pub delta_start: u32,
    pub length: u32,
    pub token_type: u32,
    pub token_modifiers: u32,
}

/// Get semantic tokens for an entire document.
pub fn semantic_tokens_full(workspace: &Workspace, uri: &Url) -> Vec<SemanticToken> {
    let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        return Vec::new();
    };
    let tokens = al_syntax::extract_semantic_tokens(&tree, &text);
    tokens.iter().map(|t| SemanticToken {
        delta_line: t.delta_line,
        delta_start: t.delta_start,
        length: t.length,
        token_type: t.token_type,
        token_modifiers: t.token_modifiers,
    }).collect()
}
