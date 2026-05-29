//! Workspace .al file index.
//!
//! Scans a project directory for `.al` files, reads their content, extracts
//! object declarations, and provides fast lookup by object name or file path.
//! This is the single implementation used by both LSP (stdio) and daemon modes.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use dashmap::DashMap;

/// Maximum number of .al files to scan. Prevents runaway memory usage
/// if a workspace root accidentally includes a huge directory tree.
pub const MAX_WORKSPACE_FILES: usize = 10_000;

/// Maximum directory depth to recurse into.
const MAX_DEPTH: usize = 10;

/// Maximum size of an individual `.al` file we will read into the index.
/// Real AL source files are KB-scale; a multi-megabyte `.al` is almost
/// certainly a build artifact, generated blob, or adversarial input. Reading
/// it would pin its full contents in the in-memory `files` map. `scan` and
/// `incremental_scan` skip (and log) any file larger than this (F-OPEN-019).
pub const MAX_AL_FILE_BYTES: u64 = 50 * 1024 * 1024; // 50 MiB

/// Snapshot of file metadata used for change detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    /// Last modification time.
    pub modified: SystemTime,
    /// File size in bytes.
    pub size: u64,
}

impl FileMetadata {
    /// Read metadata for the given path. Returns `None` on error.
    pub fn read(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: meta.len(),
        })
    }
}

/// Result of an incremental workspace scan.
#[derive(Debug, Default)]
pub struct ScanDelta {
    /// Files that were added (new) or modified (re-parsed).
    pub changed: Vec<PathBuf>,
    /// Files that were removed from disk and dropped from the index.
    pub removed: Vec<PathBuf>,
}

impl ScanDelta {
    /// Total number of files affected (added + modified + removed).
    pub fn total(&self) -> usize {
        self.changed.len() + self.removed.len()
    }

    /// Whether any files changed.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Cached metadata extracted from parsing an AL file's object declaration.
///
/// Stored alongside file content so callers can read object kind/name/range
/// without re-parsing the file on every request.
#[derive(Debug, Clone)]
pub struct CachedObjectInfo {
    /// Object kind string (e.g. "table", "page", "codeunit").
    pub kind: String,
    /// Object numeric ID, if present.
    pub id: Option<i64>,
    /// Object name with original casing.
    pub name: String,
    /// Byte range of the object declaration in the file.
    pub range: tree_sitter::Range,
}

/// Index of all .al files in a workspace directory.
///
/// Provides three synchronized maps:
/// - `files`: path → file content (full text)
/// - `objects`: lowercase object name → file path
/// - `path_to_object`: file path → lowercase object name (reverse index)
/// - `object_info`: file path → cached object declaration metadata
/// - `file_trees`: file path → cached parse tree
///
/// Incremental scanning is supported via `incremental_scan`: only files whose
/// mtime or size changed since the last scan are re-read and re-indexed.
/// A procedure/event location cached at index time.
#[derive(Debug, Clone)]
pub struct CachedProcedureInfo {
    /// File path containing this procedure.
    pub file: PathBuf,
    /// Selection range of the procedure name (for go-to-definition).
    pub selection_range: crate::queries::Range,
}

pub struct FileIndex {
    /// File path → full text content.
    /// Public — used by the al-lsp binary for stats and by daemon dispatchers.
    pub files: DashMap<PathBuf, String>,
    /// Lowercase object name → file path. Internal: invariant-coupled to
    /// `path_to_object`; mutate only via the impl methods (T004).
    pub(crate) objects: DashMap<String, PathBuf>,
    /// File path → lowercase object name (reverse index for O(1) cleanup).
    /// Internal: invariant-coupled to `objects` (T004).
    pub(crate) path_to_object: DashMap<PathBuf, String>,
    /// File path → (mtime, size) snapshot taken at last index time.
    /// Internal: only used by `incremental_scan` to detect changed files (T004).
    pub(crate) file_metadata: DashMap<PathBuf, FileMetadata>,
    /// File path → cached object declaration metadata (avoids re-parsing for workspace/symbol).
    /// Public — al-lsp's DAP path needs object_id ↔ file_path lookups.
    pub object_info: DashMap<PathBuf, CachedObjectInfo>,
    /// File path → cached parse tree (avoids re-parsing for cross-file queries).
    /// Internal: invariant-coupled to `files` content; mutate only via impl methods (T004).
    pub(crate) file_trees: DashMap<PathBuf, tree_sitter::Tree>,
    /// File path → cached document symbols (avoids re-extracting for cross-file queries).
    pub(crate) file_symbols: DashMap<PathBuf, Vec<crate::queries::AlDocumentSymbol>>,
    /// Lowercase procedure/event name → location (reverse index for O(1) go-to-definition).
    pub(crate) procedures: DashMap<String, Vec<CachedProcedureInfo>>,
    /// File path → list of procedure names (for cleanup on file remove/update).
    path_to_procedures: DashMap<PathBuf, Vec<String>>,
}

impl FileIndex {
    /// Create an empty file index.
    pub fn new() -> Self {
        Self {
            files: DashMap::new(),
            objects: DashMap::new(),
            path_to_object: DashMap::new(),
            file_metadata: DashMap::new(),
            object_info: DashMap::new(),
            file_trees: DashMap::new(),
            file_symbols: DashMap::new(),
            procedures: DashMap::new(),
            path_to_procedures: DashMap::new(),
        }
    }

    /// Look up procedure/event locations by name (case-insensitive).
    pub fn lookup_procedures(&self, name: &str) -> Option<Vec<CachedProcedureInfo>> {
        self.procedures
            .get(&name.to_lowercase())
            .map(|v| v.value().clone())
    }

    /// Get the cached parse tree and text for a workspace file (not an open document).
    ///
    /// Returns `(text, tree)` from the cache. Background files are always cached
    /// at index time via `add_file_with_meta`, so a miss means the file was never indexed.
    pub fn get_cached_parse(&self, path: &Path) -> Option<(String, tree_sitter::Tree)> {
        let text = self.files.get(path)?.value().clone();
        let tree = self.file_trees.get(path)?.value().clone();
        Some((text, tree))
    }

    /// Get cached document symbols for a workspace file.
    ///
    /// Returns the symbols extracted at index time. Falls back to extracting
    /// from the cached parse tree if symbols were not cached (shouldn't happen).
    pub fn get_cached_symbols(&self, path: &Path) -> Option<Vec<crate::queries::AlDocumentSymbol>> {
        if let Some(entry) = self.file_symbols.get(path) {
            return Some(entry.value().clone());
        }
        // Fallback: extract from cached parse tree
        let (text, tree) = self.get_cached_parse(path)?;
        let symbols: Vec<crate::queries::AlDocumentSymbol> =
            crate::syntax::extract_document_symbols(&tree, &text)
                .into_iter()
                .map(Into::into)
                .collect();
        self.file_symbols
            .insert(path.to_path_buf(), symbols.clone());
        Some(symbols)
    }

    /// Scan a directory tree for .al files and index their contents.
    ///
    /// Skips hidden directories, `node_modules`, and `.alpackages`.
    /// On a re-scan, files that were previously indexed but no longer
    /// exist on disk are removed from every index (primary + secondary
    /// object-name / object-id / procedure maps). F-010: a stale
    /// re-scan was leaving deleted AL objects discoverable by go-to-
    /// definition, workspace symbols, and code actions.
    /// Returns the number of files freshly indexed (not the resulting
    /// total — call [`len`] for that).
    pub fn scan(&self, root: &Path) -> usize {
        let mut count = 0;
        let mut on_disk: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        self.walk_al_files(root, &mut count, 0, &mut |path| {
            on_disk.insert(path.clone());
            if al_file_exceeds_cap(&path) {
                return;
            }
            match std::fs::read_to_string(&path) {
                Ok(content) => self.add_file(path, content),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable .al file during scan");
                }
            }
        });
        // Drop entries for files that disappeared from disk between scans.
        // remove_file already cleans the secondary maps (object-name,
        // object-id, procedures, file_metadata, file_trees, file_symbols).
        let indexed_paths: Vec<PathBuf> = self.files.iter().map(|e| e.key().clone()).collect();
        for path in indexed_paths {
            if !on_disk.contains(&path) {
                self.remove_file(&path);
            }
        }
        count
    }

    /// Incrementally scan a directory tree for changed `.al` files.
    ///
    /// Compared to [`scan`], this method:
    /// - Skips files whose mtime and size have not changed since the last scan.
    /// - Re-parses and re-indexes only changed or new files.
    /// - Removes files that were deleted from disk.
    ///
    /// Returns a [`ScanDelta`] describing what was added/changed/removed.
    /// The first call after construction behaves like a full scan (no prior
    /// metadata recorded).
    pub fn incremental_scan(&self, root: &Path) -> ScanDelta {
        let mut delta = ScanDelta::default();

        // Collect all .al paths currently on disk.
        let mut on_disk: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut walk_count = 0;
        self.walk_al_files(root, &mut walk_count, 0, &mut |path| {
            on_disk.insert(path);
        });

        // --- Step 1: detect new and modified files ---
        for path in &on_disk {
            let current_meta = match FileMetadata::read(path) {
                Some(m) => m,
                None => {
                    tracing::warn!(path = %path.display(), "skipping file: cannot read metadata");
                    continue;
                }
            };
            // Skip oversized .al files before reading them into memory
            // (F-OPEN-019). We already have the size from the metadata read,
            // so no extra syscall is needed here.
            if current_meta.size > MAX_AL_FILE_BYTES {
                tracing::warn!(
                    path = %path.display(),
                    size = current_meta.size,
                    cap = MAX_AL_FILE_BYTES,
                    "skipping .al file: exceeds per-file size cap"
                );
                // If this path was previously indexed (it was small enough at
                // the time) and has since grown past the cap, its old content,
                // parse tree, and object/procedure mappings are now stale and
                // will never be refreshed. Evict it so the index never serves
                // outdated data for an oversized file. Record it as removed so
                // callers can invalidate dependent caches.
                if self.files.contains_key(path) {
                    self.remove_file(path);
                    delta.removed.push(path.clone());
                }
                continue;
            }
            let needs_index = match self.file_metadata.get(path) {
                Some(prev) => *prev != current_meta,
                None => true, // new file
            };
            if needs_index {
                match std::fs::read_to_string(path) {
                    Ok(content) => {
                        self.add_file_with_meta(path.clone(), content, Some(current_meta));
                        delta.changed.push(path.clone());
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "skipping unreadable .al file during incremental scan");
                    }
                }
            }
        }

        // --- Step 2: detect deleted files ---
        let indexed_paths: Vec<PathBuf> = self.files.iter().map(|e| e.key().clone()).collect();
        for path in indexed_paths {
            if !on_disk.contains(&path) {
                self.remove_file(&path);
                delta.removed.push(path);
            }
        }

        delta
    }

    /// Add a single file to the index. Used when a file is opened/changed.
    ///
    /// If the file was previously indexed, the old object-name mapping is
    /// removed before the new one is inserted, so no stale entries remain.
    /// Records current mtime+size so `incremental_scan` can skip this file
    /// if it hasn't changed since.
    pub fn add_file(&self, path: PathBuf, content: String) {
        self.add_file_with_meta(path, content, None);
    }

    /// Like [`add_file`] but accepts pre-read metadata to avoid a redundant
    /// `stat()` when the caller already has it (e.g., `incremental_scan`).
    fn add_file_with_meta(&self, path: PathBuf, content: String, meta: Option<FileMetadata>) {
        // Record metadata snapshot for incremental scan change detection.
        let meta = meta.or_else(|| FileMetadata::read(&path));
        if let Some(m) = meta {
            self.file_metadata.insert(path.clone(), m);
        }
        // Remove the old object-name mapping for this path (if any), so
        // stale entries don't linger when the object is renamed/replaced.
        if let Some((_, old_obj_name)) = self.path_to_object.remove(&path) {
            self.remove_owned_object_mapping(&old_obj_name, &path);
        }
        // Remove stale procedure entries for this file before re-indexing.
        self.remove_procedures_for_file(&path);

        // Parse once; cache the tree for cross-file queries and extract object metadata.
        let result = crate::syntax::AlParser::parse_quick(&content);
        self.index_from_result(path, content, &result.tree);
    }

    /// Add a file to the index using a pre-parsed tree, skipping the internal parse.
    ///
    /// Used by `on_document_change` in `al-core::workspace` to avoid a double-parse:
    /// the caller parses once to warm the document cache, then passes the same tree here.
    ///
    /// The caller is responsible for removing old object-name mappings via
    /// `path_to_object` and cleaning up stale procedure entries before calling this.
    pub fn add_file_with_tree(&self, path: PathBuf, content: String, tree: tree_sitter::Tree) {
        // Remove the old object-name mapping for this path (if any).
        if let Some((_, old_obj_name)) = self.path_to_object.remove(&path) {
            self.remove_owned_object_mapping(&old_obj_name, &path);
        }
        // Remove stale procedure entries for this file before re-indexing.
        self.remove_procedures_for_file(&path);

        self.index_from_result(path, content, &tree);
    }

    /// Snapshot the per-path procedure-name list as it currently stands in
    /// the index. Used to detect topology changes between two consecutive
    /// indexings of the same file: if the post-edit set equals the pre-edit
    /// set, only the call-edge cache needs to be invalidated. F-OPEN-066.
    pub fn procedures_snapshot(&self, path: &Path) -> Vec<String> {
        self.path_to_procedures
            .get(path)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    /// Core indexing body: populate all index maps from a parsed tree.
    ///
    /// Called by both `add_file_with_meta` (after an internal parse) and
    /// `add_file_with_tree` (after a caller-supplied parse). The tree must
    /// correspond to `content`.
    ///
    /// **Atomicity:** this function mutates 7 DashMaps (`file_trees`,
    /// `objects`, `path_to_object`, `object_info`, `file_symbols`, `files`,
    /// `path_to_procedures`). A panic between any two of those mutations
    /// would leave the index in a split state — e.g. `file_trees` populated
    /// but `files` missing the text. Tree-sitter parsing and symbol
    /// extraction are well-tested and don't panic on real inputs, so this is
    /// latent (F-OPEN-077). Any future contributor adding a new map mutation
    /// here should consider widening the window or grouping mutations into
    /// a single transactional helper if the cost becomes meaningful.
    fn index_from_result(&self, path: PathBuf, content: String, tree: &tree_sitter::Tree) {
        // Cache the tree unconditionally — all files benefit from it.
        self.file_trees.insert(path.clone(), tree.clone());
        if let Some(obj_info) = crate::syntax::find_object_declaration(tree, &content) {
            let obj_name = obj_info.name.to_lowercase();
            self.objects.insert(obj_name.clone(), path.clone());
            self.path_to_object.insert(path.clone(), obj_name);
            self.object_info.insert(
                path.clone(),
                CachedObjectInfo {
                    kind: obj_info.kind,
                    id: obj_info.id,
                    name: obj_info.name,
                    range: obj_info.range,
                },
            );
        } else {
            // No object declaration — remove any stale cached metadata.
            self.object_info.remove(&path);
        }

        // Index procedure/event names for O(1) go-to-definition.
        // Inline the AlSymbolKind::Function/Event predicate here to avoid an
        // upward dependency from file_index (core infrastructure) into the
        // queries module (higher-level LSP feature code).
        let doc_symbols = crate::syntax::extract_document_symbols(tree, &content);
        let mut proc_names = Vec::new();
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    let kind: crate::queries::AlSymbolKind = child.kind.into();
                    let is_proc = matches!(
                        kind,
                        crate::queries::AlSymbolKind::Function
                            | crate::queries::AlSymbolKind::Event
                    );
                    if is_proc {
                        let proc_key = child.name.to_lowercase();
                        let info = CachedProcedureInfo {
                            file: path.clone(),
                            selection_range: child.selection_range.into(),
                        };
                        self.procedures
                            .entry(proc_key.clone())
                            .or_default()
                            .push(info);
                        proc_names.push(proc_key);
                    }
                }
            }
        }
        if !proc_names.is_empty() {
            self.path_to_procedures.insert(path.clone(), proc_names);
        }

        // Cache document symbols for cross-file queries (convert to crate-local type).
        let al_doc_symbols: Vec<crate::queries::AlDocumentSymbol> =
            doc_symbols.into_iter().map(Into::into).collect();
        self.file_symbols.insert(path.clone(), al_doc_symbols);

        self.files.insert(path, content);
    }

    /// Remove a file from the index (e.g., on file close or delete).
    pub fn remove_file(&self, path: &Path) {
        self.files.remove(path);
        self.file_metadata.remove(path);
        self.file_trees.remove(path);
        self.file_symbols.remove(path);
        self.object_info.remove(path);
        self.remove_procedures_for_file(path);
        if let Some((_, obj_name)) = self.path_to_object.remove(path) {
            self.remove_owned_object_mapping(&obj_name, path);
        }
    }

    /// F-040: only remove the `objects[name] → path` mapping when it still
    /// points at `path`. Without this guard, two AL objects sharing a name
    /// across kinds / packages — say a table `Foo` and a page `Foo` — both
    /// insert under `objects["foo"]`. Whoever inserted last wins; removing
    /// the OTHER file then dropped the surviving object's mapping and made
    /// it unfindable. Keys can still collide on insert (DashMap is a single-
    /// value map), but stale-key removal is now collision-safe.
    fn remove_owned_object_mapping(&self, obj_name: &str, path: &Path) {
        self.objects
            .remove_if(obj_name, |_, current_path| current_path == path);
    }

    /// Remove all procedure index entries associated with a file path.
    fn remove_procedures_for_file(&self, path: &Path) {
        if let Some((_, old_proc_names)) = self.path_to_procedures.remove(path) {
            for proc_name in old_proc_names {
                // Use the entry API so the retain-then-maybe-remove sequence
                // happens under a single, continuously-held shard lock. The
                // previous get_mut → drop → remove sequence released the lock
                // between the empty check and the removal: a concurrent
                // `index_from_result` could push a fresh, legitimate entry for
                // the same proc name in that window, which `remove` would then
                // wipe out, causing intermittent go-to-definition misses.
                use dashmap::mapref::entry::Entry;
                if let Entry::Occupied(mut occ) = self.procedures.entry(proc_name) {
                    occ.get_mut().retain(|e| e.file != path);
                    if occ.get().is_empty() {
                        occ.remove();
                    }
                }
            }
        }
    }

    /// Look up file content by path.
    pub fn get_content(&self, path: &Path) -> Option<String> {
        self.files.get(path).map(|r| r.value().clone())
    }

    /// Look up file path by object name (case-insensitive).
    pub fn find_by_object_name(&self, name: &str) -> Option<PathBuf> {
        self.objects
            .get(&name.to_lowercase())
            .map(|r| r.value().clone())
    }

    /// Number of indexed files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Walk the directory tree for `.al` files, calling `visitor` for each one.
    ///
    /// Skips hidden directories, `node_modules`, and `.alpackages`.
    /// Respects `MAX_DEPTH` and stops after `MAX_WORKSPACE_FILES` visits.
    fn walk_al_files(
        &self,
        dir: &Path,
        count: &mut usize,
        depth: usize,
        visitor: &mut dyn FnMut(PathBuf),
    ) {
        if depth > MAX_DEPTH || *count >= MAX_WORKSPACE_FILES {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry_result in entries {
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    // Per-entry errors (e.g. permission denied on a specific
                    // child) are surfaced rather than silently dropped by the
                    // old `flatten()`, so unreadable parts of the workspace are
                    // observable in the logs (F-OPEN-018).
                    tracing::debug!(error = %e, dir = %dir.display(), "skipping directory entry due to error");
                    continue;
                }
            };
            let path = entry.path();

            // Use the cached file type from the directory read instead of
            // `path.is_dir()`, which would issue a fresh `stat()` syscall per
            // entry (F-OPEN-017). Fall back to `false` (treat as a file) on
            // error; a genuinely unreadable entry will fail later on read.
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

            if is_dir {
                let dir_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Skip hidden directories, node_modules, and .alpackages
                if dir_name.starts_with('.')
                    || dir_name == "node_modules"
                    || dir_name == ".alpackages"
                {
                    continue;
                }

                self.walk_al_files(&path, count, depth + 1, visitor);
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("al"))
            {
                if *count >= MAX_WORKSPACE_FILES {
                    return;
                }
                visitor(path);
                *count += 1;
            }
        }
    }
}

impl Default for FileIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns `true` if the `.al` file at `path` is larger than
/// [`MAX_AL_FILE_BYTES`] and should be skipped. Logs a warning when it is.
/// On metadata-read failure returns `false` so the caller proceeds to its
/// own read (which will then surface the I/O error). (F-OPEN-019)
fn al_file_exceeds_cap(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_AL_FILE_BYTES => {
            tracing::warn!(
                path = %path.display(),
                size = meta.len(),
                cap = MAX_AL_FILE_BYTES,
                "skipping .al file: exceeds per-file size cap"
            );
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temp directory with some .al files for testing.
    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();

        // Create a valid AL file with object declaration
        let al_content = r#"table 50100 "My Test Table"
{
    fields
    {
        field(1; "Code"; Code[20]) { }
    }
}
"#;
        fs::write(dir.path().join("MyTestTable.al"), al_content).unwrap();

        // Create another AL file
        let page_content = r#"page 50100 "My Test Page"
{
    SourceTable = "My Test Table";
}
"#;
        fs::write(dir.path().join("MyTestPage.al"), page_content).unwrap();

        // Create a non-AL file (should be ignored)
        fs::write(dir.path().join("readme.md"), "# Hello").unwrap();

        // Create a subdirectory with an AL file
        let sub_dir = dir.path().join("src");
        fs::create_dir(&sub_dir).unwrap();
        let codeunit_content = r#"codeunit 50100 "My Codeunit"
{
    procedure DoSomething()
    begin
    end;
}
"#;
        fs::write(sub_dir.join("MyCodeunit.al"), codeunit_content).unwrap();

        // Create a hidden directory (should be skipped)
        let hidden = dir.path().join(".hidden");
        fs::create_dir(&hidden).unwrap();
        fs::write(hidden.join("Secret.al"), "table 1 Secret {}").unwrap();

        // Create .alpackages directory (should be skipped)
        let packages = dir.path().join(".alpackages");
        fs::create_dir(&packages).unwrap();
        fs::write(packages.join("Dep.al"), "table 2 Dep {}").unwrap();

        dir
    }

    #[test]
    fn scan_finds_al_files() {
        let dir = setup_test_dir();
        let index = FileIndex::new();
        let count = index.scan(dir.path());

        assert_eq!(
            count, 3,
            "Should find 3 .al files (2 root + 1 subdirectory)"
        );
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn al_file_exceeds_cap_helper() {
        let dir = tempfile::tempdir().unwrap();

        // A normal small file is under the cap.
        let small = dir.path().join("small.al");
        fs::write(&small, "codeunit 1 X {}").unwrap();
        assert!(!al_file_exceeds_cap(&small));

        // A missing file does not count as "over cap" (caller surfaces I/O err).
        assert!(!al_file_exceeds_cap(&dir.path().join("missing.al")));

        // A file just over the cap is rejected. Use a sparse file via set_len
        // so the test stays cheap.
        let big = dir.path().join("big.al");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
        drop(f);
        assert!(al_file_exceeds_cap(&big));
    }

    #[test]
    fn scan_skips_oversized_al_file() {
        let dir = tempfile::tempdir().unwrap();
        // One normal file and one oversized (sparse) file.
        fs::write(dir.path().join("ok.al"), "codeunit 1 Ok {}").unwrap();
        let big = dir.path().join("big.al");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
        drop(f);

        let index = FileIndex::new();
        // walk still counts both .al files, but only the small one is indexed.
        index.scan(dir.path());
        assert_eq!(index.len(), 1, "oversized .al file should not be indexed");
        assert!(index.get_content(&dir.path().join("ok.al")).is_some());
        assert!(index.get_content(&big).is_none());
    }

    #[test]
    fn incremental_scan_evicts_file_that_grew_oversized() {
        // A file indexed while small, then grown past the cap, must be evicted
        // from the index on the next incremental scan rather than left serving
        // stale content/parse-trees/object mappings.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Grower.al");
        fs::write(&path, "codeunit 50100 \"Grower\" { }").unwrap();

        let index = FileIndex::new();
        let first = index.incremental_scan(dir.path());
        assert_eq!(first.changed.len(), 1, "small file indexed on first scan");
        assert!(index.get_content(&path).is_some());
        assert_eq!(index.len(), 1);

        // Grow the file past the cap (sparse) so the next scan must skip it.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
        drop(f);

        let second = index.incremental_scan(dir.path());
        assert!(
            second.removed.contains(&path),
            "oversized-grown file should be reported removed"
        );
        assert!(
            index.get_content(&path).is_none(),
            "oversized-grown file must be evicted, not left stale"
        );
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn scan_skips_hidden_and_alpackages() {
        let dir = setup_test_dir();
        let index = FileIndex::new();
        index.scan(dir.path());

        // .hidden/Secret.al and .alpackages/Dep.al should NOT be indexed
        assert!(index.find_by_object_name("secret").is_none());
        assert!(index.find_by_object_name("dep").is_none());
    }

    #[test]
    fn scan_indexes_object_names() {
        let dir = setup_test_dir();
        let index = FileIndex::new();
        index.scan(dir.path());

        // Should find objects by name (case-insensitive)
        assert!(index.find_by_object_name("my test table").is_some());
        assert!(index.find_by_object_name("My Test Table").is_some());
        assert!(index.find_by_object_name("MY TEST TABLE").is_some());
        assert!(index.find_by_object_name("my test page").is_some());
        assert!(index.find_by_object_name("my codeunit").is_some());
    }

    #[test]
    fn get_content_returns_file_text() {
        let dir = setup_test_dir();
        let index = FileIndex::new();
        index.scan(dir.path());

        let table_path = dir.path().join("MyTestTable.al");
        let content = index.get_content(&table_path);
        assert!(content.is_some());
        assert!(content.unwrap().contains("My Test Table"));
    }

    #[test]
    fn add_file_indexes_content_and_object() {
        let index = FileIndex::new();
        let path = PathBuf::from("/tmp/test/NewTable.al");
        let content = r#"table 50200 "Added Table" { }"#.to_string();

        index.add_file(path.clone(), content);

        assert_eq!(index.len(), 1);
        assert!(index.find_by_object_name("added table").is_some());
        assert!(index.get_content(&path).is_some());
    }

    #[test]
    fn remove_file_cleans_all_maps() {
        let index = FileIndex::new();
        let path = PathBuf::from("/tmp/test/RemoveMe.al");
        let content = r#"page 50300 "Remove Me" { }"#.to_string();

        index.add_file(path.clone(), content);
        assert_eq!(index.len(), 1);
        assert!(index.find_by_object_name("remove me").is_some());

        index.remove_file(&path);
        assert_eq!(index.len(), 0);
        assert!(index.find_by_object_name("remove me").is_none());
        assert!(index.get_content(&path).is_none());
    }

    /// F-040 positive: when two files share an object name (table Foo,
    /// page Foo), removing one file must NOT drop the other file's mapping.
    #[test]
    fn remove_file_preserves_other_owners_object_mapping() {
        let index = FileIndex::new();
        let table_path = PathBuf::from("/tmp/test/TableFoo.al");
        let page_path = PathBuf::from("/tmp/test/PageFoo.al");

        index.add_file(
            table_path.clone(),
            r#"table 50100 "Foo" { fields { } }"#.to_string(),
        );
        // Insert the page second — its insert overwrites the `objects[foo]`
        // entry. Before F-040, removing the table would then have
        // unconditionally dropped the surviving page's mapping.
        index.add_file(
            page_path.clone(),
            r#"page 50100 "Foo" { layout { } actions { } }"#.to_string(),
        );

        index.remove_file(&table_path);

        // The page's mapping must survive — find_by_object_name should still
        // resolve "foo" to the page file.
        let resolved = index.find_by_object_name("foo");
        assert_eq!(
            resolved.as_deref(),
            Some(page_path.as_path()),
            "F-040: surviving owner's object mapping was dropped"
        );
    }

    /// F-040 negative: when the file being removed IS the current owner of
    /// `objects[name]`, the mapping is correctly removed (no orphan).
    #[test]
    fn remove_file_drops_objects_mapping_when_owner() {
        let index = FileIndex::new();
        let path = PathBuf::from("/tmp/test/SoloFoo.al");
        index.add_file(path.clone(), r#"codeunit 50100 "SoloFoo" { }"#.to_string());
        assert!(index.find_by_object_name("solofoo").is_some());
        index.remove_file(&path);
        assert!(
            index.find_by_object_name("solofoo").is_none(),
            "owner removal should clear the mapping"
        );
    }

    #[test]
    fn empty_index() {
        let index = FileIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn find_nonexistent_object() {
        let index = FileIndex::new();
        assert!(index.find_by_object_name("does not exist").is_none());
    }

    #[test]
    fn scan_ignores_non_al_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.md"), "# Hello").unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();

        let index = FileIndex::new();
        let count = index.scan(dir.path());
        assert_eq!(count, 0);
        assert!(index.is_empty());
    }

    #[test]
    fn reverse_index_maps_path_to_object() {
        let index = FileIndex::new();
        let path = PathBuf::from("/tmp/test/Reverse.al");
        let content = r#"codeunit 50400 "Reverse Test" { }"#.to_string();

        index.add_file(path.clone(), content);

        let obj_name = index.path_to_object.get(&path);
        assert!(obj_name.is_some());
        assert_eq!(obj_name.unwrap().value(), "reverse test");
    }

    // -------------------------------------------------------------------------
    // Incremental scan tests
    // -------------------------------------------------------------------------

    /// Verify that `incremental_scan` indexes new files on the first call.
    #[test]
    fn incremental_scan_first_call_behaves_like_full_scan() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        let delta = index.incremental_scan(dir.path());

        // 3 files: MyTestTable.al, MyTestPage.al, src/MyCodeunit.al
        assert_eq!(
            delta.changed.len(),
            3,
            "All 3 files should be indexed on first call"
        );
        assert_eq!(delta.removed.len(), 0);
        assert_eq!(index.len(), 3);
    }

    /// Verify that a second `incremental_scan` with no changes produces an empty delta.
    #[test]
    fn incremental_scan_no_changes_produces_empty_delta() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        // First scan: index everything.
        index.incremental_scan(dir.path());

        // Second scan: nothing changed on disk.
        let delta = index.incremental_scan(dir.path());

        assert!(
            delta.is_empty(),
            "No files should be re-indexed when nothing changed"
        );
        assert_eq!(index.len(), 3);
    }

    /// Verify that only the modified file is re-indexed when a single file changes.
    #[test]
    fn incremental_scan_only_reparses_changed_file() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        // Initial full scan.
        index.incremental_scan(dir.path());
        assert_eq!(index.len(), 3);

        // Sleep briefly so the mtime will differ (filesystem resolution is typically 1ms+).
        // We write new content to just one file and force a metadata change by
        // explicitly bumping the mtime via setting file times.
        let table_path = dir.path().join("MyTestTable.al");
        let new_content = r#"table 50101 "My Modified Table"
{
    fields
    {
        field(1; "Code"; Code[20]) { }
    }
}
"#;

        // Write new content — this changes mtime and size.
        fs::write(&table_path, new_content).unwrap();

        // Flush the metadata cache entry so the next stat picks up the new mtime.
        // (Some Linux filesystems have 10ms mtime resolution; if the write is
        // within the same tick the test would still pass because size changes.)

        let delta = index.incremental_scan(dir.path());

        // Only the one modified file should be in the changed list.
        assert_eq!(
            delta.changed.len(),
            1,
            "Only 1 file should be re-indexed after a single modification"
        );
        assert_eq!(delta.removed.len(), 0);
        assert_eq!(delta.changed[0], table_path);

        // The index should reflect the new object name.
        assert!(
            index.find_by_object_name("my modified table").is_some(),
            "New object name should be findable after incremental re-index"
        );
        // The old name should no longer be in the index.
        assert!(
            index.find_by_object_name("my test table").is_none(),
            "Old object name should be removed after file is re-indexed"
        );
        // Total file count unchanged.
        assert_eq!(index.len(), 3);
    }

    /// Verify that a newly created file is detected and indexed incrementally.
    #[test]
    fn incremental_scan_detects_new_file() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        // First scan.
        index.incremental_scan(dir.path());
        assert_eq!(index.len(), 3);

        // Add a new file.
        let new_path = dir.path().join("NewReport.al");
        fs::write(&new_path, r#"report 50100 "New Report" { }"#).unwrap();

        let delta = index.incremental_scan(dir.path());

        assert_eq!(
            delta.changed.len(),
            1,
            "The new file should appear in changed"
        );
        assert_eq!(delta.removed.len(), 0);
        assert_eq!(index.len(), 4, "Total file count should increase by 1");
        assert!(index.get_content(&new_path).is_some());
    }

    /// F-010: a full `scan()` re-run after a file is deleted from disk
    /// must drop the deleted file from every index — primary `files`,
    /// the object-name map, content cache, and the procedure reverse
    /// index. Previously `scan` only added/updated entries, leaving
    /// deleted AL objects discoverable by go-to-definition.
    #[test]
    fn f010_scan_removes_files_deleted_between_scans() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        index.scan(dir.path());
        assert_eq!(index.len(), 3);
        assert!(index.find_by_object_name("my test page").is_some());

        let page_path = dir.path().join("MyTestPage.al");
        fs::remove_file(&page_path).unwrap();

        let count = index.scan(dir.path());

        assert_eq!(count, 2, "scan() walks only files still on disk");
        assert_eq!(index.len(), 2, "deleted file must be dropped from index");
        assert!(
            index.find_by_object_name("my test page").is_none(),
            "deleted file's object-name entry must be cleared"
        );
        assert!(
            index.get_content(&page_path).is_none(),
            "deleted file's content cache must be cleared"
        );
    }

    /// F-010 negative regression: a scan with no on-disk changes must
    /// not remove anything. Confirms the deletion sweep is gated on
    /// "not on disk this scan", not on "older than this scan".
    #[test]
    fn f010_scan_does_not_remove_files_still_present() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        index.scan(dir.path());
        let initial_len = index.len();
        assert_eq!(initial_len, 3);

        // Re-scan with no changes.
        index.scan(dir.path());
        assert_eq!(
            index.len(),
            initial_len,
            "re-scan with no on-disk changes must not drop entries"
        );
        assert!(index.find_by_object_name("my test page").is_some());
        assert!(index.find_by_object_name("my test table").is_some());
    }

    /// Verify that a deleted file is removed from the index incrementally.
    #[test]
    fn incremental_scan_removes_deleted_file() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        // First scan.
        index.incremental_scan(dir.path());
        assert_eq!(index.len(), 3);

        // Delete one file.
        let page_path = dir.path().join("MyTestPage.al");
        fs::remove_file(&page_path).unwrap();

        let delta = index.incremental_scan(dir.path());

        assert_eq!(delta.changed.len(), 0);
        assert_eq!(
            delta.removed.len(),
            1,
            "Deleted file should appear in removed"
        );
        assert_eq!(delta.removed[0], page_path);
        assert_eq!(index.len(), 2, "Total file count should decrease by 1");
        assert!(
            index.find_by_object_name("my test page").is_none(),
            "Deleted file's object should no longer be in index"
        );
        assert!(index.get_content(&page_path).is_none());
    }

    /// Verify that renaming a file (delete old, create new) is handled correctly.
    #[test]
    fn incremental_scan_handles_file_rename() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        // First scan.
        index.incremental_scan(dir.path());

        let old_path = dir.path().join("MyTestTable.al");
        let new_path = dir.path().join("RenamedTable.al");

        // Simulate rename: the content stays the same but the path changes.
        let content = index.get_content(&old_path).unwrap();
        fs::remove_file(&old_path).unwrap();
        fs::write(&new_path, &content).unwrap();

        let delta = index.incremental_scan(dir.path());

        assert_eq!(delta.removed.len(), 1, "Old path should be removed");
        assert_eq!(delta.changed.len(), 1, "New path should be added");
        assert_eq!(delta.removed[0], old_path);
        assert_eq!(delta.changed[0], new_path);

        // Content accessible under new path.
        assert!(index.get_content(&new_path).is_some());
        // Old path gone.
        assert!(index.get_content(&old_path).is_none());
    }

    /// Verify `ScanDelta::total` and `ScanDelta::is_empty` helpers.
    #[test]
    fn scan_delta_helpers() {
        let mut delta = ScanDelta::default();
        assert!(delta.is_empty());
        assert_eq!(delta.total(), 0);

        delta.changed.push(PathBuf::from("/a.al"));
        assert!(!delta.is_empty());
        assert_eq!(delta.total(), 1);

        delta.removed.push(PathBuf::from("/b.al"));
        assert_eq!(delta.total(), 2);
    }

    // --- Concurrency / stress tests ---

    #[test]
    fn concurrent_add_and_read_files() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(FileIndex::new());

        // Spawn writers
        let mut handles = Vec::new();
        for i in 0..10 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                let path = PathBuf::from(format!("/test/src/CU{i}.al"));
                let content =
                    format!(r#"codeunit 5010{i} "CU{i}" {{ procedure Proc{i}() begin end; }}"#);
                idx.add_file(path, content);
            }));
        }

        // Spawn concurrent readers while writers are running
        for _ in 0..5 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                // These may or may not see partially-written state — should never panic
                let _count = idx.files.len();
                let _obj = idx.objects.get("cu0");
                let _proc = idx.procedures.get("proc0");
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // All 10 files should be indexed
        assert_eq!(index.files.len(), 10);
    }

    #[test]
    fn concurrent_add_and_remove() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(FileIndex::new());

        // Pre-populate
        for i in 0..10 {
            let path = PathBuf::from(format!("/test/src/T{i}.al"));
            let content = format!(
                r#"table 5010{i} "T{i}" {{ fields {{ field(1; "No."; Code[20]) {{ }} }} }}"#
            );
            index.add_file(path, content);
        }
        assert_eq!(index.files.len(), 10);

        // Concurrently remove half and add new ones
        let mut handles = Vec::new();
        for i in 0..5 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                idx.remove_file(&PathBuf::from(format!("/test/src/T{i}.al")));
            }));
        }
        for i in 10..15 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                let path = PathBuf::from(format!("/test/src/T{i}.al"));
                let content = format!(
                    r#"table 5010{i} "T{i}" {{ fields {{ field(1; "No."; Code[20]) {{ }} }} }}"#
                );
                idx.add_file(path, content);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // 5 removed + 5 added = 10 total
        assert_eq!(index.files.len(), 10);
    }

    #[test]
    fn rapid_fire_procedure_index_updates() {
        let index = FileIndex::new();

        // Add and immediately update same file 50 times
        for i in 0..50 {
            let path = PathBuf::from("/test/src/Rapid.al");
            let content =
                format!(r#"codeunit 50100 "Rapid" {{ procedure Version{i}() begin end; }}"#);
            index.add_file(path, content);
        }

        // Only the latest version should remain
        let procs = index.procedures.get("version49");
        assert!(procs.is_some(), "latest procedure should be in index");
        // Earlier versions should have been cleaned up
        let old = index.procedures.get("version0");
        assert!(old.is_none(), "old procedure should have been removed");
    }
}
