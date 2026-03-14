//! Document store — open file management with rope-based text.
//!
//! Transport-agnostic: uses [`TextChange`] instead of LSP-specific types.
//! al-lsp converts `TextDocumentContentChangeEvent` → `TextChange` at the boundary.

use dashmap::DashMap;
use ropey::Rope;
use url::Url;

/// A text change to apply to an open document.
#[derive(Debug, Clone)]
pub struct TextChange {
    /// If `None`, this is a full document replacement.
    /// If `Some`, this is an incremental change within the given range.
    pub range: Option<TextRange>,
    /// The new text to insert (replaces the range, or the entire document if range is None).
    pub text: String,
}

/// A range within a document (line/character based, 0-indexed).
#[derive(Debug, Clone, Copy)]
pub struct TextRange {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// Store for open documents.
pub struct DocumentStore {
    docs: DashMap<Url, Document>,
    trees: DashMap<Url, (i32, tree_sitter::Tree)>,
}

struct Document {
    text: Rope,
    /// Cached String form — updated on every change, avoids repeated Rope::to_string().
    text_cache: String,
    version: i32,
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            docs: DashMap::new(),
            trees: DashMap::new(),
        }
    }

    pub fn open(&self, uri: Url, text: String) {
        self.docs.insert(
            uri,
            Document {
                text: Rope::from_str(&text),
                text_cache: text,
                version: 0,
            },
        );
    }

    pub fn close(&self, uri: &Url) {
        self.docs.remove(uri);
        self.trees.remove(uri);
    }

    pub fn get_text(&self, uri: &Url) -> Option<String> {
        self.docs.get(uri).map(|d| d.text_cache.clone())
    }

    pub fn get_version(&self, uri: &Url) -> Option<i32> {
        self.docs.get(uri).map(|d| d.version)
    }

    pub fn contains(&self, uri: &Url) -> bool {
        self.docs.contains_key(uri)
    }

    pub fn apply_changes(&self, uri: &Url, changes: &[TextChange]) {
        if let Some(mut doc) = self.docs.get_mut(uri) {
            for change in changes {
                if let Some(range) = change.range {
                    let start = position_to_offset(&doc.text, range.start_line, range.start_character);
                    let end = position_to_offset(&doc.text, range.end_line, range.end_character);
                    if let (Some(start), Some(end)) = (start, end) {
                        doc.text.remove(start..end);
                        doc.text.insert(start, &change.text);
                    }
                } else {
                    doc.text = Rope::from_str(&change.text);
                }
            }
            doc.text_cache = doc.text.to_string();
            doc.version += 1;
            // Invalidate cached tree since the document changed
            self.trees.remove(uri);
        }
    }

    /// Return the cached parse tree if the version matches the current document version.
    pub fn get_cached_tree(&self, uri: &Url) -> Option<tree_sitter::Tree> {
        let doc = self.docs.get(uri)?;
        let cached = self.trees.get(uri)?;
        if cached.0 == doc.version {
            Some(cached.1.clone())
        } else {
            None
        }
    }

    /// Cache a parse tree for the given document URI and version.
    pub fn cache_tree(&self, uri: &Url, version: i32, tree: tree_sitter::Tree) {
        self.trees.insert(uri.clone(), (version, tree));
    }
}

fn position_to_offset(rope: &Rope, line: u32, character: u32) -> Option<usize> {
    let line = line as usize;
    if line >= rope.len_lines() {
        return None;
    }
    let line_start = rope.line_to_char(line);
    Some(line_start + character as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_uri(name: &str) -> Url {
        Url::parse(&format!("file:///test/{}.al", name)).unwrap()
    }

    #[test]
    fn test_open_and_get() {
        let store = DocumentStore::new();
        let uri = test_uri("hello");
        store.open(uri.clone(), "hello world".to_string());
        assert_eq!(store.get_text(&uri), Some("hello world".to_string()));
        assert!(store.contains(&uri));
    }

    #[test]
    fn test_close() {
        let store = DocumentStore::new();
        let uri = test_uri("close");
        store.open(uri.clone(), "content".to_string());
        assert!(store.contains(&uri));
        store.close(&uri);
        assert!(!store.contains(&uri));
        assert_eq!(store.get_text(&uri), None);
    }

    #[test]
    fn test_full_replace() {
        let store = DocumentStore::new();
        let uri = test_uri("replace");
        store.open(uri.clone(), "old content".to_string());
        store.apply_changes(&uri, &[TextChange { range: None, text: "new content".to_string() }]);
        assert_eq!(store.get_text(&uri), Some("new content".to_string()));
    }

    #[test]
    fn test_incremental_update() {
        let store = DocumentStore::new();
        let uri = test_uri("incr");
        store.open(uri.clone(), "hello world".to_string());
        store.apply_changes(&uri, &[TextChange {
            range: Some(TextRange { start_line: 0, start_character: 6, end_line: 0, end_character: 11 }),
            text: "AL".to_string(),
        }]);
        assert_eq!(store.get_text(&uri), Some("hello AL".to_string()));
    }

    #[test]
    fn test_version_increments() {
        let store = DocumentStore::new();
        let uri = test_uri("ver");
        store.open(uri.clone(), "v0".to_string());
        assert_eq!(store.get_version(&uri), Some(0));
        store.apply_changes(&uri, &[TextChange { range: None, text: "v1".to_string() }]);
        assert_eq!(store.get_version(&uri), Some(1));
    }

    #[test]
    fn test_nonexistent_uri() {
        let store = DocumentStore::new();
        let uri = test_uri("missing");
        assert_eq!(store.get_text(&uri), None);
        assert!(!store.contains(&uri));
    }

    #[test]
    fn test_tree_cache_invalidated_on_change() {
        let store = DocumentStore::new();
        let uri = test_uri("tree");
        store.open(uri.clone(), "content".to_string());
        // Simulate caching a tree at version 0
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&al_syntax::parser::language()).unwrap();
        let tree = parser.parse("content", None).unwrap();
        store.cache_tree(&uri, 0, tree);
        assert!(store.get_cached_tree(&uri).is_some());
        // Change invalidates tree
        store.apply_changes(&uri, &[TextChange { range: None, text: "changed".to_string() }]);
        assert!(store.get_cached_tree(&uri).is_none());
    }

    #[test]
    fn test_default_impl() {
        let store = DocumentStore::default();
        assert!(!store.contains(&test_uri("any")));
    }
}
