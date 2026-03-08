//! Parse tree caching — avoids redundant re-parses for the same document version.

use tower_lsp::lsp_types::Url;

use crate::server::AlServer;

/// Get the text and parse tree for a document, using the cache when possible.
///
/// If the document's version matches a previously cached tree, returns the cached
/// tree without re-parsing. Otherwise parses the document and caches the result.
pub(crate) fn get_or_parse(server: &AlServer, uri: &Url) -> Option<(String, tree_sitter::Tree)> {
    let text = server.documents.get_text(uri)?;
    let version = server.documents.get_version(uri).unwrap_or(0);

    // Check cache first
    if let Some(cached) = server.documents.get_cached_tree(uri) {
        return Some((text, cached));
    }

    // Parse and cache
    let tree = {
        let mut parser = server.parser.lock().unwrap();
        parser.parse(&text).tree
    };
    server.documents.cache_tree(uri, version, tree.clone());
    Some((text, tree))
}
