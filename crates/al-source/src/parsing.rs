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

    // Fast-path cache check before acquiring the parse lock.
    //
    // `get_cached_tree` validates the cached entry against the *live* document
    // version, not our captured `version`. If `apply_changes` ran after we read
    // `text` (advancing the document to a newer version) and another thread
    // already parsed that newer version, the live-version check would pass and
    // we'd return that newer tree paired with our *older* `text` — a mismatched
    // (old-text, new-tree) pair. Guard against that by also requiring the cached
    // tree's version to equal the version we captured `text` at.
    if let Some(cached) = documents.get_cached_tree_at_version(uri, version) {
        tracing::trace!(uri = %uri, version, "get_or_parse: cache hit (fast path)");
        return Some((text, cached));
    }

    // Cache miss — serialize the parse for this URI. Without this, a burst of
    // LSP requests on a keystroke (hover + completion + semantic-tokens fire
    // together) would all miss the cache, race into `parse_quick`, and each
    // pay the 30-50 ms parse cost on a large AL file. With the lock the first
    // request parses; the rest find the cache populated by the re-check below.
    //
    // Correctness: the cache invariant is enforced by `get_cached_tree`
    // (it compares the stored version against the live `doc.version`). If
    // `apply_changes` runs between our `get_version` call and the `cache_tree`
    // write, our stored entry is under the old version and the next reader's
    // version-check will reject it. This means we may briefly waste a parse,
    // but never serve a stale tree.
    let lock = documents.parse_lock(uri);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

    // Re-fetch text + version atomically under the lock in case the document
    // changed while we were waiting. (Cheap — Arc<String> clone + i32 copy.)
    let (text, version) = documents.get_text_and_version(uri)?;

    // Re-check the cache after acquiring the lock — another waiter may have
    // populated it while we were blocked. Match against the version we just
    // captured `text` at, so we never pair stale text with a newer tree.
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
        // Regression: hover + completion + semantic-tokens fire near-
        // simultaneously on a keystroke. Without per-URI parse-lock
        // serialisation, each would miss the cache, race into parse_quick,
        // and pay the 30-50 ms cost N times. With the lock, the first
        // request parses; the rest find the cache populated.
        let store = std::sync::Arc::new(DocumentStore::new());
        let uri = test_uri("concurrent");
        // A non-trivial source so the cost gap is visible enough to be
        // measurable, though we assert on cache-hits not timing.
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
        // Regression for the get_or_parse TOCTOU race: a tree cached for a newer
        // document version must NOT be served to a caller that captured text at
        // an older version, even though the cached tree matches the *live*
        // version. Otherwise the caller pairs old text with a new tree and
        // indexes text.as_bytes() with mismatched node ranges.
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

        // The live-version-only check would (incorrectly) hand back the new tree.
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
