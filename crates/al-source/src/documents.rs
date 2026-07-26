//! Document store -- open file management with rope-based text.
//!
//! Transport-agnostic: uses [`TextChange`] instead of LSP-specific types.
//! al-lsp converts `TextDocumentContentChangeEvent` -> `TextChange` at the boundary.

use dashmap::DashMap;
use ropey::Rope;
use thiserror::Error;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// A rejected document mutation.
///
/// Document changes are transactional: when any change in a batch is invalid,
/// the store returns one of these errors and leaves the document text, internal
/// version, client version, and parse-tree cache untouched.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DocumentMutationError {
    #[error("document {uri} is {bytes} bytes, exceeding the configured maximum of {cap} bytes")]
    TooLarge { uri: Url, bytes: usize, cap: usize },

    #[error("document {uri} is not open")]
    NotOpen { uri: Url },

    #[error(
        "change range {start_line}:{start_character}-{end_line}:{end_character} is outside document {uri} ({document_lines} lines)"
    )]
    RangeOutOfBounds {
        uri: Url,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
        document_lines: usize,
    },

    #[error(
        "change range for document {uri} ends before it starts (character offsets {start}..{end})"
    )]
    BackwardRange { uri: Url, start: usize, end: usize },

    #[error(
        "client version {received} for document {uri} is not newer than the current version {current}"
    )]
    StaleClientVersion {
        uri: Url,
        received: i32,
        current: i32,
    },

    #[error("document size overflow while applying a change to {uri}")]
    SizeOverflow { uri: Url },

    #[error("cannot rename document {source_uri}: it is not open")]
    RenameSourceNotOpen { source_uri: Url },

    #[error("cannot rename document {source_uri} to {destination_uri}: the destination is open")]
    RenameDestinationOpen {
        source_uri: Box<Url>,
        destination_uri: Box<Url>,
    },
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

/// Byte accounting for memory directly owned by the open-document cache.
///
/// This deliberately excludes allocator slabs and tree-sitter's opaque tree
/// allocation; callers should use process RSS separately for that view.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentStoreMemoryStats {
    pub document_text_bytes: usize,
    pub document_index_bytes: usize,
    pub cached_tree_count: usize,
    pub tracked_bytes: usize,
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

    /// Return deterministic byte totals for owned document text and lookup
    /// keys. Tree-sitter does not expose a reliable allocation-size API, so
    /// tree count is reported separately rather than inventing a byte value.
    pub fn memory_stats(&self) -> DocumentStoreMemoryStats {
        let document_text_bytes = self
            .docs
            .iter()
            .map(|entry| entry.value().text_cache.len())
            .sum::<usize>();
        let document_index_bytes = self
            .docs
            .iter()
            .map(|entry| {
                std::mem::size_of::<Url>()
                    + entry.key().as_str().len()
                    + std::mem::size_of::<Document>()
            })
            .sum::<usize>()
            + self
                .trees
                .iter()
                .map(|entry| {
                    std::mem::size_of::<Url>()
                        + entry.key().as_str().len()
                        + std::mem::size_of::<CachedTree>()
                })
                .sum::<usize>()
            + self
                .parse_locks
                .iter()
                .map(|entry| {
                    std::mem::size_of::<Url>()
                        + entry.key().as_str().len()
                        + std::mem::size_of::<std::sync::Arc<std::sync::Mutex<()>>>()
                })
                .sum::<usize>();
        DocumentStoreMemoryStats {
            document_text_bytes,
            document_index_bytes,
            cached_tree_count: self.trees.len(),
            tracked_bytes: document_text_bytes + document_index_bytes,
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

    /// Validate a prospective cap against every currently-open document.
    ///
    /// Configuration hot reload uses this before publishing the new setting so
    /// lowering the cap cannot leave already-cached documents in a state the
    /// store would reject on their next edit.
    pub fn validate_max_doc_bytes(&self, cap: Option<usize>) -> Result<(), DocumentMutationError> {
        let Some(cap) = cap.filter(|cap| *cap != 0) else {
            return Ok(());
        };
        let mut oversized: Vec<(Url, usize)> = self
            .docs
            .iter()
            .filter_map(|entry| {
                let bytes = entry.value().text_cache.len();
                (bytes > cap).then(|| (entry.key().clone(), bytes))
            })
            .collect();
        oversized.sort_by(|left, right| left.0.cmp(&right.0));
        if let Some((uri, bytes)) = oversized.into_iter().next() {
            return Err(DocumentMutationError::TooLarge { uri, bytes, cap });
        }
        Ok(())
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

    /// Validate `len` against the configured per-document byte cap.
    fn validate_size(&self, uri: &Url, len: usize) -> Result<(), DocumentMutationError> {
        match self.doc_cap() {
            Some(cap) if len > cap => Err(DocumentMutationError::TooLarge {
                uri: uri.clone(),
                bytes: len,
                cap,
            }),
            _ => Ok(()),
        }
    }

    /// Validate prospective document content without mutating the store.
    ///
    /// Disk-backed writers use this before committing a file so a configured
    /// document cap cannot leave disk and in-memory generations divergent.
    pub fn validate_document_text(
        &self,
        uri: &Url,
        text: &str,
    ) -> Result<(), DocumentMutationError> {
        self.validate_size(uri, text.len())
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

    pub fn open(&self, uri: Url, text: String) -> Result<(), DocumentMutationError> {
        self.open_with_client_version(uri, text, 0)
    }

    /// Open a document and record the client-supplied version atomically.
    pub fn open_with_client_version(
        &self,
        uri: Url,
        text: String,
        client_version: i32,
    ) -> Result<(), DocumentMutationError> {
        self.validate_size(&uri, text.len())?;
        self.docs.insert(
            uri,
            Document {
                text: Rope::from_str(&text),
                text_cache: std::sync::Arc::new(text),
                version: 0,
                client_version,
            },
        );
        Ok(())
    }

    /// Replace an existing cached document without resetting its versions, or
    /// open it when absent. The occupied/vacant decision and mutation happen
    /// under one DashMap entry lock, so daemon-side disk refreshes cannot race
    /// a concurrent open between a separate `contains` check and update.
    pub fn replace_or_open(
        &self,
        uri: Url,
        text: String,
    ) -> Result<(std::sync::Arc<String>, i32), DocumentMutationError> {
        self.validate_size(&uri, text.len())?;
        let text = std::sync::Arc::new(text);
        let version = match self.docs.entry(uri.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let document = entry.get_mut();
                document.text = Rope::from_str(&text);
                document.text_cache = std::sync::Arc::clone(&text);
                document.version += 1;
                document.version
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(Document {
                    text: Rope::from_str(&text),
                    text_cache: std::sync::Arc::clone(&text),
                    version: 0,
                    client_version: 0,
                });
                0
            }
        };
        self.trees.remove(&uri);
        Ok((text, version))
    }

    /// Close a document and its derived caches.
    ///
    /// Returns `true` only when an open document was actually removed.
    pub fn close(&self, uri: &Url) -> bool {
        let removed = self.docs.remove(uri).is_some();
        self.trees.remove(uri);
        // Evict the parse-coordination lock too. A long-running daemon can open
        // and close thousands of distinct files over its lifetime; keeping their
        // locks around forever is a slow memory leak with no benefit, since the
        // lock only matters while the document is open and being parsed.
        self.parse_locks.remove(uri);
        removed
    }

    /// Move an open document and its derived state to a new URI without
    /// recreating it. This preserves both the internal edit counter and the
    /// last client-supplied LSP version across daemon-side file renames.
    ///
    /// Returns an explicit error when the source is not open or the destination
    /// is already open. In either case the store is left unchanged.
    pub fn rename(&self, old_uri: &Url, new_uri: Url) -> Result<(), DocumentMutationError> {
        if old_uri == &new_uri {
            return if self.docs.contains_key(old_uri) {
                Ok(())
            } else {
                Err(DocumentMutationError::RenameSourceNotOpen {
                    source_uri: old_uri.clone(),
                })
            };
        }
        if self.docs.contains_key(&new_uri) {
            return Err(DocumentMutationError::RenameDestinationOpen {
                source_uri: Box::new(old_uri.clone()),
                destination_uri: Box::new(new_uri),
            });
        }

        let Some((_, document)) = self.docs.remove(old_uri) else {
            return Err(DocumentMutationError::RenameSourceNotOpen {
                source_uri: old_uri.clone(),
            });
        };
        match self.docs.entry(new_uri.clone()) {
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(document);
            }
            dashmap::mapref::entry::Entry::Occupied(_) => {
                // A concurrent didOpen won the destination between the check
                // and entry acquisition. Restore the original document rather
                // than overwriting either editor state.
                self.docs.insert(old_uri.clone(), document);
                return Err(DocumentMutationError::RenameDestinationOpen {
                    source_uri: Box::new(old_uri.clone()),
                    destination_uri: Box::new(new_uri),
                });
            }
        }

        if let Some((_, tree)) = self.trees.remove(old_uri) {
            self.trees.insert(new_uri.clone(), tree);
        }
        if let Some((_, lock)) = self.parse_locks.remove(old_uri) {
            self.parse_locks.insert(new_uri, lock);
        }
        Ok(())
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

    /// Return text and the last client-supplied LSP version from one document
    /// snapshot. Callers can additionally compare the returned `Arc` with a
    /// later [`get_text_arc`](Self::get_text_arc) result by pointer identity to
    /// detect close/reopen and same-content replacement races.
    pub fn get_text_and_client_version(&self, uri: &Url) -> Option<(std::sync::Arc<String>, i32)> {
        self.docs
            .get(uri)
            .map(|d| (std::sync::Arc::clone(&d.text_cache), d.client_version))
    }

    pub fn get_version(&self, uri: &Url) -> Option<i32> {
        self.docs.get(uri).map(|d| d.version)
    }

    /// The last client-supplied LSP version for `uri`, or `None` if not open.
    pub fn get_client_version(&self, uri: &Url) -> Option<i32> {
        self.docs.get(uri).map(|d| d.client_version)
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

    pub fn apply_changes(
        &self,
        uri: &Url,
        changes: &[TextChange],
    ) -> Result<(), DocumentMutationError> {
        self.apply_changes_and_get(uri, changes).map(|_| ())
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
    /// The complete batch is transactional. An invalid range or an intermediate
    /// size above the configured cap rejects the batch without changing any
    /// observable document state.
    pub fn apply_changes_and_get(
        &self,
        uri: &Url,
        changes: &[TextChange],
    ) -> Result<(std::sync::Arc<String>, i32), DocumentMutationError> {
        self.apply_changes_inner(uri, changes, None)
    }

    /// Apply a client-versioned batch, rejecting stale or duplicate versions
    /// under the same document lock that commits the text.
    pub fn apply_versioned_changes_and_get(
        &self,
        uri: &Url,
        client_version: i32,
        changes: &[TextChange],
    ) -> Result<(std::sync::Arc<String>, i32), DocumentMutationError> {
        self.apply_changes_inner(uri, changes, Some(client_version))
    }

    fn apply_changes_inner(
        &self,
        uri: &Url,
        changes: &[TextChange],
        client_version: Option<i32>,
    ) -> Result<(std::sync::Arc<String>, i32), DocumentMutationError> {
        let mut doc = self
            .docs
            .get_mut(uri)
            .ok_or_else(|| DocumentMutationError::NotOpen { uri: uri.clone() })?;

        if let Some(received) = client_version {
            if received <= doc.client_version {
                return Err(DocumentMutationError::StaleClientVersion {
                    uri: uri.clone(),
                    received,
                    current: doc.client_version,
                });
            }
        }

        // Work on a candidate rope so a later invalid change cannot expose a
        // partially applied batch.
        let mut candidate = doc.text.clone();
        for change in changes {
            if let Some(range) = change.range {
                let start = position_to_offset(&candidate, range.start_line, range.start_character);
                let end = position_to_offset(&candidate, range.end_line, range.end_character);
                let (start, end) = match (start, end) {
                    (Some(start), Some(end)) => (start, end),
                    _ => {
                        return Err(DocumentMutationError::RangeOutOfBounds {
                            uri: uri.clone(),
                            start_line: range.start_line,
                            start_character: range.start_character,
                            end_line: range.end_line,
                            end_character: range.end_character,
                            document_lines: candidate.len_lines(),
                        });
                    }
                };
                if start > end {
                    return Err(DocumentMutationError::BackwardRange {
                        uri: uri.clone(),
                        start,
                        end,
                    });
                }

                let removed_bytes = candidate.slice(start..end).len_bytes();
                let next_len = candidate
                    .len_bytes()
                    .checked_sub(removed_bytes)
                    .and_then(|len| len.checked_add(change.text.len()))
                    .ok_or_else(|| DocumentMutationError::SizeOverflow { uri: uri.clone() })?;
                self.validate_size(uri, next_len)?;
                candidate.remove(start..end);
                candidate.insert(start, &change.text);
            } else {
                self.validate_size(uri, change.text.len())?;
                candidate = Rope::from_str(&change.text);
            }
        }

        let text = std::sync::Arc::new(candidate.to_string());
        doc.text = candidate;
        doc.text_cache = std::sync::Arc::clone(&text);
        doc.version += 1;
        if let Some(client_version) = client_version {
            doc.client_version = client_version;
        }
        self.trees.remove(uri);
        Ok((text, doc.version))
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
        store.open(uri.clone(), "hello world".to_string()).unwrap();
        assert_eq!(store.get_text(&uri), Some("hello world".to_string()));
        assert!(store.contains(&uri));
    }

    #[test]
    fn test_close() {
        let store = DocumentStore::new();
        let uri = test_uri("close");
        store.open(uri.clone(), "content".to_string()).unwrap();
        assert!(store.contains(&uri));
        assert!(store.close(&uri));
        assert!(!store.contains(&uri));
        assert_eq!(store.get_text(&uri), None);
    }

    #[test]
    fn test_rename_preserves_text_versions_tree_and_parse_lock() {
        let store = DocumentStore::new();
        let old_uri = test_uri("rename_old");
        let new_uri = test_uri("rename_new");
        store
            .open_with_client_version(old_uri.clone(), "codeunit 50100 A { }".to_string(), 42)
            .unwrap();
        store
            .apply_changes(
                &old_uri,
                &[TextChange {
                    range: None,
                    text: "codeunit 50100 A { trigger OnRun() begin end; }".to_string(),
                }],
            )
            .unwrap();
        let tree = parse_al(&store.get_text(&old_uri).unwrap());
        store.cache_tree(&old_uri, store.get_version(&old_uri).unwrap(), tree);
        let old_lock = store.parse_lock(&old_uri);

        store.rename(&old_uri, new_uri.clone()).unwrap();
        assert!(!store.contains(&old_uri));
        assert_eq!(store.get_version(&new_uri), Some(1));
        assert_eq!(store.get_client_version(&new_uri), Some(42));
        assert!(store.get_text(&new_uri).unwrap().contains("OnRun"));
        assert!(store.get_cached_tree(&new_uri).is_some());
        assert!(std::sync::Arc::ptr_eq(
            &old_lock,
            &store.parse_lock(&new_uri)
        ));
    }

    #[test]
    fn test_rename_refuses_an_already_open_destination() {
        let store = DocumentStore::new();
        let old_uri = test_uri("rename_source");
        let new_uri = test_uri("rename_destination");
        store.open(old_uri.clone(), "source".to_string()).unwrap();
        store
            .open(new_uri.clone(), "destination".to_string())
            .unwrap();

        assert_eq!(
            store.rename(&old_uri, new_uri.clone()),
            Err(DocumentMutationError::RenameDestinationOpen {
                source_uri: Box::new(old_uri.clone()),
                destination_uri: Box::new(new_uri.clone()),
            })
        );
        assert_eq!(store.get_text(&old_uri).as_deref(), Some("source"));
        assert_eq!(store.get_text(&new_uri).as_deref(), Some("destination"));
    }

    #[test]
    fn test_close_evicts_parse_lock() {
        let store = DocumentStore::new();
        assert_eq!(store.parse_locks_len(), 0);

        for i in 0..50 {
            let uri = test_uri(&format!("ephemeral{i}"));
            store.open(uri.clone(), "content".to_string()).unwrap();
            // Materialize the lock as get_or_parse would.
            let _lock = store.parse_lock(&uri);
            assert_eq!(store.parse_locks_len(), 1);
            assert!(store.close(&uri));
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
            store
                .open(uri.clone(), "codeunit 50100 X { }".to_string())
                .unwrap();
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
        store
            .open(hot.clone(), "codeunit 50100 H { }".to_string())
            .unwrap();
        store.cache_tree(&hot, 0, parse_al("codeunit 50100 H { }"));

        // Insert several colder entries, touching `hot` between each so its
        // access stamp stays the newest.
        for i in 0..6 {
            let uri = test_uri(&format!("cold{i}"));
            store
                .open(uri.clone(), "codeunit 50100 C { }".to_string())
                .unwrap();
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
            store
                .open(uri.clone(), "codeunit 50100 U { }".to_string())
                .unwrap();
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
            store
                .open(uri.clone(), "codeunit 50100 D { }".to_string())
                .unwrap();
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
        store.open(uri.clone(), "old content".to_string()).unwrap();
        store
            .apply_changes(
                &uri,
                &[TextChange {
                    range: None,
                    text: "new content".to_string(),
                }],
            )
            .unwrap();
        assert_eq!(store.get_text(&uri), Some("new content".to_string()));
    }

    #[test]
    fn test_max_doc_bytes_rejects_oversized_open() {
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(8));
        let uri = test_uri("huge");
        let error = store
            .open(uri.clone(), "0123456789".to_string())
            .expect_err("10 bytes must exceed the 8-byte cap");
        assert_eq!(
            error,
            DocumentMutationError::TooLarge {
                uri: uri.clone(),
                bytes: 10,
                cap: 8,
            }
        );
        assert!(!store.contains(&uri), "oversized open() must be refused");
        assert_eq!(store.get_text(&uri), None);
    }

    #[test]
    fn test_max_doc_bytes_allows_within_cap() {
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(8));
        let uri = test_uri("small");
        store.open(uri.clone(), "01234".to_string()).unwrap(); // 5 bytes <= cap
        assert!(store.contains(&uri));
        assert_eq!(store.get_text(&uri), Some("01234".to_string()));
    }

    #[test]
    fn test_max_doc_bytes_boundary_equal_is_allowed() {
        // Exactly at the cap is allowed; only strictly-greater is refused.
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(5));
        let uri = test_uri("edge");
        store.open(uri.clone(), "01234".to_string()).unwrap(); // 5 bytes == cap
        assert!(store.contains(&uri));
    }

    #[test]
    fn test_no_cap_by_default_is_unbounded() {
        // Default (no cap) preserves prior unbounded behaviour.
        let store = DocumentStore::new();
        let uri = test_uri("nocap");
        let big = "x".repeat(1_000_000);
        store.open(uri.clone(), big.clone()).unwrap();
        assert_eq!(store.get_text(&uri), Some(big));
    }

    #[test]
    fn test_max_doc_bytes_cleared_restores_unbounded() {
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(4));
        let uri = test_uri("cleared");
        assert!(matches!(
            store.open(uri.clone(), "toolong".to_string()),
            Err(DocumentMutationError::TooLarge { .. })
        ));
        assert!(!store.contains(&uri), "should be refused while cap active");
        // Clearing the cap (None) restores unbounded ingestion.
        store.set_max_doc_bytes(None);
        store.open(uri.clone(), "toolong".to_string()).unwrap();
        assert_eq!(store.get_text(&uri), Some("toolong".to_string()));
    }

    #[test]
    fn prospective_cap_rejects_an_already_open_oversized_document() {
        let store = DocumentStore::new();
        let uri = test_uri("prospective_cap");
        store.open(uri.clone(), "ninebytes".to_string()).unwrap();

        assert_eq!(
            store.validate_max_doc_bytes(Some(8)),
            Err(DocumentMutationError::TooLarge {
                uri,
                bytes: 9,
                cap: 8,
            })
        );
        assert_eq!(store.doc_cap(), None, "validation must not publish the cap");
    }

    #[test]
    fn test_max_doc_bytes_rejects_oversized_full_replace() {
        // A full-document replacement larger than the cap is rejected,
        // leaving text, version, and derived state unchanged.
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(8));
        let uri = test_uri("replace_huge");
        store.open(uri.clone(), "small".to_string()).unwrap();
        let error = store
            .apply_changes(
                &uri,
                &[TextChange {
                    range: None,
                    text: "this is far too long".to_string(),
                }],
            )
            .expect_err("oversized replacement must fail explicitly");
        assert!(matches!(
            error,
            DocumentMutationError::TooLarge {
                bytes: 20,
                cap: 8,
                ..
            }
        ));
        assert_eq!(store.get_text(&uri), Some("small".to_string()));
        assert_eq!(store.get_version(&uri), Some(0));
    }

    #[test]
    fn test_incremental_update() {
        let store = DocumentStore::new();
        let uri = test_uri("incr");
        store.open(uri.clone(), "hello world".to_string()).unwrap();
        store
            .apply_changes(
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
            )
            .unwrap();
        assert_eq!(store.get_text(&uri), Some("hello AL".to_string()));
    }

    #[test]
    fn test_version_increments() {
        let store = DocumentStore::new();
        let uri = test_uri("ver");
        store.open(uri.clone(), "v0".to_string()).unwrap();
        assert_eq!(store.get_version(&uri), Some(0));
        store
            .apply_changes(
                &uri,
                &[TextChange {
                    range: None,
                    text: "v1".to_string(),
                }],
            )
            .unwrap();
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
        store.open(uri.clone(), "content".to_string()).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&al_syntax::parser::language()).unwrap();
        let tree = parser.parse("content", None).unwrap();
        store.cache_tree(&uri, 0, tree);
        assert!(store.get_cached_tree(&uri).is_some());
        store
            .apply_changes(
                &uri,
                &[TextChange {
                    range: None,
                    text: "changed".to_string(),
                }],
            )
            .unwrap();
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
        store.open(uri.clone(), "arc content".to_string()).unwrap();
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
        store.open(uri.clone(), "original".to_string()).unwrap();
        let arc1 = store.get_text_arc(&uri).unwrap();
        store
            .apply_changes(
                &uri,
                &[TextChange {
                    range: None,
                    text: "updated".to_string(),
                }],
            )
            .unwrap();
        let arc2 = store.get_text_arc(&uri).unwrap();
        assert_eq!(arc1.as_str(), "original");
        assert_eq!(arc2.as_str(), "updated");
    }

    #[test]
    fn test_incremental_multiline_edits() {
        let store = DocumentStore::new();
        let uri = test_uri("multi");
        store
            .open(uri.clone(), "line one\nline two\nline three\n".to_string())
            .unwrap();

        store
            .apply_changes(
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
            )
            .unwrap();
        assert_eq!(
            store.get_text(&uri),
            Some("line 1\nline two\nline three\n".to_string())
        );

        store
            .apply_changes(
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
            )
            .unwrap();
        assert_eq!(
            store.get_text(&uri),
            Some("line 1\nline 2\nline three\n".to_string())
        );

        store
            .apply_changes(
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
            )
            .unwrap();
        assert_eq!(
            store.get_text(&uri),
            Some("line 1\nline three\n".to_string())
        );

        store
            .apply_changes(
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
            )
            .unwrap();
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
                s.open(uri, format!("content {i}")).unwrap();
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
        store.open(uri.clone(), "v0".to_string()).unwrap();

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&al_syntax::parser::language()).unwrap();
        let tree_v0 = parser.parse("v0", None).unwrap();
        store.cache_tree(&uri, 0, tree_v0);
        assert!(store.get_cached_tree(&uri).is_some());
        assert_eq!(store.get_version(&uri), Some(0));

        store
            .apply_changes(
                &uri,
                &[TextChange {
                    range: None,
                    text: "v1".to_string(),
                }],
            )
            .unwrap();

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
        store.open(uri.clone(), "v0".to_string()).unwrap();

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
        store
            .open_with_client_version(uri.clone(), "x".to_string(), 5)
            .unwrap();

        // Client and internal versions are tracked independently.
        assert_eq!(store.get_client_version(&uri), Some(5));
        assert_eq!(store.get_version(&uri), Some(0));

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

    /// `apply_changes_and_get` reports a closed document explicitly rather than
    /// returning a stale snapshot or silently discarding the edit.
    #[test]
    fn apply_changes_and_get_errors_for_unopened_document() {
        let store = DocumentStore::new();
        let uri = test_uri("never_opened");
        let result = store.apply_changes_and_get(
            &uri,
            &[TextChange {
                range: None,
                text: "ignored".to_string(),
            }],
        );
        assert_eq!(
            result,
            Err(DocumentMutationError::NotOpen { uri: uri.clone() })
        );
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
                s.open(uri.clone(), format!("ver{i}")).unwrap();
                assert!(s.close(&uri));
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
    fn apply_changes_rejects_backward_range_atomically() {
        let store = DocumentStore::new();
        let uri = test_uri("backward");
        store.open(uri.clone(), "hello\n".to_string()).unwrap();
        let before = store.get_text(&uri).unwrap();
        let before_version = store.get_version(&uri);

        let error = store
            .apply_changes(
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
            )
            .expect_err("backward range must be rejected");
        assert_eq!(
            error,
            DocumentMutationError::BackwardRange {
                uri: uri.clone(),
                start: 5,
                end: 1,
            }
        );

        let after = store.get_text(&uri).unwrap();
        assert!(
            !after.contains("BAD"),
            "backward TextRange should not have been applied; got: {after}"
        );
        assert_eq!(
            after, before,
            "doc should be unchanged after rejected change"
        );
        assert_eq!(store.get_version(&uri), before_version);
    }

    #[test]
    fn apply_changes_rejects_out_of_bounds_range_atomically() {
        let store = DocumentStore::new();
        let uri = test_uri("oob");
        store.open(uri.clone(), "abc\n".to_string()).unwrap();

        let error = store
            .apply_changes(
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
            )
            .expect_err("out-of-bounds range must be rejected");
        assert_eq!(
            error,
            DocumentMutationError::RangeOutOfBounds {
                uri: uri.clone(),
                start_line: 999,
                start_character: 0,
                end_line: 999,
                end_character: 5,
                document_lines: 2,
            }
        );

        let after = store.get_text(&uri).unwrap();
        assert!(!after.contains("EVIL"));
        assert_eq!(after, "abc\n");
    }

    #[test]
    fn later_invalid_change_rolls_back_the_entire_batch() {
        let store = DocumentStore::new();
        let uri = test_uri("transactional_batch");
        store
            .open(uri.clone(), "alpha\nbeta\n".to_string())
            .unwrap();

        let result = store.apply_changes(
            &uri,
            &[
                TextChange {
                    range: Some(TextRange {
                        start_line: 0,
                        start_character: 0,
                        end_line: 0,
                        end_character: 5,
                    }),
                    text: "changed".to_string(),
                },
                TextChange {
                    range: Some(TextRange {
                        start_line: 99,
                        start_character: 0,
                        end_line: 99,
                        end_character: 0,
                    }),
                    text: "invalid".to_string(),
                },
            ],
        );

        assert!(matches!(
            result,
            Err(DocumentMutationError::RangeOutOfBounds { .. })
        ));
        assert_eq!(store.get_text(&uri).as_deref(), Some("alpha\nbeta\n"));
        assert_eq!(store.get_version(&uri), Some(0));
    }

    #[test]
    fn incremental_change_cannot_cross_document_size_cap() {
        let store = DocumentStore::new();
        store.set_max_doc_bytes(Some(8));
        let uri = test_uri("incremental_cap");
        store.open(uri.clone(), "12345678".to_string()).unwrap();

        let error = store
            .apply_changes(
                &uri,
                &[TextChange {
                    range: Some(TextRange {
                        start_line: 0,
                        start_character: 8,
                        end_line: 0,
                        end_character: 8,
                    }),
                    text: "9".to_string(),
                }],
            )
            .expect_err("incremental insert over the cap must fail");

        assert!(matches!(
            error,
            DocumentMutationError::TooLarge {
                bytes: 9,
                cap: 8,
                ..
            }
        ));
        assert_eq!(store.get_text(&uri).as_deref(), Some("12345678"));
        assert_eq!(store.get_version(&uri), Some(0));
    }

    #[test]
    fn versioned_change_rejects_stale_version_without_mutation() {
        let store = DocumentStore::new();
        let uri = test_uri("stale_version");
        store
            .open_with_client_version(uri.clone(), "v5".to_string(), 5)
            .unwrap();

        let error = store
            .apply_versioned_changes_and_get(
                &uri,
                5,
                &[TextChange {
                    range: None,
                    text: "duplicate".to_string(),
                }],
            )
            .expect_err("duplicate client version must be rejected");

        assert_eq!(
            error,
            DocumentMutationError::StaleClientVersion {
                uri: uri.clone(),
                received: 5,
                current: 5,
            }
        );
        assert_eq!(store.get_text(&uri).as_deref(), Some("v5"));
        assert_eq!(store.get_version(&uri), Some(0));
        assert_eq!(store.get_client_version(&uri), Some(5));
    }

    #[test]
    fn versioned_change_commits_text_and_client_version_together() {
        let store = DocumentStore::new();
        let uri = test_uri("versioned_commit");
        store
            .open_with_client_version(uri.clone(), "v5".to_string(), 5)
            .unwrap();

        let (text, internal_version) = store
            .apply_versioned_changes_and_get(
                &uri,
                9,
                &[TextChange {
                    range: None,
                    text: "v9".to_string(),
                }],
            )
            .unwrap();

        assert_eq!(text.as_str(), "v9");
        assert_eq!(internal_version, 1);
        assert_eq!(store.get_client_version(&uri), Some(9));
    }
}
