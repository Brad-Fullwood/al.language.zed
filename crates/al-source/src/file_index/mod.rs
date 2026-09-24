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
    /// Whether an object was added, removed or renamed, or a file's procedure
    /// set changed. A body-only edit leaves the object and call topology alone.
    pub topology_changed: bool,
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

/// One owner of an object name: the declaring file, the object kind
/// (`"table"`, `"page"`, …) and the app the file belongs to. AL object names
/// are unique only within one kind *of one extension*, so a name maps to a
/// list of these: a table `Customer` and a page `Customer` are distinct
/// owners, and so are `codeunit "Install"` in an app and in its test app.
#[derive(Debug, Clone)]
pub struct ObjectEntry {
    pub kind: String,
    pub path: PathBuf,
    /// Directory of the `app.json` above `path`, when there is one.
    pub app_root: Option<PathBuf>,
}

/// The `app.json` facts the index needs to rank owners of one object name.
#[derive(Debug, Clone, Default)]
struct AppIdentity {
    id: String,
    dependency_ids: Vec<String>,
}

impl AppIdentity {
    fn read(app_json: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(app_json) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_lowercase();
        let dependency_ids = value
            .get("dependencies")
            .and_then(|v| v.as_array())
            .map(|deps| {
                deps.iter()
                    .filter_map(|dep| {
                        dep.get("id")
                            .or_else(|| dep.get("appId"))
                            .and_then(|v| v.as_str())
                            .map(str::to_lowercase)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { id, dependency_ids }
    }
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
    /// File path → lowercase object names declared in the file (reverse
    /// index for O(1) cleanup). AL legally allows multiple objects per file.
    /// Invariant-coupled to `objects`.
    pub(crate) path_to_object: DashMap<PathBuf, Vec<String>>,
    /// File path → (mtime, size) snapshot taken at last index time.
    /// Used by `incremental_scan` to detect changed files.
    pub(crate) file_metadata: DashMap<PathBuf, FileMetadata>,
    /// Files whose recorded mtime was within [`RACY_WINDOW`] of the moment
    /// they were read. A rewrite of the same length inside the same timestamp
    /// tick leaves `(mtime, size)` unchanged, so `incremental_scan` re-reads
    /// these instead of trusting the metadata (git's "racy clean" rule).
    racy: dashmap::DashSet<PathBuf>,
    /// File path → cached metadata of the *first* object declaration
    /// (avoids re-parsing for workspace/symbol).
    /// Public — al-lsp's DAP path needs object_id ↔ file_path lookups.
    /// Files can declare several objects; see [`Self::object_infos`] for all
    /// of them.
    pub object_info: DashMap<PathBuf, CachedObjectInfo>,
    /// File path → cached metadata for *every* object declared in the file,
    /// in document order. `object_info` holds the first entry of this list.
    pub object_infos: DashMap<PathBuf, Vec<CachedObjectInfo>>,
    /// Parsed files stored as coherent `(text, tree)` pairs. Mutate only via the
    /// implementation methods to keep this cache consistent with `files`.
    pub(crate) file_trees: DashMap<PathBuf, std::sync::Arc<(String, tree_sitter::Tree)>>,
    /// File path → cached document symbols (avoids re-extracting for cross-file queries).
    pub(crate) file_symbols: DashMap<PathBuf, Vec<al_syntax::types::SyntaxDocumentSymbol>>,
    /// Lowercase procedure/event name → location (reverse index for O(1) go-to-definition).
    pub procedures: DashMap<String, Vec<CachedProcedureInfo>>,
    /// File path → list of procedure names (for cleanup on file remove/update).
    path_to_procedures: DashMap<PathBuf, Vec<String>>,
    /// Directory → the app root above it (`None` when the directory is outside
    /// any app). Memoizes the ancestor walk done once per indexed directory.
    dir_app_root: DashMap<PathBuf, Option<PathBuf>>,
    /// App root → the identity read from its `app.json`.
    apps: DashMap<PathBuf, AppIdentity>,
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
            racy: dashmap::DashSet::new(),
            object_info: DashMap::new(),
            object_infos: DashMap::new(),
            file_trees: DashMap::new(),
            file_symbols: DashMap::new(),
            procedures: DashMap::new(),
            path_to_procedures: DashMap::new(),
            dir_app_root: DashMap::new(),
            apps: DashMap::new(),
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
        self.racy.clear();
        self.object_info.clear();
        self.object_infos.clear();
        self.file_trees.clear();
        self.file_symbols.clear();
        self.procedures.clear();
        self.path_to_procedures.clear();
        self.dir_app_root.clear();
        self.apps.clear();

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
        for key in replacement.racy {
            self.racy.insert(key);
        }
        for (key, value) in replacement.object_info {
            self.object_info.insert(key, value);
        }
        for (key, value) in replacement.object_infos {
            self.object_infos.insert(key, value);
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
        for (key, value) in replacement.dir_app_root {
            self.dir_app_root.insert(key, value);
        }
        for (key, value) in replacement.apps {
            self.apps.insert(key, value);
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
                .object_infos
                .iter()
                .map(|entry| {
                    entry.key().as_os_str().len()
                        + entry
                            .value()
                            .iter()
                            .map(|info| {
                                info.kind.capacity()
                                    + info.name.capacity()
                                    + std::mem::size_of_val(info)
                            })
                            .sum::<usize>()
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
    /// Compared to [`Self::scan`], this method:
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
            let unchanged = self
                .file_metadata
                .get(path)
                .is_some_and(|prev| *prev == *current_meta);
            if unchanged && !self.racy.contains(path) {
                continue;
            }
            let content = read_stable_file(path, current_meta)?;
            if unchanged
                && self
                    .files
                    .get(path)
                    .is_some_and(|indexed| *indexed.value() == content)
            {
                // Racy but the same text: settled once the tick has passed.
                if !is_racy(current_meta, SystemTime::now()) {
                    self.racy.remove(path);
                }
                continue;
            }
            staged.push((path.clone(), content, current_meta.clone()));
        }

        // Discovery and every changed-file read completed successfully. Only
        // now publish the new generation and evict deleted paths.
        for (path, content, metadata) in staged {
            let before = self.topology_of(&path);
            self.add_file_with_meta(path.clone(), content, Some(metadata));
            let after = self.topology_of(&path);
            delta.topology_changed |= before != after;
            delta.changed.push(path);
        }

        // Only a file this index read from disk (it has recorded metadata) and
        // that is gone now is a deletion. An entry added in memory never had
        // a file, and one outside `root` was not this scan's to find.
        let deleted: Vec<PathBuf> = self
            .file_metadata
            .iter()
            .map(|entry| entry.key().clone())
            .filter(|path| path.starts_with(root) && !on_disk.contains(path))
            .collect();
        for path in deleted {
            self.remove_file(&path);
            delta.topology_changed = true;
            delta.removed.push(path);
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
            if is_racy(&m, SystemTime::now()) {
                self.racy.insert(path.clone());
            } else {
                self.racy.remove(&path);
            }
            self.file_metadata.insert(path.clone(), m);
        }
        let result = al_syntax::AlParser::parse_quick(&content);
        self.replace_file_entries(path, content, &result.tree);
    }

    /// Add a file to the index using a pre-parsed tree, skipping the internal parse.
    ///
    /// Used by `on_document_change` in `al_workspace` to avoid a double-parse:
    /// the caller parses once to warm the document cache, then passes the same tree here.
    ///
    /// Mappings the file no longer declares are removed.
    pub fn add_file_with_tree(&self, path: PathBuf, content: String, tree: tree_sitter::Tree) {
        self.replace_file_entries(path, content, &tree);
    }

    /// Re-index `path`, replacing its entries rather than removing them first.
    ///
    /// Each object and procedure name the file still declares has its owner
    /// list swapped under one map-entry lock, and only the names it no longer
    /// declares are removed afterwards. Removing everything first left a
    /// window in which a request on another daemon connection found the
    /// file's objects missing while a refresh re-indexed it.
    fn replace_file_entries(&self, path: PathBuf, content: String, tree: &tree_sitter::Tree) {
        let old_objects = self
            .path_to_object
            .get(&path)
            .map(|names| names.value().clone())
            .unwrap_or_default();
        let old_procedures = self.procedures_snapshot(&path);
        self.index_from_result(path.clone(), content, tree);
        let new_objects = self
            .path_to_object
            .get(&path)
            .map(|names| names.value().clone())
            .unwrap_or_default();
        for name in old_objects
            .iter()
            .filter(|name| !new_objects.contains(name))
        {
            self.remove_owned_object_mapping(name, &path);
        }
        let new_procedures = self.procedures_snapshot(&path);
        for name in old_procedures
            .iter()
            .filter(|name| !new_procedures.contains(name))
        {
            self.remove_owned_procedure(name, &path);
        }
    }

    /// Snapshot the per-path procedure-name list as it currently stands in
    /// the index. Used to detect topology changes between two consecutive
    /// indexings of the same file: if the post-edit set equals the pre-edit
    /// set, only the call-edge cache needs to be invalidated.
    /// The objects `path` declares, as `(kind, id, name)` in lowercase, and
    /// its procedure names: what a change must alter before cross-file graphs
    /// built from this file are out of date.
    #[allow(clippy::type_complexity)]
    fn topology_of(&self, path: &Path) -> (Vec<(String, Option<i64>, String)>, Vec<String>) {
        let objects = self
            .object_infos
            .get(path)
            .map(|infos| {
                infos
                    .iter()
                    .map(|info| {
                        (
                            info.kind.to_ascii_lowercase(),
                            info.id,
                            info.name.to_lowercase(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut procedures = self.procedures_snapshot(path);
        procedures.sort();
        (objects, procedures)
    }

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
        let infos = collect_object_declarations(tree, &content);
        if infos.is_empty() {
            self.object_info.remove(&path);
            self.object_infos.remove(&path);
            self.path_to_object.remove(&path);
        } else {
            let app_root = self.app_root_for(&path);
            // This file's owners per name, swapped in under one entry lock
            // each: same-named owners in other files (a second app in the
            // same workspace root) survive, and a reader never sees the name
            // without this file's current owner.
            let mut by_name: Vec<(String, Vec<ObjectEntry>)> = Vec::new();
            for info in &infos {
                if info.name.is_empty() {
                    continue;
                }
                let obj_name = info.name.to_lowercase();
                let entry = ObjectEntry {
                    kind: info.kind.clone(),
                    path: path.clone(),
                    app_root: app_root.clone(),
                };
                match by_name.iter_mut().find(|(name, _)| *name == obj_name) {
                    Some((_, entries)) => entries.push(entry),
                    None => by_name.push((obj_name, vec![entry])),
                }
            }
            let mut declared_names = Vec::with_capacity(by_name.len());
            for (obj_name, entries) in by_name {
                let mut owners = self.objects.entry(obj_name.clone()).or_default();
                owners.retain(|e| e.path != path);
                owners.extend(entries);
                drop(owners);
                declared_names.push(obj_name);
            }
            self.path_to_object.insert(path.clone(), declared_names);
            // `object_info` keeps its historical "the file's object" meaning:
            // the first declaration in document order.
            self.object_info
                .insert(path.clone(), infos.first().expect("non-empty").clone());
            self.object_infos.insert(path.clone(), infos);
        }

        // Index procedure/event names for O(1) go-to-definition.
        // Inline the AlSymbolKind::Function/Event predicate here to avoid an
        // upward dependency from file_index (core infrastructure) into the
        // queries module (higher-level LSP feature code).
        let doc_symbols = al_syntax::extract_document_symbols(tree, &content);
        let mut by_name: Vec<(String, Vec<CachedProcedureInfo>)> = Vec::new();
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
                        match by_name.iter_mut().find(|(name, _)| *name == proc_key) {
                            Some((_, infos)) => infos.push(info),
                            None => by_name.push((proc_key, vec![info])),
                        }
                    }
                }
            }
        }
        // Swapped per name like the objects above.
        let mut proc_names = Vec::with_capacity(by_name.len());
        for (proc_key, infos) in by_name {
            let mut entries = self.procedures.entry(proc_key.clone()).or_default();
            entries.retain(|e| e.file != path);
            entries.extend(infos);
            drop(entries);
            proc_names.push(proc_key);
        }
        if proc_names.is_empty() {
            self.path_to_procedures.remove(&path);
        } else {
            self.path_to_procedures.insert(path.clone(), proc_names);
        }

        self.file_symbols.insert(path.clone(), doc_symbols);

        self.files.insert(path, content);
    }

    pub fn remove_file(&self, path: &Path) {
        self.files.remove(path);
        self.file_metadata.remove(path);
        self.racy.remove(path);
        self.file_trees.remove(path);
        self.file_symbols.remove(path);
        self.object_info.remove(path);
        self.object_infos.remove(path);
        self.remove_procedures_for_file(path);
        if let Some((_, obj_names)) = self.path_to_object.remove(path) {
            for obj_name in obj_names {
                self.remove_owned_object_mapping(&obj_name, path);
            }
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

    /// Every object declared in `path`, in document order.
    pub fn object_infos_in(&self, path: &Path) -> Vec<CachedObjectInfo> {
        self.object_infos
            .get(path)
            .map(|infos| infos.value().clone())
            .or_else(|| self.object_info.get(path).map(|info| vec![info.clone()]))
            .unwrap_or_default()
    }

    /// The declaration of `name` in `path`, of `kind` when one is given.
    ///
    /// A file declaring `table 50100 "Shipment Header"` then
    /// `table 50101 "Shipment Line"` answers for either, where
    /// [`Self::object_info`] only ever describes the header.
    pub fn object_info_named(
        &self,
        path: &Path,
        name: &str,
        kind: Option<&str>,
    ) -> Option<CachedObjectInfo> {
        self.object_infos_in(path).into_iter().find(|info| {
            info.name.eq_ignore_ascii_case(name)
                && kind.is_none_or(|expected| info.kind.eq_ignore_ascii_case(expected))
        })
    }

    /// The object declaration whose source range covers `byte_offset`.
    pub fn object_info_at_byte(&self, path: &Path, byte_offset: usize) -> Option<CachedObjectInfo> {
        self.object_infos_in(path)
            .into_iter()
            .find(|info| byte_offset >= info.range.start_byte && byte_offset < info.range.end_byte)
    }

    /// The app root of `file`: the nearest ancestor directory holding an
    /// `app.json`. Memoized per directory.
    pub fn app_root_for(&self, file: &Path) -> Option<PathBuf> {
        let start = file.parent()?;
        if let Some(cached) = self.dir_app_root.get(start) {
            return cached.value().clone();
        }
        let mut visited = Vec::new();
        let mut found = None;
        for dir in start.ancestors() {
            if let Some(cached) = self.dir_app_root.get(dir) {
                found = cached.value().clone();
                break;
            }
            visited.push(dir.to_path_buf());
            if dir.join("app.json").is_file() {
                found = Some(dir.to_path_buf());
                break;
            }
        }
        for dir in visited {
            self.dir_app_root.insert(dir, found.clone());
        }
        found
    }

    fn app_identity(&self, app_root: &Path) -> AppIdentity {
        if let Some(cached) = self.apps.get(app_root) {
            return cached.value().clone();
        }
        let identity = AppIdentity::read(&app_root.join("app.json"));
        self.apps.insert(app_root.to_path_buf(), identity.clone());
        identity
    }

    /// Rank an owner against the app that `from` belongs to: 0 for the same
    /// app, 1 for an app the referring app depends on, 2 for anything else.
    fn owner_rank(&self, owner: &ObjectEntry, from_app: Option<&PathBuf>) -> u8 {
        let Some(from_app) = from_app else {
            return 2;
        };
        let Some(owner_app) = owner.app_root.as_ref() else {
            return 2;
        };
        if owner_app == from_app {
            return 0;
        }
        let owner_id = self.app_identity(owner_app).id;
        if !owner_id.is_empty()
            && self
                .app_identity(from_app)
                .dependency_ids
                .contains(&owner_id)
        {
            return 1;
        }
        2
    }

    /// Like [`object_path`], but resolved from the perspective of `from`: an
    /// owner in the same app wins, then one in an app that app depends on.
    ///
    /// [`object_path`]: Self::object_path
    pub fn object_path_near(&self, name: &str, from: &Path) -> Option<PathBuf> {
        self.object_path_where(name, Some(from), |_| true)
    }

    /// The owner of `name` whose kind satisfies `kind_matches`, with the app
    /// preference of [`object_path_near`] when the referring file is known.
    ///
    /// [`object_path_near`]: Self::object_path_near
    pub fn object_path_where(
        &self,
        name: &str,
        from: Option<&Path>,
        kind_matches: impl Fn(&str) -> bool,
    ) -> Option<PathBuf> {
        let from_app = from.and_then(|from| self.app_root_for(from));
        let owners = self.objects.get(&name.to_lowercase())?;
        owners
            .iter()
            .filter(|e| kind_matches(&e.kind))
            .min_by_key(|e| self.owner_rank(e, from_app.as_ref()))
            .map(|e| e.path.clone())
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
                self.remove_owned_procedure(&proc_name, path);
            }
        }
    }

    /// Drop `path`'s entries for one procedure name, and the name when no
    /// file declares it any more.
    fn remove_owned_procedure(&self, proc_name: &str, path: &Path) {
        // Retain and conditional removal must hold one shard lock.
        use dashmap::mapref::entry::Entry;
        if let Entry::Occupied(mut occ) = self.procedures.entry(proc_name.to_string()) {
            occ.get_mut().retain(|e| e.file != path);
            if occ.get().is_empty() {
                occ.remove();
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

/// Extract cached metadata for *every* top-level object declaration in a
/// parsed AL file, in document order.
///
/// AL legally allows multiple objects per `.al` file;
/// `al_syntax::find_object_declaration` returns only the first, which made
/// all subsequent objects invisible to name lookup. Falls back to the
/// single-object helper for grammar variants that expose the object type
/// directly at the root.
pub fn collect_object_declarations(
    tree: &tree_sitter::Tree,
    content: &str,
) -> Vec<CachedObjectInfo> {
    al_syntax::find_object_declarations(tree, content)
        .into_iter()
        .map(|info| CachedObjectInfo {
            kind: info.kind,
            id: info.id,
            name: info.name,
            range: info.range,
        })
        .collect()
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

/// How close to the moment a file is read its mtime has to be for a later
/// same-length rewrite to be able to share it. Covers coarse filesystem clocks
/// (HFS+, exFAT, SMB: one to two seconds).
const RACY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a file read at `read_at` with this metadata could be rewritten
/// without its `(mtime, size)` changing.
fn is_racy(metadata: &FileMetadata, read_at: SystemTime) -> bool {
    read_at
        .duration_since(metadata.modified)
        .map_or(true, |age| age < RACY_WINDOW)
}

/// A directory [`collect_al_files`] does not descend into.
fn is_skipped_directory(name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case("node_modules")
        || name.eq_ignore_ascii_case(".alpackages")
}

/// Whether a scan of `root` would index `path`: an `.al` file under `root`,
/// reached without a skipped directory or a symlinked one.
///
/// For a single changed path, such as a file-watcher event, which must not
/// index a file the scan leaves out. The file itself is checked when it is
/// read: [`read_source_file`] refuses anything but a regular file.
pub fn is_scanned_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("al"))
    {
        return false;
    }
    let mut directory = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            // The file name.
            return matches!(component, std::path::Component::Normal(_));
        }
        let std::path::Component::Normal(name) = component else {
            return false;
        };
        if is_skipped_directory(&name.to_string_lossy()) {
            return false;
        }
        directory.push(name);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => return false,
            // A deleted file's directory may be gone too; the path is still
            // one the scan could have indexed.
            Ok(_) | Err(_) => {}
        }
    }
    false
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
                if is_skipped_directory(&entry.file_name().to_string_lossy()) {
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

/// Production `ProcedureSource` for the AL interpreter (al-runtime).
///
/// The interpreter (T1) reaches procedures through this seam rather than
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

#[cfg(test)]
mod tests;
