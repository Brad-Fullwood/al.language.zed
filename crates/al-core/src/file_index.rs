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
    pub selection_range: tower_lsp::lsp_types::Range,
}

pub struct FileIndex {
    /// File path → full text content.
    pub files: DashMap<PathBuf, String>,
    /// Lowercase object name → file path.
    pub objects: DashMap<String, PathBuf>,
    /// File path → lowercase object name (reverse index for O(1) cleanup).
    pub path_to_object: DashMap<PathBuf, String>,
    /// File path → (mtime, size) snapshot taken at last index time.
    pub file_metadata: DashMap<PathBuf, FileMetadata>,
    /// File path → cached object declaration metadata (avoids re-parsing for workspace/symbol).
    pub object_info: DashMap<PathBuf, CachedObjectInfo>,
    /// File path → cached parse tree (avoids re-parsing for cross-file queries).
    pub file_trees: DashMap<PathBuf, tree_sitter::Tree>,
    /// Lowercase procedure/event name → location (reverse index for O(1) go-to-definition).
    pub procedures: DashMap<String, Vec<CachedProcedureInfo>>,
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
            procedures: DashMap::new(),
            path_to_procedures: DashMap::new(),
        }
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

    /// Scan a directory tree for .al files and index their contents.
    ///
    /// Skips hidden directories, `node_modules`, and `.alpackages`.
    /// Returns the number of files indexed.
    pub fn scan(&self, root: &Path) -> usize {
        let mut count = 0;
        self.walk_al_files(root, &mut count, 0, &mut |path| {
            if let Ok(content) = std::fs::read_to_string(&path) {
                self.add_file(path, content);
            }
        });
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
                None => continue, // can't read metadata — skip
            };
            let needs_index = match self.file_metadata.get(path) {
                Some(prev) => *prev != current_meta,
                None => true, // new file
            };
            if needs_index {
                if let Ok(content) = std::fs::read_to_string(path) {
                    self.add_file_with_meta(path.clone(), content, Some(current_meta));
                    delta.changed.push(path.clone());
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
            self.objects.remove(&old_obj_name);
        }
        // Remove stale procedure entries for this file before re-indexing.
        self.remove_procedures_for_file(&path);

        // Parse once; cache the tree for cross-file queries and extract object metadata.
        let result = al_syntax::AlParser::parse_quick(&content);
        // Cache the tree unconditionally — all files benefit from it.
        self.file_trees.insert(path.clone(), result.tree.clone());
        if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &content) {
            let obj_name = obj_info.name.to_lowercase();
            self.objects.insert(obj_name.clone(), path.clone());
            self.path_to_object.insert(path.clone(), obj_name);
            self.object_info.insert(path.clone(), CachedObjectInfo {
                kind: obj_info.kind,
                id: obj_info.id,
                name: obj_info.name,
                range: obj_info.range,
            });
        } else {
            // No object declaration — remove any stale cached metadata.
            self.object_info.remove(&path);
        }

        // Index procedure/event names for O(1) go-to-definition.
        let doc_symbols = al_syntax::extract_document_symbols(&result.tree, &content);
        let mut proc_names = Vec::new();
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if crate::queries::is_procedure_symbol(child.kind) {
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

        self.files.insert(path, content);
    }

    /// Remove a file from the index (e.g., on file close or delete).
    pub fn remove_file(&self, path: &Path) {
        self.files.remove(path);
        self.file_metadata.remove(path);
        self.file_trees.remove(path);
        self.object_info.remove(path);
        self.remove_procedures_for_file(path);
        if let Some((_, obj_name)) = self.path_to_object.remove(path) {
            self.objects.remove(&obj_name);
        }
    }

    /// Remove all procedure index entries associated with a file path.
    fn remove_procedures_for_file(&self, path: &Path) {
        if let Some((_, old_proc_names)) = self.path_to_procedures.remove(path) {
            for proc_name in old_proc_names {
                if let Some(mut entries) = self.procedures.get_mut(&proc_name) {
                    entries.retain(|e| e.file != path);
                    if entries.is_empty() {
                        drop(entries);
                        self.procedures.remove(&proc_name);
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
        self.objects.get(&name.to_lowercase()).map(|r| r.value().clone())
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

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
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

        assert_eq!(count, 3, "Should find 3 .al files (2 root + 1 subdirectory)");
        assert_eq!(index.len(), 3);
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
        assert_eq!(delta.changed.len(), 3, "All 3 files should be indexed on first call");
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

        assert!(delta.is_empty(), "No files should be re-indexed when nothing changed");
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
        fs::write(
            &new_path,
            r#"report 50100 "New Report" { }"#,
        )
        .unwrap();

        let delta = index.incremental_scan(dir.path());

        assert_eq!(delta.changed.len(), 1, "The new file should appear in changed");
        assert_eq!(delta.removed.len(), 0);
        assert_eq!(index.len(), 4, "Total file count should increase by 1");
        assert!(index.get_content(&new_path).is_some());
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
        assert_eq!(delta.removed.len(), 1, "Deleted file should appear in removed");
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
                let content = format!(r#"codeunit 5010{i} "CU{i}" {{ procedure Proc{i}() begin end; }}"#);
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
            let content = format!(r#"table 5010{i} "T{i}" {{ fields {{ field(1; "No."; Code[20]) {{ }} }} }}"#);
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
                let content = format!(r#"table 5010{i} "T{i}" {{ fields {{ field(1; "No."; Code[20]) {{ }} }} }}"#);
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
            let content = format!(
                r#"codeunit 50100 "Rapid" {{ procedure Version{i}() begin end; }}"#
            );
            index.add_file(path, content);
        }

        // Only the latest version should remain
        let procs = index.procedures.get(&format!("version49"));
        assert!(procs.is_some(), "latest procedure should be in index");
        // Earlier versions should have been cleaned up
        let old = index.procedures.get("version0");
        assert!(old.is_none(), "old procedure should have been removed");
    }
}
