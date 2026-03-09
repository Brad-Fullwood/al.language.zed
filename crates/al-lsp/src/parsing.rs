//! Parse tree caching — avoids redundant re-parses for the same document version.

use tower_lsp::lsp_types::Url;

use crate::server::AlServer;

/// Get the text and parse tree for a document, using the cache when possible.
///
/// If the document's version matches a previously cached tree, returns the cached
/// tree without re-parsing. Otherwise parses the document and caches the result.
pub(crate) fn get_or_parse(server: &AlServer, uri: &Url) -> Option<(String, tree_sitter::Tree)> {
    let text = server.documents.get_text(uri);
    if text.is_none() {
        tracing::warn!(uri = %uri, "get_or_parse: document not in store (not opened?)");
        return None;
    }
    let text = text.unwrap();
    let version = server.documents.get_version(uri).unwrap_or(0);

    // Check cache first
    if let Some(cached) = server.documents.get_cached_tree(uri) {
        tracing::trace!(uri = %uri, version, "get_or_parse: cache hit");
        return Some((text, cached));
    }

    // Parse and cache
    tracing::debug!(uri = %uri, version, len = text.len(), "get_or_parse: parsing");
    let tree = {
        let mut parser = server.parser.lock().unwrap();
        parser.parse(&text).tree
    };
    server.documents.cache_tree(uri, version, tree.clone());
    Some((text, tree))
}
