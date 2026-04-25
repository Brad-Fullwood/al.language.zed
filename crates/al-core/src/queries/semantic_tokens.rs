//! Semantic tokens query.

use url::Url;

use crate::workspace::Workspace;

/// A semantic token (delta-encoded position + type + modifiers).
///
/// Mirrors `al_syntax::SemanticToken` field-for-field but adds `serde::Serialize`
/// for the daemon JSON-RPC path. `al_syntax::SemanticToken` intentionally avoids
/// a serde dependency, so this thin wrapper is the serialisable boundary type.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SemanticToken {
    #[serde(rename = "deltaLine")]
    pub delta_line: u32,
    #[serde(rename = "deltaStart")]
    pub delta_start: u32,
    pub length: u32,
    #[serde(rename = "tokenType")]
    pub token_type: u32,
    #[serde(rename = "tokenModifiers")]
    pub token_modifiers: u32,
}

impl From<al_syntax::SemanticToken> for SemanticToken {
    fn from(t: al_syntax::SemanticToken) -> Self {
        Self {
            delta_line: t.delta_line,
            delta_start: t.delta_start,
            length: t.length,
            token_type: t.token_type,
            token_modifiers: t.token_modifiers,
        }
    }
}

/// Get semantic tokens for an entire document.
#[must_use]
pub fn semantic_tokens_full(workspace: &Workspace, uri: &Url) -> Vec<SemanticToken> {
    let _span = tracing::debug_span!("semantic_tokens_full", uri = %uri).entered();
    let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        tracing::debug!("document not parsed; returning empty token list");
        return Vec::new();
    };
    let tokens: Vec<SemanticToken> = al_syntax::extract_semantic_tokens(&tree, &text)
        .into_iter()
        .map(SemanticToken::from)
        .collect();
    tracing::debug!(count = tokens.len(), "semantic tokens emitted");
    tokens
}
