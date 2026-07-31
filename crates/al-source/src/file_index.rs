//! Workspace .al file index.
//!
//! Scans a project directory for `.al` files, reads their content, extracts
//! object declarations, and provides fast lookup by object name or file path.
//! This is the single implementation used by both LSP (stdio) and daemon modes.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use dashmap::DashMap;

/// Prevents runaway memory usage if a workspace root accidentally includes a huge directory tree.
pub const MAX_WORKSPACE_FILES: usize = 10_000;

/// Real AL source files are KB-scale; a multi-megabyte `.al` is almost
/// certainly a build artifact, generated blob, or pathological input.
pub const MAX_AL_FILE_BYTES: u64 = 50 * 1024 * 1024; // 50 MiB

/// Bound the complete source snapshot staged before an atomic index refresh.
/// Reaching the cap is an explicit scan error rather than a partial index.
pub const MAX_WORKSPACE_SOURCE_BYTES: u64 = 512 * 1024 * 1024; // 512 MiB

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("failed to read workspace directory '{}': {source}", path.display())]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read an entry in workspace directory '{}': {source}", path.display())]
    ReadDirectoryEntry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect workspace path '{}': {source}", path.display())]
    InspectPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "workspace contains more than {limit} AL files; narrow the workspace root or increase the explicit limit"
    )]
    FileLimit { limit: usize },
    #[error(
        "AL source '{}' is {size} bytes, exceeding the per-file limit of {limit} bytes",
        path.display()
    )]
    FileTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
    #[error(
        "workspace AL source totals at least {size} bytes, exceeding the limit of {limit} bytes"
    )]
    WorkspaceTooLarge { size: u64, limit: u64 },
    #[error("failed to read AL source '{}': {source}", path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("AL source '{}' changed while the workspace snapshot was being read", path.display())]
    ChangedDuringScan { path: PathBuf },
    #[error("workspace source '{}' is not a regular file", path.display())]
    NotRegularFile { path: PathBuf },
}

/// Snapshot of file metadata used for change detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    pub modified: SystemTime,
    pub size: u64,
}

impl FileMetadata {
    pub fn read(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: meta.len(),
        })
    }
}

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
    pub id: Option<i64>,
    /// Object name with original casing.
    pub name: String,
    /// Byte range of the object declaration in the file.
    pub range: tree_sitter::Range,
}

/// One owner of an object name: the declaring file plus the object kind
/// (`"table"`, `"page"`, …). AL object names are unique only *within* a kind,
/// so a name maps to a list of these — a table `Customer` and a page `Customer`
/// are distinct owners that must not collapse onto one another.
#[derive(Debug, Clone)]
pub struct ObjectEntry {
    pub kind: String,
    pub path: PathBuf,
}

/// A procedure/event location cached at index time.
#[derive(Debug, Clone)]
pub struct CachedProcedureInfo {
    pub file: PathBuf,
    /// Selection range of the procedure name (for go-to-definition).
    pub selection_range: al_syntax::types::SyntaxRange,
}

pub struct FileIndex {
    /// File path → full text content.
    /// Public — used by the al-lsp binary for stats and by daemon dispatchers.
    pub files: DashMap<PathBuf, String>,
    /// Lowercase object name → the list of owners (one per object kind) that
    /// declare it. Keyed by name only, but multi-valued and kind-tagged so
    /// same-named objects of different kinds coexist and a removal of one can
    /// never strand another. Invariant-coupled to `path_to_object`; mutate only
    /// via the impl methods. Read via
    /// `object_path` / `object_path_of_kind` / `object_count`.
    pub objects: DashMap<String, Vec<ObjectEntry>>,
    /// File path → lowercase object name (reverse index for O(1) cleanup).
    /// Invariant-coupled to `objects`.
    pub(crate) path_to_object: DashMap<PathBuf, String>,
    /// File path → (mtime, size) snapshot taken at last index time.
    /// Used by `incremental_scan` to detect changed files.
    pub(crate) file_metadata: DashMap<PathBuf, FileMetadata>,
    /// File path → cached object declaration metadata (avoids re-parsing for workspace/symbol).
    /// Public — al-lsp's DAP path needs object_id ↔ file_path lookups.
    pub object_info: DashMap<PathBuf, CachedObjectInfo>,
    /// Parsed files stored as coherent `(text, tree)` pairs. Mutate only via the
    /// implementation methods to keep this cache consistent with `files`.
    pub(crate) file_trees: DashMap<PathBuf, std::sync::Arc<(String, tree_sitter::Tree)>>,
    /// File path → cached document symbols (avoids re-extracting for cross-file queries).
    pub(crate) file_symbols: DashMap<PathBuf, Vec<al_syntax::types::SyntaxDocumentSymbol>>,
    /// Lowercase procedure/event name → location (reverse index for O(1) go-to-definition).
    pub procedures: DashMap<String, Vec<CachedProcedureInfo>>,
    /// File path → list of procedure names (for cleanup on file remove/update).
    path_to_procedures: DashMap<PathBuf, Vec<String>>,
}

/// Deterministic accounting for the text and secondary indexes owned by a
/// [`FileIndex`]. Opaque tree-sitter allocations are intentionally not guessed.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIndexMemoryStats {
    pub source_text_bytes: usize,
    pub index_bytes: usize,
    pub cached_tree_count: usize,
    pub tracked_bytes: usize,
}

impl FileIndex {
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

    /// Replace every primary and secondary index with a completely staged
    /// generation.
    ///
    /// Callers that publish this into a live workspace must hold their
    /// workspace-generation write lock so readers cannot observe the brief
    /// clear-and-move window.
    pub fn replace_with(&self, replacement: FileIndex) {
        self.files.clear();
        self.objects.clear();
        self.path_to_object.clear();
        self.file_metadata.clear();
        self.object_info.clear();
        self.file_trees.clear();
        self.file_symbols.clear();
        self.procedures.clear();
        self.path_to_procedures.clear();

        for (key, value) in replacement.files {
            self.files.insert(key, value);
        }
        for (key, value) in replacement.objects {
            self.objects.insert(key, value);
        }
        for (key, value) in replacement.path_to_object {
            self.path_to_object.insert(key, value);
        }
        for (key, value) in replacement.file_metadata {
            self.file_metadata.insert(key, value);
        }
        for (key, value) in replacement.object_info {
            self.object_info.insert(key, value);
        }
        for (key, value) in replacement.file_trees {
            self.file_trees.insert(key, value);
        }
        for (key, value) in replacement.file_symbols {
            self.file_symbols.insert(key, value);
        }
        for (key, value) in replacement.procedures {
            self.procedures.insert(key, value);
        }
        for (key, value) in replacement.path_to_procedures {
            self.path_to_procedures.insert(key, value);
        }
    }

    pub fn memory_stats(&self) -> FileIndexMemoryStats {
        let source_text_bytes = self
            .files
            .iter()
            .map(|entry| entry.value().capacity())
            .sum();
        // The secondary structures contain paths, names, vectors, and metadata.
        // Count their concrete values/keys without pretending to know DashMap
        // bucket allocation or tree-sitter tree allocation sizes.
        let index_bytes = self
            .objects
            .iter()
            .map(|entry| {
                entry.key().capacity()
                    + entry
                        .value()
                        .iter()
                        .map(|owner| {
                            owner.kind.capacity()
                                + owner.path.as_os_str().len()
                                + std::mem::size_of_val(owner)
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
            + self
                .path_to_object
                .iter()
                .map(|entry| entry.key().as_os_str().len() + entry.value().capacity())
                .sum::<usize>()
            + self
                .file_metadata
                .iter()
                .map(|entry| entry.key().as_os_str().len() + std::mem::size_of_val(entry.value()))
                .sum::<usize>()
            + self
                .object_info
                .iter()
                .map(|entry| {
                    entry.key().as_os_str().len()
                        + entry.value().kind.capacity()
                        + entry.value().name.capacity()
                        + std::mem::size_of_val(entry.value())
                })
                .sum::<usize>()
            + self
                .file_symbols
                .iter()
                .map(|entry| {
                    entry.key().as_os_str().len()
                        + entry.value().capacity()
                            * std::mem::size_of::<al_syntax::types::SyntaxDocumentSymbol>()
                })
                .sum::<usize>()
            + self
                .procedures
                .iter()
                .map(|entry| {
                    entry.key().capacity()
                        + entry.value().len() * std::mem::size_of::<CachedProcedureInfo>()
                })
                .sum::<usize>()
            + self
                .path_to_procedures
                .iter()
                .map(|entry| {
                    entry.key().as_os_str().len()
                        + entry.value().iter().map(String::capacity).sum::<usize>()
                })
                .sum::<usize>();
        FileIndexMemoryStats {
            source_text_bytes,
            index_bytes,
            cached_tree_count: self.file_trees.len(),
            tracked_bytes: source_text_bytes + index_bytes,
        }
    }

    pub fn lookup_procedures(&self, name: &str) -> Option<Vec<CachedProcedureInfo>> {
        self.procedures
            .get(&name.to_lowercase())
            .map(|v| v.value().clone())
    }

    /// Get the cached parse tree and text for a workspace file (not an open document).
    ///
    /// Returns `(text, tree)` from the cache. Background files are always cached
    /// at index time via `add_file_with_meta`, so a miss means the file was never indexed.
    ///
    /// The `(text, tree)` pair is stored under a single `Arc` in `file_trees`,
    /// so one `get` returns a mutually-consistent pair from the same indexing
    /// pass, so a concurrent re-index cannot produce a torn read.
    pub fn get_cached_parse(&self, path: &Path) -> Option<(String, tree_sitter::Tree)> {
        let pair = self.file_trees.get(path)?.value().clone();
        Some((pair.0.clone(), pair.1.clone()))
    }

    /// Returns the symbols extracted at index time.
    pub fn get_cached_symbols(
        &self,
        path: &Path,
    ) -> Option<Vec<al_syntax::types::SyntaxDocumentSymbol>> {
        self.file_symbols
            .get(path)
            .map(|entry| entry.value().clone())
    }

    /// Skips hidden directories, `node_modules`, `.alpackages`, and symlinks.
    /// On a re-scan, indexed files that no longer
    /// exist on disk are removed from every index (primary + secondary
    /// object-name / object-id / procedure maps).
    ///
    /// The refresh is atomic with respect to discovery and file reads: a walk,
    /// size, UTF-8, or concurrent-modification failure leaves the previous
    /// generation intact and returns the precise error.
    pub fn scan(&self, root: &Path) -> Result<usize, ScanError> {
        let paths = collect_al_files(root)?;
        let staged = stage_files(&paths)?;
        let on_disk: std::collections::HashSet<PathBuf> = paths.iter().cloned().collect();

        for (path, content, metadata) in staged {
            self.add_file_with_meta(path, content, Some(metadata));
        }

        // Drop entries for files that disappeared from disk between scans.
        // remove_file already cleans the secondary maps (object-name,
        // object-id, procedures, file_metadata, file_trees, file_symbols).
        let indexed_paths: Vec<PathBuf> = self.files.iter().map(|e| e.key().clone()).collect();
        for path in indexed_paths {
            if !on_disk.contains(&path) {
                self.remove_file(&path);
            }
        }
        Ok(paths.len())
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
    pub fn incremental_scan(&self, root: &Path) -> Result<ScanDelta, ScanError> {
        let mut delta = ScanDelta::default();
        let paths = collect_al_files(root)?;
        let on_disk: std::collections::HashSet<PathBuf> = paths.iter().cloned().collect();
        let metadata = strict_metadata_for_paths(&paths)?;
        let mut staged = Vec::new();

        for (path, current_meta) in &metadata {
            let needs_index = match self.file_metadata.get(path) {
                Some(prev) => *prev != *current_meta,
                None => true, // new file
            };
            if needs_index {
                let content = read_stable_file(path, current_meta)?;
                staged.push((path.clone(), content, current_meta.clone()));
            }
        }

        // Discovery and every changed-file read completed successfully. Only
        // now publish the new generation and evict deleted paths.
        for (path, content, metadata) in staged {
            self.add_file_with_meta(path.clone(), content, Some(metadata));
            delta.changed.push(path);
        }

        let indexed_paths: Vec<PathBuf> = self.files.iter().map(|e| e.key().clone()).collect();
        for path in indexed_paths {
            if !on_disk.contains(&path) {
                self.remove_file(&path);
                delta.removed.push(path);
            }
        }

        Ok(delta)
    }

    /// If the file is already indexed, the old object-name mapping is
    /// removed before the new one is inserted, so no stale entries remain.
    /// Records current mtime+size so `incremental_scan` can skip this file
    /// if it hasn't changed since.
    pub fn add_file(&self, path: PathBuf, content: String) {
        self.add_file_with_meta(path, content, None);
    }

    /// Like [`add_file`] but accepts pre-read metadata to avoid a redundant
    /// `stat()` when the caller already has it (e.g., `incremental_scan`).
    fn add_file_with_meta(&self, path: PathBuf, content: String, meta: Option<FileMetadata>) {
        let meta = meta.or_else(|| FileMetadata::read(&path));
        if let Some(m) = meta {
            self.file_metadata.insert(path.clone(), m);
        }
        // Remove the old object-name mapping for this path (if any), so
        // stale entries don't linger when the object is renamed/replaced.
        if let Some((_, old_obj_name)) = self.path_to_object.remove(&path) {
            self.remove_owned_object_mapping(&old_obj_name, &path);
        }
        self.remove_procedures_for_file(&path);

        let result = al_syntax::AlParser::parse_quick(&content);
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
        if let Some((_, old_obj_name)) = self.path_to_object.remove(&path) {
            self.remove_owned_object_mapping(&old_obj_name, &path);
        }
        self.remove_procedures_for_file(&path);

        self.index_from_result(path, content, &tree);
    }

    /// Snapshot the per-path procedure-name list as it currently stands in
    /// the index. Used to detect topology changes between two consecutive
    /// indexings of the same file: if the post-edit set equals the pre-edit
    /// set, only the call-edge cache needs to be invalidated.
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
    /// All secondary maps must be updated together when this method changes.
    fn index_from_result(&self, path: PathBuf, content: String, tree: &tree_sitter::Tree) {
        // Cache the (text, tree) pair atomically under one Arc so a concurrent
        // reader can never observe a torn text/tree combination.
        self.file_trees.insert(
            path.clone(),
            std::sync::Arc::new((content.clone(), tree.clone())),
        );
        if let Some(obj_info) = al_syntax::find_object_declaration(tree, &content) {
            let obj_name = obj_info.name.to_lowercase();
            let kind = obj_info.kind.clone();
            // Record this owner under its name, replacing any prior entry from
            // this same path (re-index) or of the same kind (redefinition) —
            // owners of *other* kinds are preserved so they never collapse.
            {
                let mut owners = self.objects.entry(obj_name.clone()).or_default();
                owners.retain(|e| e.path != path && !e.kind.eq_ignore_ascii_case(&kind));
                owners.push(ObjectEntry {
                    kind,
                    path: path.clone(),
                });
            }
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
            self.object_info.remove(&path);
        }

        // Index procedure/event names for O(1) go-to-definition.
        // Inline the AlSymbolKind::Function/Event predicate here to avoid an
        // upward dependency from file_index (core infrastructure) into the
        // queries module (higher-level LSP feature code).
        let doc_symbols = al_syntax::extract_document_symbols(tree, &content);
        let mut proc_names = Vec::new();
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    let kind = child.kind;
                    let is_proc = matches!(
                        kind,
                        al_syntax::types::SyntaxSymbolKind::Function
                            | al_syntax::types::SyntaxSymbolKind::Event
                    );
                    if is_proc {
                        let proc_key = child.name.to_lowercase();
                        let info = CachedProcedureInfo {
                            file: path.clone(),
                            selection_range: child.selection_range,
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

        self.file_symbols.insert(path.clone(), doc_symbols);

        self.files.insert(path, content);
    }

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

    /// Drop only the owner declared by `path`, keeping same-named owners of
    /// other kinds. A table `Foo` and a page `Foo` are separate entries
    /// under `objects["foo"]`, so removing the table's file leaves the page
    /// findable. The key is removed only when its last owner is gone.
    fn remove_owned_object_mapping(&self, obj_name: &str, path: &Path) {
        use dashmap::mapref::entry::Entry;
        if let Entry::Occupied(mut occ) = self.objects.entry(obj_name.to_string()) {
            occ.get_mut().retain(|e| e.path != path);
            if occ.get().is_empty() {
                occ.remove();
            }
        }
    }

    /// First owner of `name`, regardless of kind. For callers that don't carry
    /// a kind; kind-aware callers should use [`object_path_of_kind`].
    ///
    /// [`object_path_of_kind`]: Self::object_path_of_kind
    pub fn object_path(&self, name: &str) -> Option<PathBuf> {
        self.objects
            .get(&name.to_lowercase())
            .and_then(|owners| owners.first().map(|e| e.path.clone()))
    }

    /// The owner of `name` whose kind is one of `kinds` (case-insensitive) —
    /// e.g. resolving `Enum Foo` passes `["enum", "enumextension"]` so a
    /// same-named table can never win.
    pub fn object_path_of_kind(&self, name: &str, kinds: &[&str]) -> Option<PathBuf> {
        self.objects.get(&name.to_lowercase()).and_then(|owners| {
            owners
                .iter()
                .find(|e| kinds.iter().any(|k| e.kind.eq_ignore_ascii_case(k)))
                .map(|e| e.path.clone())
        })
    }

    /// Every file that declares an object named `name` (all kinds).
    pub fn object_paths(&self, name: &str) -> Vec<PathBuf> {
        self.objects
            .get(&name.to_lowercase())
            .map(|owners| owners.iter().map(|e| e.path.clone()).collect())
            .unwrap_or_default()
    }

    /// Total number of indexed objects across all names and kinds.
    pub fn object_count(&self) -> usize {
        self.objects.iter().map(|e| e.value().len()).sum()
    }

    fn remove_procedures_for_file(&self, path: &Path) {
        if let Some((_, old_proc_names)) = self.path_to_procedures.remove(path) {
            for proc_name in old_proc_names {
                // Retain and conditional removal must hold one shard lock.
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

    pub fn get_content(&self, path: &Path) -> Option<String> {
        self.files.get(path).map(|r| r.value().clone())
    }

    pub fn find_by_object_name(&self, name: &str) -> Option<PathBuf> {
        self.object_path(name)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Read one on-disk AL source with the same size, UTF-8, file-type, and
/// concurrent-modification guarantees as a workspace scan.
///
/// `Ok(None)` means the path no longer exists. This distinction is used when
/// an editor closes a buffer: a real project file must be restored from disk,
/// while a never-saved transient buffer must be removed from every index.
pub fn read_source_file(path: &Path) -> Result<Option<String>, ScanError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ScanError::InspectPath {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.file_type().is_file() {
        return Err(ScanError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    let expected = FileMetadata {
        modified: metadata
            .modified()
            .map_err(|source| ScanError::InspectPath {
                path: path.to_path_buf(),
                source,
            })?,
        size: metadata.len(),
    };
    if expected.size > MAX_AL_FILE_BYTES {
        return Err(ScanError::FileTooLarge {
            path: path.to_path_buf(),
            size: expected.size,
            limit: MAX_AL_FILE_BYTES,
        });
    }
    read_stable_file(path, &expected).map(Some)
}

impl Default for FileIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Discover the exact AL source set used by [`FileIndex::scan`].
///
/// Returned paths preserve the caller's root identity instead of
/// canonicalizing it. This is important on platforms such as macOS where
/// lexical aliases (for example `/var` and `/private/var`) can name the same
/// directory: consumers must be able to compare discovery results with the
/// paths stored in the index.
pub fn collect_al_files(root: &Path) -> Result<Vec<PathBuf>, ScanError> {
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(directory) = directories.pop() {
        let read_dir =
            std::fs::read_dir(&directory).map_err(|source| ScanError::ReadDirectory {
                path: directory.clone(),
                source,
            })?;
        let mut entries = read_dir
            .map(|result| {
                result.map_err(|source| ScanError::ReadDirectoryEntry {
                    path: directory.clone(),
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_unstable_by_key(std::fs::DirEntry::path);

        let mut child_directories = Vec::new();
        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| ScanError::InspectPath {
                path: path.clone(),
                source,
            })?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.')
                    || name.eq_ignore_ascii_case("node_modules")
                    || name.eq_ignore_ascii_case(".alpackages")
                {
                    continue;
                }
                child_directories.push(path);
            } else if file_type.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("al"))
            {
                files.push(path);
                if files.len() > MAX_WORKSPACE_FILES {
                    return Err(ScanError::FileLimit {
                        limit: MAX_WORKSPACE_FILES,
                    });
                }
            }
        }
        // The stack is LIFO; reverse to retain stable lexical traversal.
        child_directories.reverse();
        directories.extend(child_directories);
    }

    files.sort_unstable();
    Ok(files)
}

fn strict_file_metadata(path: &Path) -> Result<FileMetadata, ScanError> {
    let metadata = std::fs::metadata(path).map_err(|source| ScanError::InspectPath {
        path: path.to_path_buf(),
        source,
    })?;
    let modified = metadata
        .modified()
        .map_err(|source| ScanError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    let size = metadata.len();
    if size > MAX_AL_FILE_BYTES {
        return Err(ScanError::FileTooLarge {
            path: path.to_path_buf(),
            size,
            limit: MAX_AL_FILE_BYTES,
        });
    }
    Ok(FileMetadata { modified, size })
}

fn strict_metadata_for_paths(paths: &[PathBuf]) -> Result<Vec<(PathBuf, FileMetadata)>, ScanError> {
    let mut total = 0u64;
    let mut result = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = strict_file_metadata(path)?;
        total = total
            .checked_add(metadata.size)
            .ok_or(ScanError::WorkspaceTooLarge {
                size: u64::MAX,
                limit: MAX_WORKSPACE_SOURCE_BYTES,
            })?;
        if total > MAX_WORKSPACE_SOURCE_BYTES {
            return Err(ScanError::WorkspaceTooLarge {
                size: total,
                limit: MAX_WORKSPACE_SOURCE_BYTES,
            });
        }
        result.push((path.clone(), metadata));
    }
    Ok(result)
}

fn read_stable_file(path: &Path, expected: &FileMetadata) -> Result<String, ScanError> {
    let content = std::fs::read_to_string(path).map_err(|source| ScanError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let after = strict_file_metadata(path)?;
    if &after != expected || content.len() as u64 != expected.size {
        return Err(ScanError::ChangedDuringScan {
            path: path.to_path_buf(),
        });
    }
    Ok(content)
}

fn stage_files(paths: &[PathBuf]) -> Result<Vec<(PathBuf, String, FileMetadata)>, ScanError> {
    strict_metadata_for_paths(paths)?
        .into_iter()
        .map(|(path, metadata)| {
            let content = read_stable_file(&path, &metadata)?;
            Ok((path, content, metadata))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // ProcedureSource impl + iter_parsed follow (crate extraction)
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();

        let al_content = r#"table 50100 "My Test Table"
{
    fields
    {
        field(1; "Code"; Code[20]) { }
    }
}
"#;
        fs::write(dir.path().join("MyTestTable.al"), al_content).unwrap();

        let page_content = r#"page 50100 "My Test Page"
{
    SourceTable = "My Test Table";
}
"#;
        fs::write(dir.path().join("MyTestPage.al"), page_content).unwrap();

        fs::write(dir.path().join("readme.md"), "# Hello").unwrap();

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

        let hidden = dir.path().join(".hidden");
        fs::create_dir(&hidden).unwrap();
        fs::write(hidden.join("Secret.al"), "table 1 Secret {}").unwrap();

        let packages = dir.path().join(".alpackages");
        fs::create_dir(&packages).unwrap();
        fs::write(packages.join("Dep.al"), "table 2 Dep {}").unwrap();

        dir
    }

    #[test]
    fn scan_finds_al_files() {
        let dir = setup_test_dir();
        let index = FileIndex::new();
        let count = index.scan(dir.path()).unwrap();

        assert_eq!(
            count, 3,
            "Should find 3 .al files (2 root + 1 subdirectory)"
        );
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn strict_file_metadata_enforces_limits_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();

        let small = dir.path().join("small.al");
        fs::write(&small, "codeunit 1 X {}").unwrap();
        assert!(strict_file_metadata(&small).unwrap().size < MAX_AL_FILE_BYTES);

        assert!(matches!(
            strict_file_metadata(&dir.path().join("missing.al")),
            Err(ScanError::InspectPath { .. })
        ));

        // A file just over the cap is rejected. Use a sparse file via set_len
        // so the test stays cheap.
        let big = dir.path().join("big.al");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
        drop(f);
        assert!(matches!(
            strict_file_metadata(&big),
            Err(ScanError::FileTooLarge { .. })
        ));
    }

    #[test]
    fn read_source_file_distinguishes_regular_missing_and_oversized_sources() {
        let dir = tempfile::tempdir().unwrap();

        let regular = dir.path().join("Regular.al");
        fs::write(&regular, "codeunit 50100 Regular {}").unwrap();
        assert_eq!(
            read_source_file(&regular).unwrap().as_deref(),
            Some("codeunit 50100 Regular {}")
        );

        assert_eq!(
            read_source_file(&dir.path().join("Missing.al")).unwrap(),
            None
        );

        let oversized = dir.path().join("Oversized.al");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
        drop(file);
        assert!(matches!(
            read_source_file(&oversized),
            Err(ScanError::FileTooLarge { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn read_source_file_rejects_symlinks_and_directories() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Source.al");
        fs::write(&source, "codeunit 50100 Source {}").unwrap();

        let link = dir.path().join("Linked.al");
        symlink(&source, &link).unwrap();
        assert!(matches!(
            read_source_file(&link),
            Err(ScanError::NotRegularFile { .. })
        ));
        assert!(matches!(
            read_source_file(dir.path()),
            Err(ScanError::NotRegularFile { .. })
        ));
    }

    #[test]
    fn scan_rejects_oversized_al_file_without_publishing_partial_index() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ok.al"), "codeunit 1 Ok {}").unwrap();
        let big = dir.path().join("big.al");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
        drop(f);

        let index = FileIndex::new();
        let error = index
            .scan(dir.path())
            .expect_err("oversized source must fail the complete scan");
        assert!(matches!(error, ScanError::FileTooLarge { .. }));
        assert_eq!(
            index.len(),
            0,
            "a failed scan must publish no partial index"
        );
        assert!(index.get_content(&dir.path().join("ok.al")).is_none());
        assert!(index.get_content(&big).is_none());
    }

    #[test]
    fn scan_rejects_invalid_utf8_without_replacing_previous_generation() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("Existing.al");
        fs::write(&existing, "codeunit 1 Existing {}").unwrap();
        let index = FileIndex::new();
        index.scan(dir.path()).unwrap();

        let invalid = dir.path().join("Invalid.al");
        fs::write(&invalid, [0xff, 0xfe]).unwrap();
        let error = index
            .scan(dir.path())
            .expect_err("non-UTF-8 AL source must fail the complete scan");
        assert!(matches!(error, ScanError::ReadFile { .. }));
        assert_eq!(index.len(), 1);
        assert!(index.get_content(&existing).is_some());
        assert!(index.get_content(&invalid).is_none());
    }

    #[test]
    fn scan_has_no_arbitrary_directory_depth_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let mut nested = dir.path().to_path_buf();
        for index in 0..24 {
            nested.push(format!("level-{index:02}"));
        }
        fs::create_dir_all(&nested).unwrap();
        let deep = nested.join("Deep.al");
        fs::write(&deep, "codeunit 1 Deep {}").unwrap();

        let index = FileIndex::new();
        assert_eq!(index.scan(dir.path()).unwrap(), 1);
        assert!(index.get_content(&deep).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_file_or_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("Outside.al");
        fs::write(&outside_file, "codeunit 1 Outside {}").unwrap();
        symlink(&outside_file, dir.path().join("FileLink.al")).unwrap();
        symlink(outside.path(), dir.path().join("DirectoryLink")).unwrap();
        fs::write(dir.path().join("Inside.al"), "codeunit 2 Inside {}").unwrap();

        let index = FileIndex::new();
        assert_eq!(index.scan(dir.path()).unwrap(), 1);
        assert!(index.find_by_object_name("Inside").is_some());
        assert!(index.find_by_object_name("Outside").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn discovery_preserves_an_aliased_project_root() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let project = parent.path().join("project");
        let alias = parent.path().join("alias");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("Inside.al"), "codeunit 2 Inside {}").unwrap();
        symlink(&project, &alias).unwrap();

        assert_eq!(
            collect_al_files(&alias).unwrap(),
            vec![alias.join("Inside.al")],
            "discovery paths must use the same root identity as the index caller"
        );
    }

    #[test]
    fn incremental_scan_rejects_file_that_grew_oversized_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Grower.al");
        fs::write(&path, "codeunit 50100 \"Grower\" { }").unwrap();

        let index = FileIndex::new();
        let first = index.incremental_scan(dir.path()).unwrap();
        assert_eq!(first.changed.len(), 1, "small file indexed on first scan");
        assert!(index.get_content(&path).is_some());
        assert_eq!(index.len(), 1);

        // Grow the file past the cap (sparse) so the next scan must skip it.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
        drop(f);

        let second = index
            .incremental_scan(dir.path())
            .expect_err("oversized changed file must fail the refresh");
        assert!(matches!(second, ScanError::FileTooLarge { .. }));
        assert!(
            index.get_content(&path).is_some(),
            "failed refresh must retain the complete previous generation"
        );
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn scan_skips_hidden_and_alpackages() {
        let dir = setup_test_dir();
        let index = FileIndex::new();
        index.scan(dir.path()).unwrap();

        assert!(index.find_by_object_name("secret").is_none());
        assert!(index.find_by_object_name("dep").is_none());
    }

    #[test]
    fn scan_indexes_object_names() {
        let dir = setup_test_dir();
        let index = FileIndex::new();
        index.scan(dir.path()).unwrap();

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
        index.scan(dir.path()).unwrap();

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

    #[test]
    fn remove_file_preserves_other_owners_object_mapping() {
        let index = FileIndex::new();
        let table_path = PathBuf::from("/tmp/test/TableFoo.al");
        let page_path = PathBuf::from("/tmp/test/PageFoo.al");

        index.add_file(
            table_path.clone(),
            r#"table 50100 "Foo" { fields { } }"#.to_string(),
        );
        index.add_file(
            page_path.clone(),
            r#"page 50100 "Foo" { layout { } actions { } }"#.to_string(),
        );
        assert_eq!(index.object_count(), 2);

        index.remove_file(&table_path);

        let resolved = index.find_by_object_name("foo");
        assert_eq!(
            resolved.as_deref(),
            Some(page_path.as_path()),
            "surviving owner's object mapping was dropped"
        );
    }

    #[test]
    fn same_named_objects_are_kind_addressable() {
        for page_first in [false, true] {
            let index = FileIndex::new();
            let table_path = PathBuf::from("/tmp/c22/Foo.Table.al");
            let page_path = PathBuf::from("/tmp/c22/Foo.Page.al");
            let add_table = || {
                index.add_file(
                    table_path.clone(),
                    r#"table 50100 "Foo" { fields { } }"#.to_string(),
                )
            };
            let add_page = || {
                index.add_file(
                    page_path.clone(),
                    r#"page 50100 "Foo" { layout { } actions { } }"#.to_string(),
                )
            };
            if page_first {
                add_page();
                add_table();
            } else {
                add_table();
                add_page();
            }

            assert_eq!(
                index.object_path_of_kind("foo", &["table"]).as_deref(),
                Some(table_path.as_path()),
                "kind=table must resolve to the table (page_first={page_first})"
            );
            assert_eq!(
                index.object_path_of_kind("foo", &["page"]).as_deref(),
                Some(page_path.as_path()),
                "kind=page must resolve to the page (page_first={page_first})"
            );

            index.remove_file(&page_path);
            assert_eq!(
                index.object_path_of_kind("foo", &["table"]).as_deref(),
                Some(table_path.as_path()),
                "table must survive page deletion (page_first={page_first})"
            );
            assert!(index.object_path_of_kind("foo", &["page"]).is_none());
        }
    }

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

    /// three objects (table, page, codeunit) sharing a name all coexist
    /// and are independently kind-addressable.
    #[test]
    fn three_kinds_same_name_coexist() {
        let index = FileIndex::new();
        let t = PathBuf::from("/c22/Foo.Table.al");
        let p = PathBuf::from("/c22/Foo.Page.al");
        let c = PathBuf::from("/c22/Foo.Codeunit.al");
        index.add_file(t.clone(), r#"table 50100 "Foo" { fields { } }"#.to_string());
        index.add_file(
            p.clone(),
            r#"page 50100 "Foo" { layout { } actions { } }"#.to_string(),
        );
        index.add_file(c.clone(), r#"codeunit 50100 "Foo" { }"#.to_string());

        assert_eq!(index.object_count(), 3);
        assert_eq!(index.object_paths("foo").len(), 3);
        assert_eq!(
            index.object_path_of_kind("foo", &["table"]).as_deref(),
            Some(t.as_path())
        );
        assert_eq!(
            index.object_path_of_kind("foo", &["page"]).as_deref(),
            Some(p.as_path())
        );
        assert_eq!(
            index.object_path_of_kind("foo", &["codeunit"]).as_deref(),
            Some(c.as_path())
        );
        assert!(index.object_path_of_kind("foo", &["enum"]).is_none());
    }

    #[test]
    fn reindex_object_rename_clears_old_name() {
        let index = FileIndex::new();
        let path = PathBuf::from("/c22/Renamed.al");
        index.add_file(
            path.clone(),
            r#"table 50100 "OldName" { fields { } }"#.to_string(),
        );
        assert!(index.find_by_object_name("oldname").is_some());

        index.add_file(
            path.clone(),
            r#"table 50100 "NewName" { fields { } }"#.to_string(),
        );
        assert!(
            index.find_by_object_name("oldname").is_none(),
            "old object name must not linger after a rename re-index"
        );
        assert_eq!(
            index.find_by_object_name("newname").as_deref(),
            Some(path.as_path())
        );
        assert_eq!(index.object_count(), 1, "no stale duplicate owner");
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
        let count = index.scan(dir.path()).unwrap();
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

    #[test]
    fn incremental_scan_first_call_behaves_like_full_scan() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        let delta = index.incremental_scan(dir.path()).unwrap();

        assert_eq!(
            delta.changed.len(),
            3,
            "All 3 files should be indexed on first call"
        );
        assert_eq!(delta.removed.len(), 0);
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn incremental_scan_no_changes_produces_empty_delta() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        index.incremental_scan(dir.path()).unwrap();

        let delta = index.incremental_scan(dir.path()).unwrap();

        assert!(
            delta.is_empty(),
            "No files should be re-indexed when nothing changed"
        );
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn incremental_scan_only_reparses_changed_file() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        index.incremental_scan(dir.path()).unwrap();
        assert_eq!(index.len(), 3);

        let table_path = dir.path().join("MyTestTable.al");
        let new_content = r#"table 50101 "My Modified Table"
{
    fields
    {
        field(1; "Code"; Code[20]) { }
    }
}
"#;

        fs::write(&table_path, new_content).unwrap();

        // The size change makes the metadata snapshot differ even on coarse filesystems.

        let delta = index.incremental_scan(dir.path()).unwrap();

        assert_eq!(
            delta.changed.len(),
            1,
            "Only 1 file should be re-indexed after a single modification"
        );
        assert_eq!(delta.removed.len(), 0);
        assert_eq!(delta.changed[0], table_path);

        assert!(
            index.find_by_object_name("my modified table").is_some(),
            "New object name should be findable after incremental re-index"
        );
        assert!(
            index.find_by_object_name("my test table").is_none(),
            "Old object name should be removed after file is re-indexed"
        );
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn incremental_scan_detects_new_file() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        index.incremental_scan(dir.path()).unwrap();
        assert_eq!(index.len(), 3);

        let new_path = dir.path().join("NewReport.al");
        fs::write(&new_path, r#"report 50100 "New Report" { }"#).unwrap();

        let delta = index.incremental_scan(dir.path()).unwrap();

        assert_eq!(
            delta.changed.len(),
            1,
            "The new file should appear in changed"
        );
        assert_eq!(delta.removed.len(), 0);
        assert_eq!(index.len(), 4, "Total file count should increase by 1");
        assert!(index.get_content(&new_path).is_some());
    }

    #[test]
    fn scan_removes_files_deleted_between_scans() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        index.scan(dir.path()).unwrap();
        assert_eq!(index.len(), 3);
        assert!(index.find_by_object_name("my test page").is_some());

        let page_path = dir.path().join("MyTestPage.al");
        fs::remove_file(&page_path).unwrap();

        let count = index.scan(dir.path()).unwrap();

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

    #[test]
    fn scan_preserves_files_still_present() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        index.scan(dir.path()).unwrap();
        let initial_len = index.len();
        assert_eq!(initial_len, 3);

        index.scan(dir.path()).unwrap();
        assert_eq!(
            index.len(),
            initial_len,
            "re-scan with no on-disk changes must not drop entries"
        );
        assert!(index.find_by_object_name("my test page").is_some());
        assert!(index.find_by_object_name("my test table").is_some());
    }

    #[test]
    fn incremental_scan_removes_deleted_file() {
        let dir = setup_test_dir();
        let index = FileIndex::new();

        index.incremental_scan(dir.path()).unwrap();
        assert_eq!(index.len(), 3);

        let page_path = dir.path().join("MyTestPage.al");
        fs::remove_file(&page_path).unwrap();

        let delta = index.incremental_scan(dir.path()).unwrap();

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

        index.incremental_scan(dir.path()).unwrap();

        let old_path = dir.path().join("MyTestTable.al");
        let new_path = dir.path().join("RenamedTable.al");

        let content = index.get_content(&old_path).unwrap();
        fs::remove_file(&old_path).unwrap();
        fs::write(&new_path, &content).unwrap();

        let delta = index.incremental_scan(dir.path()).unwrap();

        assert_eq!(delta.removed.len(), 1, "Old path should be removed");
        assert_eq!(delta.changed.len(), 1, "New path should be added");
        assert_eq!(delta.removed[0], old_path);
        assert_eq!(delta.changed[0], new_path);

        assert!(index.get_content(&new_path).is_some());
        assert!(index.get_content(&old_path).is_none());
    }

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

    #[test]
    fn concurrent_add_and_read_files() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(FileIndex::new());

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

        for _ in 0..5 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                // These may or may not see partially-written state — should never panic
                let _count = idx.files.len();
                let _obj = idx.object_path("cu0");
                let _proc = idx.procedures.get("proc0");
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(index.files.len(), 10);
    }

    #[test]
    fn concurrent_add_and_remove() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(FileIndex::new());

        for i in 0..10 {
            let path = PathBuf::from(format!("/test/src/T{i}.al"));
            let content = format!(
                r#"table 5010{i} "T{i}" {{ fields {{ field(1; "No."; Code[20]) {{ }} }} }}"#
            );
            index.add_file(path, content);
        }
        assert_eq!(index.files.len(), 10);

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

        for i in 0..50 {
            let path = PathBuf::from("/test/src/Rapid.al");
            let content =
                format!(r#"codeunit 50100 "Rapid" {{ procedure Version{i}() begin end; }}"#);
            index.add_file(path, content);
        }

        let procs = index.procedures.get("version49");
        assert!(procs.is_some(), "latest procedure should be in index");
        let old = index.procedures.get("version0");
        assert!(old.is_none(), "old procedure should have been removed");
    }

    #[test]
    fn get_cached_parse_pair_is_always_coherent() {
        // Sanity: a freshly indexed file returns a (text, tree) pair where the
        // tree spans exactly the returned text. This is the invariant that
        // get_cached_parse relies on to detect a torn text/tree race.
        let index = FileIndex::new();
        let path = PathBuf::from("/test/src/Coherent.al");
        let content = r#"codeunit 50100 "Coherent" { procedure P() begin end; }"#.to_string();
        index.add_file(path.clone(), content.clone());

        let (text, tree) = index.get_cached_parse(&path).expect("indexed");
        assert_eq!(text, content);
        assert_eq!(tree.root_node().end_byte(), text.len());
    }

    #[test]
    fn equal_length_reindex_returns_coherent_new_pair() {
        let index = FileIndex::new();
        let path = PathBuf::from("/test/src/Eq.al");
        let a = r#"codeunit 50100 "Eq" { procedure Aaa() begin end; }"#.to_string();
        let b = r#"codeunit 50100 "Eq" { procedure Bbb() begin end; }"#.to_string();
        assert_eq!(a.len(), b.len(), "test contents must be equal length");

        index.add_file(path.clone(), a.clone());
        let (t1, _) = index.get_cached_parse(&path).unwrap();
        assert_eq!(t1, a);

        index.add_file(path.clone(), b.clone());
        let (t2, tree2) = index.get_cached_parse(&path).unwrap();
        assert_eq!(t2, b, "must return the re-indexed content, not stale text");
        // The tree came from the same Arc as the text, so its span matches.
        assert_eq!(tree2.root_node().end_byte(), t2.len());
    }

    #[test]
    fn replace_with_removes_old_entries_and_moves_all_secondary_indexes() {
        let active = FileIndex::new();
        let old = PathBuf::from("/old/Old.al");
        active.add_file(
            old.clone(),
            r#"codeunit 50100 Old { procedure OldProcedure() begin end; }"#.to_string(),
        );

        let staged = FileIndex::new();
        let new = PathBuf::from("/new/New.al");
        staged.add_file(
            new.clone(),
            r#"codeunit 50101 New { procedure NewProcedure() begin end; }"#.to_string(),
        );

        active.replace_with(staged);

        assert!(active.get_content(&old).is_none());
        assert!(active.find_by_object_name("Old").is_none());
        assert!(active.lookup_procedures("OldProcedure").is_none());
        assert!(active.get_content(&new).is_some());
        assert_eq!(
            active.find_by_object_name("New").as_deref(),
            Some(new.as_path())
        );
        assert_eq!(
            active
                .lookup_procedures("NewProcedure")
                .expect("procedure secondary index")
                .len(),
            1
        );
        assert!(active.get_cached_parse(&new).is_some());
        assert!(active.get_cached_symbols(&new).is_some());
    }

    #[test]
    fn get_cached_parse_never_returns_torn_pair_under_concurrency() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(FileIndex::new());
        let path = PathBuf::from("/test/src/Race.al");

        index.add_file(
            path.clone(),
            r#"codeunit 50100 "Race" { procedure A() begin end; }"#.to_string(),
        );

        let mut handles = Vec::new();

        // Writer: continually re-index the same path with content of varying
        // length so a torn (old_text, new_tree) pair would fail the
        // end_byte()==len() invariant.
        for w in 0..4 {
            let idx = Arc::clone(&index);
            let p = path.clone();
            handles.push(thread::spawn(move || {
                for i in 0..200 {
                    let pad = "X".repeat((w * 200 + i) % 97);
                    let content = format!(
                        r#"codeunit 50100 "Race" {{ procedure A{i}() begin Message('{pad}'); end; }}"#
                    );
                    idx.add_file(p.clone(), content);
                }
            }));
        }

        for _ in 0..4 {
            let idx = Arc::clone(&index);
            let p = path.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..400 {
                    if let Some((text, tree)) = idx.get_cached_parse(&p) {
                        assert_eq!(
                            tree.root_node().end_byte(),
                            text.len(),
                            "get_cached_parse returned a torn text/tree pair"
                        );
                    }
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
    }
}

/// Production `ProcedureSource` for the AL interpreter (al-runtime).
///
/// The interpreter (tier 1) reaches procedures through this seam rather than
/// naming the workspace hub. `al-runtime` defines the trait in `al-types`;
/// this is its real implementation over the file index.
impl al_types::ProcedureSource for FileIndex {
    fn find_by_object_name(&self, name: &str) -> Option<std::path::PathBuf> {
        FileIndex::find_by_object_name(self, name)
    }
    fn iter_paths(&self) -> Vec<std::path::PathBuf> {
        self.files.iter().map(|e| e.key().clone()).collect()
    }
    fn get_cached_parse(&self, p: &std::path::Path) -> Option<(String, tree_sitter::Tree)> {
        FileIndex::get_cached_parse(self, p)
    }
    fn object_name(&self, p: &std::path::Path) -> Option<String> {
        self.object_info.get(p).map(|i| i.name.clone())
    }
}

impl FileIndex {
    /// All indexed files as (path, source text, parse tree) — for
    /// whole-workspace passes such as dead-code analysis. Replaces direct
    /// access to the private `file_trees`/`files` maps from other crates.
    pub fn iter_parsed(&self) -> Vec<(std::path::PathBuf, String, tree_sitter::Tree)> {
        self.file_trees
            .iter()
            .map(|entry| {
                let path = entry.key().clone();
                let pair = entry.value();
                (path, pair.0.clone(), pair.1.clone())
            })
            .collect()
    }
}
