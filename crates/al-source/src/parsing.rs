//! Parse tree caching — avoids redundant re-parses for the same document version.

use std::sync::Arc;

use al_syntax::AlParser;
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
    // Capture text and version under a single read lock so the pair cannot be
    // skewed by a concurrent `apply_changes` (see `get_text_and_version`).
    let Some((text, version)) = documents.get_text_and_version(uri) else {
        // Downgrade to debug — this fires on every keystroke against a
        // closed/virtual document and is not actionable for the user.
        tracing::debug!(uri = %uri, "get_or_parse: document not in store (not opened?)");
        return None;
    };

    // Match the captured version, not only the live document version, so a
    // concurrent edit cannot pair older text with a newer tree.
    if let Some(cached) = documents.get_cached_tree_at_version(uri, version) {
        tracing::trace!(uri = %uri, version, "get_or_parse: cache hit (fast path)");
        return Some((text, cached));
    }

    // Serialize cache misses per URI so concurrent LSP requests do not all
    // parse the same document version.
    let lock = documents.parse_lock(uri);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

    // The document may have changed while this request waited for the lock.
    let (text, version) = documents.get_text_and_version(uri)?;

    // Another waiter may have populated the cache while this request blocked.
    if let Some(cached) = documents.get_cached_tree_at_version(uri, version) {
        tracing::trace!(uri = %uri, version, "get_or_parse: cache hit (post-lock)");
        return Some((text, cached));
    }

    tracing::debug!(uri = %uri, version, len = text.len(), "get_or_parse: parsing");
    let tree = AlParser::parse_quick(&text).tree;
    documents.cache_tree(uri, version, tree.clone());
    Some((text, tree))
}

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

        let result = get_or_parse(&store, &uri);
        assert!(result.is_some());
        let (text, _tree) = result.unwrap();
        assert_eq!(&*text, "codeunit 50100 Test { }");

        let result2 = get_or_parse(&store, &uri);
        assert!(result2.is_some());
    }

    #[test]
    fn test_concurrent_get_or_parse_does_not_double_parse() {
        let store = std::sync::Arc::new(DocumentStore::new());
        let uri = test_uri("concurrent");
        let src = "codeunit 50100 Concurrent { procedure Foo() begin end; }\n".repeat(50);
        store.open(uri.clone(), src);

        let threads: Vec<_> = (0..16)
            .map(|_| {
                let store = store.clone();
                let uri = uri.clone();
                std::thread::spawn(move || {
                    assert!(get_or_parse(&store, &uri).is_some());
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        assert!(store.get_cached_tree(&uri).is_some());
    }

    #[test]
    fn test_cache_at_version_rejects_version_skew() {
        let store = DocumentStore::new();
        let uri = test_uri("skew");
        store.open(uri.clone(), "codeunit 50100 A { }".to_string());

        let (_old_text, captured_version) = store.get_text_and_version(&uri).unwrap();
        assert_eq!(captured_version, 0);

        store.apply_changes(
            &uri,
            &[crate::documents::TextChange {
                range: None,
                text: "codeunit 50100 B { } // longer".to_string(),
            }],
        );
        let new_version = store.get_version(&uri).unwrap();
        assert_eq!(new_version, 1);
        let new_tree = AlParser::parse_quick("codeunit 50100 B { } // longer").tree;
        store.cache_tree(&uri, new_version, new_tree);

        assert!(store.get_cached_tree(&uri).is_some());

        assert!(
            store
                .get_cached_tree_at_version(&uri, captured_version)
                .is_none(),
            "must not serve a newer-version tree to a caller holding older text"
        );

        assert!(store
            .get_cached_tree_at_version(&uri, new_version)
            .is_some());
    }

    #[test]
    fn test_cache_invalidated_after_change() {
        let store = DocumentStore::new();
        let uri = test_uri("change");
        store.open(uri.clone(), "codeunit 50100 A { }".to_string());

        let _ = get_or_parse(&store, &uri);
        assert!(store.get_cached_tree(&uri).is_some());

        store.apply_changes(
            &uri,
            &[crate::documents::TextChange {
                range: None,
                text: "codeunit 50100 B { }".to_string(),
            }],
        );

        assert!(store.get_cached_tree(&uri).is_none());

        let result = get_or_parse(&store, &uri);
        assert!(result.is_some());
        assert_eq!(&*result.unwrap().0, "codeunit 50100 B { }");
    }
}
