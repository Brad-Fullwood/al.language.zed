//! Parse tree caching — avoids redundant re-parses for the same document version.

use std::sync::Arc;

use crate::syntax::AlParser;
use url::Url;

use crate::documents::DocumentStore;

/// Get the text and parse tree for a document, using the cache when possible.
///
/// If the document's version matches a previously cached tree, returns the cached
/// tree without re-parsing. Otherwise parses the document and caches the result.
///
/// Returns `Arc<String>` to avoid deep-copying the document on every LSP request.
pub fn get_or_parse(
    documents: &DocumentStore,
    uri: &Url,
) -> Option<(Arc<String>, tree_sitter::Tree)> {
    let Some(text) = documents.get_text_arc(uri) else {
        // Downgrade to debug — this fires on every keystroke against a
        // closed/virtual document and is not actionable for the user.
        tracing::debug!(uri = %uri, "get_or_parse: document not in store (not opened?)");
        return None;
    };
    let version = documents.get_version(uri).unwrap_or(0);

    // Check cache first
    if let Some(cached) = documents.get_cached_tree(uri) {
        tracing::trace!(uri = %uri, version, "get_or_parse: cache hit");
        return Some((text, cached));
    }

    // Parse and cache
    tracing::debug!(uri = %uri, version, len = text.len(), "get_or_parse: parsing");
    let tree = AlParser::parse_quick(&text).tree;
    documents.cache_tree(uri, version, tree.clone());
    Some((text, tree))
}

/// Count all nodes in a parse tree (for diagnostic/debug purposes).
pub fn count_nodes(tree: &tree_sitter::Tree) -> usize {
    let mut count = 0;
    let mut cursor = tree.walk();
    loop {
        count += 1;
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return count;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_uri(name: &str) -> Url {
        Url::parse(&format!("file:///test/{}.al", name)).unwrap()
    }

    #[test]
    fn test_get_or_parse_returns_none_for_missing() {
        let store = DocumentStore::new();
        let uri = test_uri("missing");
        assert!(get_or_parse(&store, &uri).is_none());
    }

    #[test]
    fn test_get_or_parse_parses_and_caches() {
        let store = DocumentStore::new();
        let uri = test_uri("cached");
        store.open(uri.clone(), "codeunit 50100 Test { }".to_string());

        // First call: should parse
        let result = get_or_parse(&store, &uri);
        assert!(result.is_some());
        let (text, _tree) = result.unwrap();
        assert_eq!(&*text, "codeunit 50100 Test { }");

        // Second call: should use cache (same version)
        let result2 = get_or_parse(&store, &uri);
        assert!(result2.is_some());
    }

    #[test]
    fn test_cache_invalidated_after_change() {
        let store = DocumentStore::new();
        let uri = test_uri("change");
        store.open(uri.clone(), "codeunit 50100 A { }".to_string());

        // Parse and cache
        let _ = get_or_parse(&store, &uri);
        assert!(store.get_cached_tree(&uri).is_some());

        // Change document
        store.apply_changes(
            &uri,
            &[crate::documents::TextChange {
                range: None,
                text: "codeunit 50100 B { }".to_string(),
            }],
        );

        // Cache should be invalidated
        assert!(store.get_cached_tree(&uri).is_none());

        // Re-parse
        let result = get_or_parse(&store, &uri);
        assert!(result.is_some());
        assert_eq!(&*result.unwrap().0, "codeunit 50100 B { }");
    }
}
