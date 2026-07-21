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

/// Default upper bound on the number of cached parse trees.
///
/// A long-running daemon opens every scanned workspace file (and lazily loads
/// more from disk on demand) without ever calling [`DocumentStore::close`], so
/// the tree cache would otherwise grow once per file ever touched and never
/// shrink. Parse trees are a pure derived cache — re-parsing a file costs
/// ~30-50 ms via `AlParser::parse_quick` — so bounding the cache and evicting
/// the least-recently-used entries trades a rare re-parse for a hard memory
/// ceiling. A typical BC project has thousands of `.al` files but only a
/// handful are hot at once, so 256 keeps the working set resident while
/// capping growth.
pub const DEFAULT_MAX_CACHED_TREES: usize = 256;

/// A cached parse tree plus the metadata needed for version validation and
/// approximate-LRU eviction.
struct CachedTree {
    /// Document version the tree was parsed against.
    version: i32,
    tree: tree_sitter::Tree,
    /// Monotonic access stamp, bumped on every cache read/write. The lowest
    /// stamps are the least-recently-used entries and are evicted first when
    /// the cache exceeds its cap.
    last_access: u64,
}

pub struct DocumentStore {
    docs: DashMap<Url, Document>,
    trees: DashMap<Url, CachedTree>,
    /// Monotonic source of access stamps for the approximate-LRU tree cache.
    tree_access_counter: std::sync::atomic::AtomicU64,
    /// Maximum number of parse trees to retain. `0` means unbounded; the constructor seeds it with
    /// [`DEFAULT_MAX_CACHED_TREES`]. Atomic so the daemon can retune it on a
    /// config update without locking the whole store.
    max_cached_trees: std::sync::atomic::AtomicUsize,
    /// Per-URI parse-coordination locks. Acquired by `parse_lock` so that
    /// concurrent `get_or_parse` calls for the same URI serialize on the
    /// expensive `AlParser::parse_quick` step instead of all racing into it.
    /// Distinct URIs still parse in parallel. Entries are evicted in `close()`
    /// alongside `docs`/`trees`, so a long-running daemon that opens and closes
    /// many distinct files over its lifetime does not accumulate stale locks.
    parse_locks: DashMap<Url, std::sync::Arc<std::sync::Mutex<()>>>,
    /// Optional per-document byte cap. `0` means "no cap" (the
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
    /// The last client-supplied LSP document version (from `didOpen`/`didChange`
    /// params). Distinct from `version`, which is the store's internal edit
    /// counter used to key the parse-tree cache. The client version is what the
    /// out-of-order-delivery guard must compare against — the internal counter
    /// starts at 0 and can never exceed the client's number, so comparing to it
    /// is dead code.
    client_version: i32,
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
            tree_access_counter: std::sync::atomic::AtomicU64::new(0),
            max_cached_trees: std::sync::atomic::AtomicUsize::new(DEFAULT_MAX_CACHED_TREES),
            parse_locks: DashMap::new(),
            max_doc_bytes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Set the maximum number of parse trees the cache will retain.
    /// `None` (or `Some(0)`) disables the cap, restoring unbounded caching;
    /// any other value bounds the cache and triggers least-recently-used
    /// eviction once exceeded. Safe to call at any time — the next
    /// [`cache_tree`](Self::cache_tree) enforces the new bound.
    pub fn set_max_cached_trees(&self, cap: Option<usize>) {
        self.max_cached_trees
            .store(cap.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
    }

    #[inline]
    fn next_tree_stamp(&self) -> u64 {
        self.tree_access_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Evict least-recently-used parse trees until the cache is within its cap.
    ///
    /// Called from [`cache_tree`](Self::cache_tree) after an insert. With a
    /// `0` (unbounded) cap this is a no-op. Eviction is approximate-LRU: it
    /// drops entries with the smallest `last_access` stamps, which are the
    /// ones least recently parsed or read. Evicting a tree only forces a
    /// future re-parse (correctness is unaffected — trees are a derived cache).
    fn evict_trees_over_cap(&self) {
        let cap = self
            .max_cached_trees
            .load(std::sync::atomic::Ordering::Relaxed);
        if cap == 0 {
            return;
        }
        let len = self.trees.len();
        if len <= cap {
            return;
        }
        // Collect (stamp, uri) pairs, sort ascending by stamp, and remove the
        // oldest until we are back within the cap. DashMap has no ordered
        // iteration, so we materialise the keys once per overflow. Overflow is
        // rare (only when the working set exceeds the cap), so the O(n log n)
        // sort is acceptable and bounded by `len`.
        let mut stamps: Vec<(u64, Url)> = self
            .trees
            .iter()
            .map(|e| (e.value().last_access, e.key().clone()))
            .collect();
        stamps.sort_unstable_by_key(|(stamp, _)| *stamp);
        let to_remove = stamps.len().saturating_sub(cap);
        for (_, uri) in stamps.into_iter().take(to_remove) {
            self.trees.remove(&uri);
        }
    }

    /// Set the per-document byte cap. `None` (or `Some(0)`) clears
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
                    "DocumentStore: refusing document exceeding configured max_document_size_bytes"
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
        // Leave an existing document untouched when the replacement exceeds the cap.
        if self.exceeds_cap(&uri, text.len()) {
            return;
        }
        self.docs.insert(
            uri,
            Document {
                text: Rope::from_str(&text),
                text_cache: std::sync::Arc::new(text),
                version: 0,
                client_version: 0,
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

    /// Prefer `get_text_arc` on hot paths to avoid deep-copying large file content.
    pub fn get_text(&self, uri: &Url) -> Option<String> {
        self.docs.get(uri).map(|d| d.text_cache.as_ref().clone())
    }

    /// Return the document text as a cheaply-cloneable `Arc<String>`.
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

    /// The last client-supplied LSP version for `uri`, or `None` if not open.
    pub fn get_client_version(&self, uri: &Url) -> Option<i32> {
        self.docs.get(uri).map(|d| d.client_version)
    }

    /// Record the client-supplied LSP version for `uri` (from `didOpen` /
    /// `didChange`). No-op if the document isn't open.
    pub fn set_client_version(&self, uri: &Url, version: i32) {
        if let Some(mut d) = self.docs.get_mut(uri) {
            d.client_version = version;
        }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn contains(&self, uri: &Url) -> bool {
        self.docs.contains_key(uri)
    }

    /// Snapshot of every currently-open document URI.
    ///
    /// Materialised into a `Vec` (DashMap has no stable iterator that can be
    /// held across `.await`) so the LSP `workspace/diagnostic` handler can walk
    /// the open set without pinning a shard lock while it runs per-file
    /// diagnostics.
    pub fn open_uris(&self) -> Vec<Url> {
        self.docs.iter().map(|e| e.key().clone()).collect()
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
    /// task belongs to a later keystroke than the one that scheduled it.
    /// Returning the pair from inside the `get_mut` borrow closes that window:
    /// the returned text and version are always mutually
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
                        (Some(s), Some(e)) if s <= e => {
                            doc.text.remove(s..e);
                            doc.text.insert(s, &change.text);
                        }
                        (Some(s), Some(e)) => {
                            tracing::warn!(
                                uri = %uri,
                                start = s,
                                end = e,
                                "DocumentStore: skipping malformed TextChange with end<start"
                            );
                        }
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
                    // Leave the prior text intact after an oversized
                    // full-document replacement; the editor should resync on
                    // the next edit.
                } else {
                    doc.text = Rope::from_str(&change.text);
                }
            }
            // Incremental inserts cannot be rejected up front based on their
            // final size, so warn when the completed batch exceeds the cap.
            let cap = self
                .max_doc_bytes
                .load(std::sync::atomic::Ordering::Relaxed);
            if cap != 0 && doc.text.len_bytes() > cap {
                tracing::warn!(
                    uri = %uri,
                    size = doc.text.len_bytes(),
                    cap,
                    "DocumentStore: document exceeds max_doc_bytes after incremental edits"
                );
            }
            doc.text_cache = std::sync::Arc::new(doc.text.to_string());
            doc.version += 1;
            self.trees.remove(uri);
            Some((std::sync::Arc::clone(&doc.text_cache), doc.version))
        } else {
            None
        }
    }

    /// Return the cached parse tree if the version matches the current document version.
    pub fn get_cached_tree(&self, uri: &Url) -> Option<tree_sitter::Tree> {
        let stamp = self.next_tree_stamp();
        let doc_version = self.docs.get(uri)?.version;
        let mut cached = self.trees.get_mut(uri)?;
        if cached.version == doc_version {
            cached.last_access = stamp;
            Some(cached.tree.clone())
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
        let stamp = self.next_tree_stamp();
        let doc_version = self.docs.get(uri)?.version;
        let mut cached = self.trees.get_mut(uri)?;
        if cached.version == doc_version && cached.version == expected_version {
            cached.last_access = stamp;
            Some(cached.tree.clone())
        } else {
            None
        }
    }

    /// Bounded by [`set_max_cached_trees`](Self::set_max_cached_trees): after
    /// inserting, the least-recently-used trees are evicted
    /// if the cache exceeds its cap, so a long-running daemon that opens many
    /// files cannot accumulate parse trees without limit.
    pub fn cache_tree(&self, uri: &Url, version: i32, tree: tree_sitter::Tree) {
        let stamp = self.next_tree_stamp();
        self.trees.insert(
            uri.clone(),
            CachedTree {
                version,
                tree,
                last_access: stamp,
            },
        );
        self.evict_trees_over_cap();
    }

    /// Number of cached parse trees. Test-only accessor used to assert the
    /// LRU cap bounds cache growth.
    #[cfg(test)]
    pub(crate) fn cached_trees_len(&self) -> usize {
        self.trees.len()
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
    // LSP line lengths exclude the trailing line break.
    let max_char = line_slice.len_utf16_cu() - line_break_utf16_width(line_slice);
    let utf16_idx = (character as usize).min(max_char);
    let char_in_line = line_slice.utf16_cu_to_char(utf16_idx);
    Some(rope.byte_to_char(line_byte_start) + char_in_line)
}

/// UTF-16 width of the trailing line break of `line` (a slice from
/// `Rope::line`): 2 for `\r\n`, 1 for a lone `\n`/`\r`, 0 if the line has no
/// terminator (e.g. the last line of a file without a trailing newline). Line
/// breaks are ASCII, so their UTF-16 width equals their char count.
fn line_break_utf16_width(line: ropey::RopeSlice) -> usize {
    let n = line.len_chars();
    if n == 0 {
        return 0;
    }
    match line.char(n - 1) {
        '\n' => {
            if n >= 2 && line.char(n - 2) == '\r' {
                2
            } else {
                1
            }
        }
        '\r' => 1,
        _ => 0,
    }
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

    fn parse_al(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&al_syntax::parser::language()).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn test_tree_cache_evicts_lru_when_over_cap() {
        let store = DocumentStore::new();
        store.set_max_cached_trees(Some(4));

        let mut uris = Vec::new();
        for i in 0..10 {
            let uri = test_uri(&format!("f{i}"));
            store.open(uri.clone(), "codeunit 50100 X { }".to_string());
            store.cache_tree(&uri, 0, parse_al("codeunit 50100 X { }"));
            uris.push(uri);
        }

        assert!(
            store.cached_trees_len() <= 4,
            "tree cache must stay within cap; got {}",
            store.cached_trees_len()
        );

        assert!(
            store.get_cached_tree(&uris[9]).is_some(),
            "most-recent tree must still be cached"
        );
        assert!(
            store.get_cached_tree(&uris[0]).is_none(),
            "oldest tree must have been evicted"
        );
    }

    #[test]
    fn test_tree_cache_lru_keeps_hot_entries() {
        // The eviction policy is least-recently-*used*, not least-recently-
        // *inserted*: an entry kept hot by reads must survive even though it
        // was cached early.
        let store = DocumentStore::new();
        store.set_max_cached_trees(Some(3));

        let hot = test_uri("hot");
        store.open(hot.clone(), "codeunit 50100 H { }".to_string());
        store.cache_tree(&hot, 0, parse_al("codeunit 50100 H { }"));

        // Insert several colder entries, touching `hot` between each so its
        // access stamp stays the newest.
        for i in 0..6 {
            let uri = test_uri(&format!("cold{i}"));
            store.open(uri.clone(), "codeunit 50100 C { }".to_string());
            store.cache_tree(&uri, 0, parse_al("codeunit 50100 C { }"));
            assert!(store.get_cached_tree(&hot).is_some());
        }

        assert!(store.cached_trees_len() <= 3);
        assert!(
            store.get_cached_tree(&hot).is_some(),
            "hot entry refreshed by reads must not be evicted"
        );
    }

    #[test]
    fn test_tree_cache_unbounded_when_cap_cleared() {
        let store = DocumentStore::new();
        store.set_max_cached_trees(None);
        for i in 0..50 {
            let uri = test_uri(&format!("u{i}"));
            store.open(uri.clone(), "codeunit 50100 U { }".to_string());
            store.cache_tree(&uri, 0, parse_al("codeunit 50100 U { }"));
        }
        assert_eq!(store.cached_trees_len(), 50);
    }

    #[test]
    fn test_default_cap_bounds_daemon_style_growth() {
        let store = DocumentStore::new();
        let n = DEFAULT_MAX_CACHED_TREES * 3;
        for i in 0..n {
            let uri = test_uri(&format!("daemon{i}"));
            store.open(uri.clone(), "codeunit 50100 D { }".to_string());
            store.cache_tree(&uri, 0, parse_al("codeunit 50100 D { }"));
        }
        assert!(
            store.cached_trees_len() <= DEFAULT_MAX_CACHED_TREES,
            "default cap must bound the cache; got {}",
            store.cached_trees_len()
        );
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
        parser.set_language(&al_syntax::parser::language()).unwrap();
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

        for i in 0..20 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let uri = Url::parse(&format!("file:///test/doc{i}.al")).unwrap();
                s.open(uri, format!("content {i}"));
            }));
        }

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

        for i in 0..20 {
            let uri = Url::parse(&format!("file:///test/doc{i}.al")).unwrap();
            assert!(store.get_text(&uri).is_some(), "doc{i} should exist");
        }
    }

    /// `position_to_offset` clamps an over-large `character`
    /// (UTF-16 code units past end-of-line) to the line's UTF-16 length and
    /// returns Some, never None. The clamp is the contract relied on by
    /// apply_changes when a client sends a position past EOL during a paste.
    #[test]
    fn position_to_offset_clamps_overflow_character() {
        let rope = Rope::from_str("hello\nworld\n");
        // Line 0 is "hello" (5 utf16 cu). Pass character = 999.
        let off = position_to_offset(&rope, 0, 999);
        assert!(off.is_some(), "out-of-line character must clamp, not None");
        // Clamped result must point to the end of the VISIBLE text on line 0
        // (char offset 5, just after 'hello'), BEFORE the trailing '\n' — LSP
        // clamps an over-EOL character to the line length, not past the
        // terminator. Landing at 6 would swallow the newline and merge lines.
        assert_eq!(off, Some(5));
    }

    #[test]
    fn position_to_offset_over_eol_does_not_merge_lines() {
        // replacing (0,2)..(0,999) with "XX" must NOT delete the newline.
        // The end position clamps to offset 5 (before '\n'), so 'hello\nworld\n'
        // becomes 'heXX\nworld\n', not 'heXXworld\n'.
        let rope = Rope::from_str("hello\nworld\n");
        let start = position_to_offset(&rope, 0, 2).unwrap();
        let end = position_to_offset(&rope, 0, 999).unwrap();
        assert_eq!(start, 2);
        assert_eq!(end, 5, "end must clamp before the newline, not past it");
        let mut edited = rope.clone();
        edited.remove(start..end);
        edited.insert(start, "XX");
        assert_eq!(edited.to_string(), "heXX\nworld\n");
    }

    #[test]
    fn position_to_offset_over_eol_crlf() {
        // A \r\n terminator (width 2) must be excluded from the clamp too.
        let rope = Rope::from_str("hi\r\nthere\r\n");
        let off = position_to_offset(&rope, 0, 999);
        // "hi" is 2 chars; clamp must land at char offset 2 (before \r\n).
        assert_eq!(off, Some(2));
    }

    /// A non-existent line still returns `None`; the clamp is
    /// for `character` only, not for `line`.
    #[test]
    fn position_to_offset_returns_none_for_overflow_line() {
        let rope = Rope::from_str("only one line\n");
        // Line index 99 is past EOF — must be None.
        let off = position_to_offset(&rope, 99, 0);
        assert!(off.is_none());
    }

    /// Locks in the `apply_changes` lock-discipline invariant
    /// — the version bump and the tree-cache invalidation are observable as a
    /// single atomic step from any concurrent reader. Reading get_cached_tree
    /// either sees (old version, old tree) or (new version, no tree) — never
    /// (old version, no tree) or (new version, old tree).
    #[test]
    fn apply_changes_version_bump_and_tree_remove_are_consistent() {
        let store = DocumentStore::new();
        let uri = test_uri("lockdiscipline");
        store.open(uri.clone(), "v0".to_string());

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&al_syntax::parser::language()).unwrap();
        let tree_v0 = parser.parse("v0", None).unwrap();
        store.cache_tree(&uri, 0, tree_v0);
        assert!(store.get_cached_tree(&uri).is_some());
        assert_eq!(store.get_version(&uri), Some(0));

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

    /// `apply_changes_and_get` returns the text and
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
        assert_eq!(
            store.get_text_and_version(&uri),
            Some((text, version)),
            "returned snapshot must equal the store's live (text, version)"
        );
    }

    #[test]
    fn client_version_is_tracked_separately_from_internal_counter() {
        // the client version is what the did_change guard compares against.
        // It is distinct from the internal edit counter, which starts at 0 and
        // is bumped per apply — the two must not be conflated.
        let store = DocumentStore::new();
        let uri = test_uri("client_version");
        store.open(uri.clone(), "x".to_string());

        // Fresh document: client version defaults to 0, internal version 0.
        assert_eq!(store.get_client_version(&uri), Some(0));
        assert_eq!(store.get_version(&uri), Some(0));

        // A client version can jump by more than 1 per notification.
        store.set_client_version(&uri, 5);
        assert_eq!(store.get_client_version(&uri), Some(5));

        // Applying an edit bumps the internal counter but NOT the client version.
        store
            .apply_changes_and_get(
                &uri,
                &[TextChange {
                    range: None,
                    text: "y".to_string(),
                }],
            )
            .unwrap();
        assert_eq!(store.get_version(&uri), Some(1));
        assert_eq!(
            store.get_client_version(&uri),
            Some(5),
            "internal edit did not change the client version"
        );

        // Unopened document: no client version.
        assert_eq!(store.get_client_version(&test_uri("nope")), None);
    }

    /// `apply_changes_and_get` returns `None` (rather than a stale
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
        for i in 0..10 {
            let uri = Url::parse(&format!("file:///race/{i}.al")).unwrap();
            assert!(store.get_text(&uri).is_none(), "doc{i} should be closed");
        }
    }

    #[test]
    fn apply_changes_skips_backward_range_and_logs() {
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

        let after = store.get_text(&uri).unwrap();
        assert!(
            !after.contains("BAD"),
            "backward TextRange should not have been applied; got: {after}"
        );
        assert_eq!(
            after, before,
            "doc should be unchanged after skipped change"
        );
    }

    #[test]
    fn apply_changes_skips_out_of_bounds_range_and_logs() {
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

        let after = store.get_text(&uri).unwrap();
        assert!(!after.contains("EVIL"));
        assert_eq!(after, "abc\n");
    }
}
