//! Workspace .al file index.
//!
//! Scans a project directory for `.al` files, reads their content, extracts
//! object declarations, and provides fast lookup by object name or file path.
//! This is the single implementation used by both LSP (stdio) and daemon modes.

use std::path::{Path, PathBuf};

use dashmap::DashMap;

/// Maximum number of .al files to scan. Prevents runaway memory usage
/// if a workspace root accidentally includes a huge directory tree.
pub const MAX_WORKSPACE_FILES: usize = 10_000;

/// Maximum directory depth to recurse into.
const MAX_DEPTH: usize = 10;

/// Index of all .al files in a workspace directory.
///
/// Provides three synchronized maps:
/// - `files`: path → file content (full text)
/// - `objects`: lowercase object name → file path
/// - `path_to_object`: file path → lowercase object name (reverse index)
pub struct FileIndex {
    /// File path → full text content.
    pub files: DashMap<PathBuf, String>,
    /// Lowercase object name → file path.
    pub objects: DashMap<String, PathBuf>,
    /// File path → lowercase object name (reverse index for O(1) cleanup).
    pub path_to_object: DashMap<PathBuf, String>,
}

impl FileIndex {
    /// Create an empty file index.
    pub fn new() -> Self {
        Self {
            files: DashMap::new(),
            objects: DashMap::new(),
            path_to_object: DashMap::new(),
        }
    }

    /// Scan a directory tree for .al files and index their contents.
    ///
    /// Skips hidden directories, `node_modules`, and `.alpackages`.
    /// Returns the number of files indexed.
    pub fn scan(&self, root: &Path) -> usize {
        let mut count = 0;
        self.scan_recursive(root, &mut count, 0);
        count
    }

    /// Add a single file to the index. Used when a file is opened/changed.
    pub fn add_file(&self, path: PathBuf, content: String) {
        // Extract object name and update object index
        let result = al_syntax::AlParser::parse_quick(&content);
        if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &content) {
            let obj_name = obj_info.name.to_lowercase();
            self.objects.insert(obj_name.clone(), path.clone());
            self.path_to_object.insert(path.clone(), obj_name);
        }
        self.files.insert(path, content);
    }

    /// Remove a file from the index (e.g., on file close or delete).
    pub fn remove_file(&self, path: &Path) {
        self.files.remove(path);
        if let Some((_, obj_name)) = self.path_to_object.remove(path) {
            self.objects.remove(&obj_name);
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

    fn scan_recursive(&self, dir: &Path, count: &mut usize, depth: usize) {
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

                self.scan_recursive(&path, count, depth + 1);
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("al"))
            {
                if *count >= MAX_WORKSPACE_FILES {
                    return;
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    self.add_file(path, content);
                    *count += 1;
                }
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
}
