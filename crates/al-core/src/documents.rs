//! Document store -- open file management with rope-based text.
//!
//! Transport-agnostic: uses [`TextChange`] instead of LSP-specific types.
//! al-lsp converts `TextDocumentContentChangeEvent` -> `TextChange` at the boundary.

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
    /// Per-URI parse-coordination locks. Acquired by `parse_lock` so that
    /// concurrent `get_or_parse` calls for the same URI serialize on the
    /// expensive `AlParser::parse_quick` step instead of all racing into it.
    /// Distinct URIs still parse in parallel. Entries are evicted in `close()`
    /// alongside `docs`/`trees`, so a long-running daemon that opens and closes
    /// many distinct files over its lifetime does not accumulate stale locks.
    parse_locks: DashMap<Url, std::sync::Arc<std::sync::Mutex<()>>>,
    /// Optional per-document byte cap (F-OPEN-042). `0` means "no cap" (the
    /// default, preserving prior unbounded behaviour). When non-zero, `open`
    /// and full-document replacements refuse content larger than this many
    /// bytes so a single oversized file (e.g. a multi-gigabyte blob opened by
    /// accident) cannot consume memory unbounded in the rope/parse-tree store.
    /// Atomic so the daemon can apply a config update without locking the
    /// whole store.
    max_doc_bytes: std::sync::atomic::AtomicUsize,
}

struct Document {
    text: Rope,
    /// Cached Arc<String> -- updated on every change, avoids repeated Rope::to_string().
    /// Arc allows get_text_arc() to return a cheap pointer copy instead of a string clone.
    text_cache: std::sync::Arc<String>,
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
            parse_locks: DashMap::new(),
            max_doc_bytes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Set the per-document byte cap (F-OPEN-042). `None` (or `Some(0)`) clears
    /// the cap, restoring unbounded ingestion. Applied at the boundary from
    /// `AlConfig::max_document_size_bytes` whenever configuration is (re)loaded.
    pub fn set_max_doc_bytes(&self, cap: Option<usize>) {
        self.max_doc_bytes
            .store(cap.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns the active per-document byte cap, or `None` when unbounded.
    #[inline]
    fn doc_cap(&self) -> Option<usize> {
        match self
            .max_doc_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            0 => None,
            n => Some(n),
        }
    }

    /// Whether `len` bytes exceed the configured cap. Logs a warning and
    /// returns `true` when the content should be rejected.
    fn exceeds_cap(&self, uri: &Url, len: usize) -> bool {
        match self.doc_cap() {
            Some(cap) if len > cap => {
                tracing::warn!(
                    uri = %uri,
                    bytes = len,
                    cap,
                    "DocumentStore: refusing document exceeding configured max_document_size_bytes (F-OPEN-042)"
                );
                true
            }
            _ => false,
        }
    }

    /// Acquire (or lazily create) the parse-coordination lock for `uri`.
    ///
    /// `parsing::get_or_parse` holds this across the parse-and-cache step so a
    /// keystroke-triggered burst of LSP requests (hover + completion +
    /// semantic-tokens fire near-simultaneously) doesn't trigger N concurrent
    /// `AlParser::parse_quick` calls for the same file. The first acquirer
    /// parses and caches; subsequent acquirers find the cache populated and
    /// short-circuit.
    pub fn parse_lock(&self, uri: &Url) -> std::sync::Arc<std::sync::Mutex<()>> {
        self.parse_locks
            .entry(uri.clone())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone()
    }

    pub fn open(&self, uri: Url, text: String) {
        // F-OPEN-042: refuse to ingest an oversized document. Any previously
        // open version of this URI is left untouched, and crucially the giant
        // text is never copied into the rope/cache.
        if self.exceeds_cap(&uri, text.len()) {
            return;
        }
        self.docs.insert(
            uri,
            Document {
                text: Rope::from_str(&text),
                text_cache: std::sync::Arc::new(text),
                version: 0,
            },
        );
    }

    pub fn close(&self, uri: &Url) {
        self.docs.remove(uri);
        self.trees.remove(uri);
        // Evict the parse-coordination lock too. A long-running daemon can open
        // and close thousands of distinct files over its lifetime; keeping their
        // locks around forever is a slow memory leak with no benefit, since the
        // lock only matters while the document is open and being parsed.
        self.parse_locks.remove(uri);
    }

    /// Return the document text as a cloned `String`.
    ///
    /// Prefer `get_text_arc` on hot paths to avoid deep-copying large file content.
    pub fn get_text(&self, uri: &Url) -> Option<String> {
        self.docs.get(uri).map(|d| d.text_cache.as_ref().clone())
    }

    /// Return the document text as a cheaply-cloneable `Arc<String>` (ISSUE-142 fix).
    ///
    /// Cloning the returned `Arc` is a pointer copy -- no string allocation.
    /// Use this on hot query paths to avoid deep-copying large file content.
    pub fn get_text_arc(&self, uri: &Url) -> Option<std::sync::Arc<String>> {
        self.docs
            .get(uri)
            .map(|d| std::sync::Arc::clone(&d.text_cache))
    }

    /// Return the document text and version captured under a **single** read
    /// lock, so the pair is guaranteed to be mutually consistent.
    ///
    /// Fetching text and version with two separate calls (`get_text_arc` then
    /// `get_version`) can interleave with `apply_changes` between them, yielding
    /// an `Arc<String>` for one version paired with the integer of another. The
    /// parse cache then keys on version, so a mismatched (old-text, new-tree)
    /// pair can escape — callers index `text.as_bytes()` with node ranges from a
    /// tree parsed against different text, producing wrong `utf8_text()` results
    /// or out-of-bounds slices. Capturing both atomically prevents that skew.
    pub fn get_text_and_version(&self, uri: &Url) -> Option<(std::sync::Arc<String>, i32)> {
        self.docs
            .get(uri)
            .map(|d| (std::sync::Arc::clone(&d.text_cache), d.version))
    }

    pub fn get_version(&self, uri: &Url) -> Option<i32> {
        self.docs.get(uri).map(|d| d.version)
    }

    /// Number of open documents.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether the store has no open documents.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn contains(&self, uri: &Url) -> bool {
        self.docs.contains_key(uri)
    }

    pub fn apply_changes(&self, uri: &Url, changes: &[TextChange]) {
        let _ = self.apply_changes_and_get(uri, changes);
    }

    /// Apply `changes` and return the resulting `(text, version)` pair captured
    /// under the **same** write lock that performed the mutation.
    ///
    /// `did_change` must feed the post-change text into the debounced
    /// diagnostics task. Doing that as `apply_changes(...)` followed by a
    /// separate `get_text(...)`/`get_version(...)` opens a TOCTOU window: a
    /// concurrent `did_change` notification (tower-lsp can interleave handlers
    /// under load) can run its own `apply_changes` between this call's mutation
    /// and the subsequent read, so the text/version handed to the diagnostics
    /// task belongs to a *later* keystroke than the one that scheduled it
    /// (F-OPEN-054). Returning the pair from inside the `get_mut` borrow closes
    /// that window — the returned text and version are always mutually
    /// consistent and reflect exactly the changes this call applied.
    ///
    /// Returns `None` only if the document is not open.
    pub fn apply_changes_and_get(
        &self,
        uri: &Url,
        changes: &[TextChange],
    ) -> Option<(std::sync::Arc<String>, i32)> {
        if let Some(mut doc) = self.docs.get_mut(uri) {
            for change in changes {
                if let Some(range) = change.range {
                    let start =
                        position_to_offset(&doc.text, range.start_line, range.start_character);
                    let end = position_to_offset(&doc.text, range.end_line, range.end_character);
                    match (start, end) {
                        // Well-formed forward range — apply.
                        (Some(s), Some(e)) if s <= e => {
                            doc.text.remove(s..e);
                            doc.text.insert(s, &change.text);
                        }
                        // Backward range (end < start). Silently dropping the
                        // change would mean the editor and server diverge with
                        // no obvious cause. Log a warn so the bug surfaces in
                        // daemon logs and skip this change. F-OPEN-(docstore-audit-2).
                        (Some(s), Some(e)) => {
                            tracing::warn!(
                                uri = %uri,
                                start = s,
                                end = e,
                                "DocumentStore: skipping malformed TextChange with end<start"
                            );
                        }
                        // One or both endpoints out-of-bounds. The position
                        // came from the LSP client; almost always a client bug.
                        // Same treatment: warn + skip rather than silently
                        // diverge.
                        _ => {
                            tracing::warn!(
                                uri = %uri,
                                start_line = range.start_line,
                                start_char = range.start_character,
                                end_line = range.end_line,
                                end_char = range.end_character,
                                doc_lines = doc.text.len_lines(),
                                "DocumentStore: skipping TextChange with out-of-bounds range"
                            );
                        }
                    }
                } else if self.exceeds_cap(uri, change.text.len()) {
                    // F-OPEN-042: an oversized full-document replacement is
                    // skipped rather than ingested. Like the malformed-range
                    // case above, the prior text is left intact; the editor
                    // should resync on the next edit.
                } else {
                    doc.text = Rope::from_str(&change.text);
                }
            }
            doc.text_cache = std::sync::Arc::new(doc.text.to_string());
            doc.version += 1;
            // Invalidate cached tree since the document changed
            self.trees.remove(uri);
            Some((std::sync::Arc::clone(&doc.text_cache), doc.version))
        } else {
            None
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

    /// Return the cached parse tree only if it matches **both** the live
    /// document version and the caller's `expected_version`.
    ///
    /// `get_cached_tree` validates against the live version alone, which lets a
    /// caller pair a tree for a newer version with text it captured at an older
    /// version (a TOCTOU race against `apply_changes`). Callers that hold an
    /// `Arc<String>` captured at a known version use this to ensure the returned
    /// tree was parsed against *that same* text.
    pub fn get_cached_tree_at_version(
        &self,
        uri: &Url,
        expected_version: i32,
    ) -> Option<tree_sitter::Tree> {
        let doc = self.docs.get(uri)?;
        let cached = self.trees.get(uri)?;
        if cached.0 == doc.version && cached.0 == expected_version {
            Some(cached.1.clone())
        } else {
            None
        }
    }

    /// Cache a parse tree for the given document URI and version.
    pub fn cache_tree(&self, uri: &Url, version: i32, tree: tree_sitter::Tree) {
        self.trees.insert(uri.clone(), (version, tree));
    }

    /// Number of live parse-coordination locks. Test-only accessor used to
    /// assert that `close()` evicts the per-URI lock and the map does not grow
    /// unbounded over a daemon's lifetime.
    #[cfg(test)]
    pub(crate) fn parse_locks_len(&self) -> usize {
        self.parse_locks.len()
    }
}

fn position_to_offset(rope: &Rope, line: u32, character: u32) -> Option<usize> {
    let line = line as usize;
    if line >= rope.len_lines() {
        return None;
    }
    // LSP positions are UTF-16 code units; ropey chars are Unicode scalar values.
    // Use ropey's native utf16_cu_to_char on the line slice to avoid
    // allocating a fresh String (rope.line(line).to_string() copies the
    // entire line on every position-to-offset call).
    let line_slice = rope.line(line);
    let line_byte_start = rope.line_to_byte(line);
    let utf16_idx = (character as usize).min(line_slice.len_utf16_cu());
    let char_in_line = line_slice.utf16_cu_to_char(utf16_idx);
    Some(rope.byte_to_char(line_byte_start) + char_in_line)
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
    fn test_close_evicts_parse_lock() {
        // Regression: parse_locks must not grow unbounded over a long-running
        // daemon's lifetime. Opening and closing distinct files should leave the
        // lock map empty, not accumulate one stale entry per file ever opened.
        let store = DocumentStore::new();
        assert_eq!(store.parse_locks_len(), 0);

        for i in 0..50 {
            let uri = test_uri(&format!("ephemeral{i}"));
            store.open(uri.clone(), "content".to_string());
            // Materialize the lock as get_or_parse would.
            let _lock = store.parse_lock(&uri);
            assert_eq!(store.parse_locks_len(), 1);
            store.close(&uri);
            assert_eq!(
                store.parse_locks_len(),
                0,
                "close() must evict the parse lock for {uri}"
            );
        }
    }

    #[test]
    fn test_full_replace() {
        let store = DocumentStore::new();
        let uri = test_uri("replace");
        store.open(uri.clone(), "old content".to_string());
        store.apply_changes(
            &uri,
            &[TextChange {
                range: None,
                text: "new content".to_string(),
            }],
        );
        assert_eq!(store.get_text(&uri), Some("new content".to_string()));
    }

    #[test]
    fn test_max_doc_bytes_rejects_oversized_open() {
        // F-OPEN-042: with a cap set, an oversized open() must not ingest the
        // document — neither the rope nor the cache should hold the giant text.
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(8));
        let uri = test_uri("huge");
        store.open(uri.clone(), "0123456789".to_string()); // 10 bytes > cap
        assert!(!store.contains(&uri), "oversized open() must be refused");
        assert_eq!(store.get_text(&uri), None);
    }

    #[test]
    fn test_max_doc_bytes_allows_within_cap() {
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(8));
        let uri = test_uri("small");
        store.open(uri.clone(), "01234".to_string()); // 5 bytes <= cap
        assert!(store.contains(&uri));
        assert_eq!(store.get_text(&uri), Some("01234".to_string()));
    }

    #[test]
    fn test_max_doc_bytes_boundary_equal_is_allowed() {
        // Exactly at the cap is allowed; only strictly-greater is refused.
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(5));
        let uri = test_uri("edge");
        store.open(uri.clone(), "01234".to_string()); // 5 bytes == cap
        assert!(store.contains(&uri));
    }

    #[test]
    fn test_no_cap_by_default_is_unbounded() {
        // Default (no cap) preserves prior unbounded behaviour.
        let store = DocumentStore::new();
        let uri = test_uri("nocap");
        let big = "x".repeat(1_000_000);
        store.open(uri.clone(), big.clone());
        assert_eq!(store.get_text(&uri), Some(big));
    }

    #[test]
    fn test_max_doc_bytes_cleared_restores_unbounded() {
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(4));
        let uri = test_uri("cleared");
        store.open(uri.clone(), "toolong".to_string());
        assert!(!store.contains(&uri), "should be refused while cap active");
        // Clearing the cap (None) restores unbounded ingestion.
        store.set_max_doc_bytes(None);
        store.open(uri.clone(), "toolong".to_string());
        assert_eq!(store.get_text(&uri), Some("toolong".to_string()));
    }

    #[test]
    fn test_max_doc_bytes_rejects_oversized_full_replace() {
        // A full-document replacement larger than the cap is skipped, leaving
        // the prior text intact rather than ingesting the giant payload.
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(8));
        let uri = test_uri("replace_huge");
        store.open(uri.clone(), "small".to_string());
        store.apply_changes(
            &uri,
            &[TextChange {
                range: None,
                text: "this is far too long".to_string(),
            }],
        );
        // Unchanged — the oversized replacement was refused.
        assert_eq!(store.get_text(&uri), Some("small".to_string()));
    }

    #[test]
    fn test_incremental_update() {
        let store = DocumentStore::new();
        let uri = test_uri("incr");
        store.open(uri.clone(), "hello world".to_string());
        store.apply_changes(
            &uri,
            &[TextChange {
                range: Some(TextRange {
                    start_line: 0,
                    start_character: 6,
                    end_line: 0,
                    end_character: 11,
                }),
                text: "AL".to_string(),
            }],
        );
        assert_eq!(store.get_text(&uri), Some("hello AL".to_string()));
    }

    #[test]
    fn test_version_increments() {
        let store = DocumentStore::new();
        let uri = test_uri("ver");
        store.open(uri.clone(), "v0".to_string());
        assert_eq!(store.get_version(&uri), Some(0));
        store.apply_changes(
            &uri,
            &[TextChange {
                range: None,
                text: "v1".to_string(),
            }],
        );
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
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::syntax::parser::language())
            .unwrap();
        let tree = parser.parse("content", None).unwrap();
        store.cache_tree(&uri, 0, tree);
        assert!(store.get_cached_tree(&uri).is_some());
        store.apply_changes(
            &uri,
            &[TextChange {
                range: None,
                text: "changed".to_string(),
            }],
        );
        assert!(store.get_cached_tree(&uri).is_none());
    }

    #[test]
    fn test_default_impl() {
        let store = DocumentStore::default();
        assert!(!store.contains(&test_uri("any")));
    }

    #[test]
    fn test_get_text_arc_cheap_clone() {
        let store = DocumentStore::new();
        let uri = test_uri("arc");
        store.open(uri.clone(), "arc content".to_string());
        let arc1 = store.get_text_arc(&uri).unwrap();
        let arc2 = store.get_text_arc(&uri).unwrap();
        // Both arcs point to the same allocation
        assert!(std::sync::Arc::ptr_eq(&arc1, &arc2));
        assert_eq!(arc1.as_str(), "arc content");
    }

    #[test]
    fn test_get_text_arc_updates_after_change() {
        let store = DocumentStore::new();
        let uri = test_uri("arc_change");
        store.open(uri.clone(), "original".to_string());
        let arc1 = store.get_text_arc(&uri).unwrap();
        store.apply_changes(
            &uri,
            &[TextChange {
                range: None,
                text: "updated".to_string(),
            }],
        );
        let arc2 = store.get_text_arc(&uri).unwrap();
        assert_eq!(arc1.as_str(), "original");
        assert_eq!(arc2.as_str(), "updated");
    }

    #[test]
    fn test_incremental_multiline_edits() {
        let store = DocumentStore::new();
        let uri = test_uri("multi");
        store.open(uri.clone(), "line one\nline two\nline three\n".to_string());

        store.apply_changes(
            &uri,
            &[TextChange {
                range: Some(TextRange {
                    start_line: 0,
                    start_character: 5,
                    end_line: 0,
                    end_character: 8,
                }),
                text: "1".to_string(),
            }],
        );
        assert_eq!(
            store.get_text(&uri),
            Some("line 1\nline two\nline three\n".to_string())
        );

        store.apply_changes(
            &uri,
            &[TextChange {
                range: Some(TextRange {
                    start_line: 1,
                    start_character: 5,
                    end_line: 1,
                    end_character: 8,
                }),
                text: "2".to_string(),
            }],
        );
        assert_eq!(
            store.get_text(&uri),
            Some("line 1\nline 2\nline three\n".to_string())
        );

        store.apply_changes(
            &uri,
            &[TextChange {
                range: Some(TextRange {
                    start_line: 0,
                    start_character: 6,
                    end_line: 1,
                    end_character: 6,
                }),
                text: "".to_string(),
            }],
        );
        assert_eq!(
            store.get_text(&uri),
            Some("line 1\nline three\n".to_string())
        );

        store.apply_changes(
            &uri,
            &[TextChange {
                range: Some(TextRange {
                    start_line: 0,
                    start_character: 0,
                    end_line: 0,
                    end_character: 0,
                }),
                text: "// ".to_string(),
            }],
        );
        assert_eq!(
            store.get_text(&uri),
            Some("// line 1\nline three\n".to_string())
        );

        assert_eq!(store.get_version(&uri), Some(4));
    }

    #[test]
    fn concurrent_open_and_read() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(DocumentStore::new());
        let mut handles = Vec::new();

        // Concurrent opens
        for i in 0..20 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let uri = Url::parse(&format!("file:///test/doc{i}.al")).unwrap();
                s.open(uri, format!("content {i}"));
            }));
        }

        // Concurrent reads
        for i in 0..20 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let uri = Url::parse(&format!("file:///test/doc{i}.al")).unwrap();
                let _ = s.get_text(&uri);
                let _ = s.get_text_arc(&uri);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Verify all 20 docs are accessible
        for i in 0..20 {
            let uri = Url::parse(&format!("file:///test/doc{i}.al")).unwrap();
            assert!(store.get_text(&uri).is_some(), "doc{i} should exist");
        }
    }

    /// T055 regression: position_to_offset clamps an over-large `character`
    /// (UTF-16 code units past end-of-line) to the line's UTF-16 length and
    /// returns Some, never None. The clamp is the contract relied on by
    /// apply_changes when a client sends a position past EOL during a paste.
    #[test]
    fn position_to_offset_clamps_overflow_character() {
        let rope = Rope::from_str("hello\nworld\n");
        // Line 0 is "hello" (5 utf16 cu). Pass character = 999.
        let off = position_to_offset(&rope, 0, 999);
        assert!(off.is_some(), "out-of-line character must clamp, not None");
        // Clamped result must point to end of line 0 INCLUDING the trailing
        // \n — ropey's `line(line)` slice covers the line break, so the
        // utf16_cu_to_char clamp lands at byte index 6 (just past 'hello\n').
        assert_eq!(off, Some(6));
    }

    /// T055 regression: a non-existent line still returns None — the clamp is
    /// for `character` only, not for `line`.
    #[test]
    fn position_to_offset_returns_none_for_overflow_line() {
        let rope = Rope::from_str("only one line\n");
        // Line index 99 is past EOF — must be None.
        let off = position_to_offset(&rope, 99, 0);
        assert!(off.is_none());
    }

    /// T055 regression: locks in the apply_changes lock-discipline invariant
    /// — the version bump and the tree-cache invalidation are observable as a
    /// single atomic step from any concurrent reader. Reading get_cached_tree
    /// either sees (old version, old tree) or (new version, no tree) — never
    /// (old version, no tree) or (new version, old tree).
    #[test]
    fn apply_changes_version_bump_and_tree_remove_are_consistent() {
        let store = DocumentStore::new();
        let uri = test_uri("lockdiscipline");
        store.open(uri.clone(), "v0".to_string());

        // Cache a tree at v0.
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::syntax::parser::language())
            .unwrap();
        let tree_v0 = parser.parse("v0", None).unwrap();
        store.cache_tree(&uri, 0, tree_v0);
        assert!(store.get_cached_tree(&uri).is_some());
        assert_eq!(store.get_version(&uri), Some(0));

        // Apply a change.
        store.apply_changes(
            &uri,
            &[TextChange {
                range: None,
                text: "v1".to_string(),
            }],
        );

        // Post-state: version is 1 AND tree is gone. Both must be true; if
        // apply_changes ever leaks the v0 tree under v1's version, this fails.
        assert_eq!(store.get_version(&uri), Some(1));
        assert!(store.get_cached_tree(&uri).is_none());
    }

    /// F-OPEN-054 regression: `apply_changes_and_get` returns the text and
    /// version produced by *this* call, captured under the same write lock that
    /// performed the mutation. `did_change` relies on this so the snapshot fed
    /// into the debounced diagnostics task can never be skewed forward by a
    /// concurrent `did_change` that interleaves between mutate and read.
    #[test]
    fn apply_changes_and_get_returns_post_change_snapshot() {
        let store = DocumentStore::new();
        let uri = test_uri("atomic_snapshot");
        store.open(uri.clone(), "v0".to_string());

        let (text, version) = store
            .apply_changes_and_get(
                &uri,
                &[TextChange {
                    range: None,
                    text: "v1".to_string(),
                }],
            )
            .expect("document is open");

        // The returned pair reflects exactly the change just applied — the new
        // text paired with the bumped version, never an older or newer mix.
        assert_eq!(text.as_str(), "v1");
        assert_eq!(version, 1);
        // And it matches the store's own atomic accessor.
        assert_eq!(
            store.get_text_and_version(&uri),
            Some((text, version)),
            "returned snapshot must equal the store's live (text, version)"
        );
    }

    /// F-OPEN-054: `apply_changes_and_get` returns `None` (rather than a stale
    /// snapshot) when the document is not open, so `did_change` skips scheduling
    /// diagnostics for a URI that was closed out from under it.
    #[test]
    fn apply_changes_and_get_returns_none_for_unopened_document() {
        let store = DocumentStore::new();
        let uri = test_uri("never_opened");
        let result = store.apply_changes_and_get(
            &uri,
            &[TextChange {
                range: None,
                text: "ignored".to_string(),
            }],
        );
        assert!(result.is_none());
        assert!(!store.contains(&uri));
    }

    #[test]
    fn concurrent_open_close_no_panic() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(DocumentStore::new());
        let mut handles = Vec::new();

        // Race opens and closes
        for i in 0..10 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let uri = Url::parse(&format!("file:///race/{i}.al")).unwrap();
                s.open(uri.clone(), format!("ver{i}"));
                s.close(&uri);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
        // All closed — docs should be gone
        for i in 0..10 {
            let uri = Url::parse(&format!("file:///race/{i}.al")).unwrap();
            assert!(store.get_text(&uri).is_none(), "doc{i} should be closed");
        }
    }

    #[test]
    fn apply_changes_skips_backward_range_and_logs() {
        // Negative regression: a malformed TextChange with end before start
        // should be skipped (not panic, not silently apply garbage). The
        // doc version still bumps because subsequent valid changes in the
        // same batch should still take effect; only THIS change is dropped.
        let store = DocumentStore::new();
        let uri = test_uri("backward");
        store.open(uri.clone(), "hello\n".to_string());
        let before = store.get_text(&uri).unwrap();

        store.apply_changes(
            &uri,
            &[TextChange {
                range: Some(TextRange {
                    start_line: 0,
                    start_character: 5, // after "hello"
                    end_line: 0,
                    end_character: 1, // BEFORE start
                }),
                text: "BAD".to_string(),
            }],
        );

        // Text must not contain the bad inserted string.
        let after = store.get_text(&uri).unwrap();
        assert!(
            !after.contains("BAD"),
            "backward TextRange should not have been applied; got: {after}"
        );
        // Original text content survives (the change was skipped).
        assert_eq!(
            after, before,
            "doc should be unchanged after skipped change"
        );
    }

    #[test]
    fn apply_changes_skips_out_of_bounds_range_and_logs() {
        // Negative regression: range pointing past EOF must be skipped, not
        // panicked over. The Ropey `remove` would itself accept an invalid
        // index path, but `position_to_offset` returns None for
        // out-of-bounds lines, which gets caught here.
        let store = DocumentStore::new();
        let uri = test_uri("oob");
        store.open(uri.clone(), "abc\n".to_string());

        store.apply_changes(
            &uri,
            &[TextChange {
                range: Some(TextRange {
                    start_line: 999,
                    start_character: 0,
                    end_line: 999,
                    end_character: 5,
                }),
                text: "EVIL".to_string(),
            }],
        );

        // No panic and no insertion.
        let after = store.get_text(&uri).unwrap();
        assert!(!after.contains("EVIL"));
        assert_eq!(after, "abc\n");
    }
}
