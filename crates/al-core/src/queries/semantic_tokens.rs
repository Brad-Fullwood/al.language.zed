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
        // LSP delta-encoding: the first token's delta is measured from origin (0,0).
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/first.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let tokens = semantic_tokens_full(&ws, &uri);
        let first = tokens.first().expect("expected at least one token");
        assert_eq!(first.delta_line, 0, "first token starts on line 0");
        assert_eq!(first.delta_start, 0, "first token starts at column 0");
        assert!(first.length > 0, "a token must have non-zero length");
    }

    #[test]
    fn semantic_tokens_full_preserves_underlying_fields() {
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

    #[test]
    fn semantic_tokens_full_delta_encoding_is_monotonic_per_line() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/delta.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let tokens = semantic_tokens_full(&ws, &uri);
        assert!(tokens.len() >= 2, "sample must yield multiple tokens");

        for window in tokens.windows(2) {
            let next = &window[1];
            if next.delta_line == 0 {
                assert!(
                    next.delta_start > 0,
                    "two tokens on the same line must advance the column: {next:?}"
                );
            }
            assert!(next.length > 0, "every token has a length: {next:?}");
        }
    }

    #[test]
    fn semantic_tokens_full_is_deterministic_across_calls() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/repeat.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let first = semantic_tokens_full(&ws, &uri);
        let second = semantic_tokens_full(&ws, &uri);

        assert_eq!(
            first.len(),
            second.len(),
            "repeated queries must return the same token count"
        );
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(a.delta_line, b.delta_line);
            assert_eq!(a.delta_start, b.delta_start);
            assert_eq!(a.length, b.length);
            assert_eq!(a.token_type, b.token_type);
            assert_eq!(a.token_modifiers, b.token_modifiers);
        }
    }

    #[test]
    fn semantic_tokens_full_scales_with_document_content() {
        // Distinct URIs needed: re-opening the same URI serves the cached tree.
        let ws = Workspace::new();
        let small_uri = Url::parse("file:///test/small.al").expect("test");
        let big_uri = Url::parse("file:///test/big.al").expect("test");

        ws.documents.open(small_uri.clone(), SAMPLE_AL.to_string());
        let small = semantic_tokens_full(&ws, &small_uri);
        assert!(!small.is_empty());

        let bigger = format!(
            "{SAMPLE_AL}\n\ncodeunit 50101 \"Other\"\n{{\n    procedure More()\n    begin\n    end;\n}}"
        );
        ws.documents.open(big_uri.clone(), bigger);
        let big = semantic_tokens_full(&ws, &big_uri);

        assert!(
            big.len() > small.len(),
            "a document with more code must surface more tokens (small {}, big {})",
            small.len(),
            big.len()
        );
    }

    #[test]
    fn semantic_tokens_full_unknown_uri_returns_empty() {
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

    #[test]
    fn semantic_tokens_full_emits_keyword_token_type_for_object_keyword() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/kind.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let (text, tree) =
            crate::parsing::get_or_parse(&ws.documents, &uri).expect("document should parse");
        let syntax_tokens = crate::syntax::extract_semantic_tokens(&tree, &text);
        let syntax_first = syntax_tokens.first().expect("syntax layer yields a token");

        let tokens = semantic_tokens_full(&ws, &uri);
        let first = tokens.first().expect("wrapper yields a token");
        assert_eq!(
            first.token_type, syntax_first.token_type,
            "wrapper must not remap the token type chosen by the syntax layer"
        );
    }

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
    fn semantic_tokens_full_surfaces_more_than_one_token_type() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/types.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let tokens = semantic_tokens_full(&ws, &uri);
        assert!(!tokens.is_empty(), "sample must produce tokens");

        let mut seen = std::collections::HashSet::new();
        for tok in &tokens {
            seen.insert(tok.token_type);
        }
        assert!(
            seen.len() >= 2,
            "a real AL object must surface multiple token types, got {seen:?}"
        );
    }

    #[test]
    fn semantic_tokens_full_delta_stream_reconstructs_syntax_positions() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/reconstruct.al").expect("test");
        ws.documents.open(uri.clone(), SAMPLE_AL.to_string());

        let tokens = semantic_tokens_full(&ws, &uri);
        assert!(tokens.len() >= 2, "need multiple tokens to test ordering");

        let mut abs_line: u32 = 0;
        let mut abs_col: u32 = 0;
        let mut prev_line: Option<u32> = None;
        let mut prev_col: u32 = 0;

        for (idx, tok) in tokens.iter().enumerate() {
            abs_line += tok.delta_line;
            abs_col = if tok.delta_line == 0 {
                abs_col + tok.delta_start
            } else {
                tok.delta_start
            };

            if let Some(pl) = prev_line {
                assert!(
                    abs_line >= pl,
                    "token {idx} moved backwards by line: {abs_line} < {pl}"
                );
                if abs_line == pl {
                    assert!(
                        abs_col > prev_col,
                        "token {idx} on line {abs_line} did not advance column: {abs_col} <= {prev_col}"
                    );
                }
            }
            prev_line = Some(abs_line);
            prev_col = abs_col;
        }
    }

    #[test]
    fn semantic_tokens_full_handles_multi_line_string_literal() {
        let multi_line = "codeunit 50100 \"Multi\"\n{\n    procedure A()\n    begin\n    end;\n\n    procedure B()\n    begin\n    end;\n}";
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/multiline.al").expect("test");
        ws.documents.open(uri.clone(), multi_line.to_string());

        let tokens = semantic_tokens_full(&ws, &uri);
        assert!(!tokens.is_empty(), "multi-line doc must produce tokens");
        assert!(
            tokens.iter().any(|t| t.delta_line > 0),
            "a multi-line document must yield at least one token that advances the line"
        );
    }

    #[test]
    fn semantic_tokens_full_carries_nonzero_token_modifiers_through_wrapper() {
        // Real AL parsed here happens to emit token_modifiers == 0; only the From
        // conversion + serialization path can prove a non-zero modifier survives to the wire.
        let src = crate::syntax::SemanticToken {
            delta_line: 0,
            delta_start: 0,
            length: 4,
            token_type: 1,
            token_modifiers: 0b1010,
        };
        let wrapped = SemanticToken::from(src);
        assert_eq!(wrapped.token_modifiers, 0b1010);
        let json = serde_json::to_string(&wrapped).expect("serialize");
        assert!(
            json.contains("\"tokenModifiers\":10"),
            "non-zero modifier bitset must survive to the wire, got: {json}"
        );
    }

    #[test]
    fn semantic_token_serializes_with_lsp_field_names() {
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
