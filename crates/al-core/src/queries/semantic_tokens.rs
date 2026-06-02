//! Semantic tokens query.

use url::Url;

use crate::workspace::Workspace;

/// A semantic token (delta-encoded position + type + modifiers).
///
/// Mirrors `crate::syntax::SemanticToken` field-for-field but adds `serde::Serialize`
/// for the daemon JSON-RPC path. `crate::syntax::SemanticToken` intentionally avoids
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

impl From<crate::syntax::SemanticToken> for SemanticToken {
    fn from(t: crate::syntax::SemanticToken) -> Self {
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
    let tokens: Vec<SemanticToken> = crate::syntax::extract_semantic_tokens(&tree, &text)
        .into_iter()
        .map(SemanticToken::from)
        .collect();
    tracing::debug!(count = tokens.len(), "semantic tokens emitted");
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use url::Url;

    const SAMPLE_AL: &str = r#"codeunit 50100 "My CU"
{
    procedure DoWork()
    begin
        Message('Hello');
    end;
}"#;

    // --- positive / happy path ---

    #[test]
    fn semantic_tokens_full_returns_tokens_for_open_document() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/tokens.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let tokens = semantic_tokens_full(&ws, &uri);
        assert!(
            !tokens.is_empty(),
            "A non-empty AL document must yield at least one semantic token"
        );
    }

    #[test]
    fn semantic_tokens_full_first_token_is_absolute_position() {
        // The LSP delta-encoding contract: the very first token's delta is
        // measured from origin (0,0), so its delta_line/delta_start are the
        // token's absolute line/column. The wrapper must preserve this.
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/first.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let tokens = semantic_tokens_full(&ws, &uri);
        let first = tokens.first().expect("expected at least one token");
        // First meaningful token in the sample is the `codeunit` keyword on
        // line 0, column 0.
        assert_eq!(first.delta_line, 0, "first token starts on line 0");
        assert_eq!(first.delta_start, 0, "first token starts at column 0");
        assert!(first.length > 0, "a token must have non-zero length");
    }

    #[test]
    fn semantic_tokens_full_preserves_underlying_fields() {
        // The wrapper must map each syntax token field-for-field with no
        // reordering or mutation. Compare against the syntax layer directly.
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/preserve.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let (text, tree) =
            crate::parsing::get_or_parse(&ws.documents, &uri).expect("document should parse");
        let syntax_tokens = crate::syntax::extract_semantic_tokens(&tree, &text);

        let tokens = semantic_tokens_full(&ws, &uri);
        assert_eq!(
            tokens.len(),
            syntax_tokens.len(),
            "wrapper must not add or drop tokens"
        );
        for (got, expected) in tokens.iter().zip(syntax_tokens.iter()) {
            assert_eq!(got.delta_line, expected.delta_line);
            assert_eq!(got.delta_start, expected.delta_start);
            assert_eq!(got.length, expected.length);
            assert_eq!(got.token_type, expected.token_type);
            assert_eq!(got.token_modifiers, expected.token_modifiers);
        }
    }

    // --- negative / edge paths ---

    #[test]
    fn semantic_tokens_full_unknown_uri_returns_empty() {
        // Document was never opened -> get_or_parse yields None -> empty vec.
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent/missing.al").expect("test");
        let tokens = semantic_tokens_full(&ws, &uri);
        assert!(
            tokens.is_empty(),
            "Unknown URI must produce an empty token list, not panic"
        );
    }

    #[test]
    fn semantic_tokens_full_empty_document_returns_empty() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/empty.al").expect("test");
        ws.documents.open(uri.clone(), String::new());
        let tokens = semantic_tokens_full(&ws, &uri);
        assert!(
            tokens.is_empty(),
            "An empty document has no tokens to highlight"
        );
    }

    // --- conversion boundary type ---

    #[test]
    fn semantic_token_from_maps_all_fields() {
        let src = crate::syntax::SemanticToken {
            delta_line: 3,
            delta_start: 7,
            length: 11,
            token_type: 2,
            token_modifiers: 5,
        };
        let got = SemanticToken::from(src);
        assert_eq!(got.delta_line, 3);
        assert_eq!(got.delta_start, 7);
        assert_eq!(got.length, 11);
        assert_eq!(got.token_type, 2);
        assert_eq!(got.token_modifiers, 5);
    }

    #[test]
    fn semantic_token_serializes_with_lsp_field_names() {
        // The daemon JSON-RPC path depends on these exact camelCase keys.
        let tok = SemanticToken {
            delta_line: 1,
            delta_start: 2,
            length: 3,
            token_type: 4,
            token_modifiers: 0,
        };
        let json = serde_json::to_string(&tok).expect("serialize");
        assert!(json.contains("\"deltaLine\":1"), "got: {json}");
        assert!(json.contains("\"deltaStart\":2"), "got: {json}");
        assert!(json.contains("\"length\":3"), "got: {json}");
        assert!(json.contains("\"tokenType\":4"), "got: {json}");
        assert!(json.contains("\"tokenModifiers\":0"), "got: {json}");
    }
}
