//! Test-only `ProcedureSource` mock for the AL interpreter.
//!
//! The production implementation (`impl ProcedureSource for FileIndex`) lives
//! in `al-source`, which depends on this crate — so al-runtime's own unit
//! tests cannot use it without a dependency cycle. This lightweight mock
//! parses AL source on demand via `al-syntax` (a dev-dependency) and serves
//! the four `ProcedureSource` lookups the dispatcher needs. It is aliased to
//! the name `Workspace` in the test modules so the original fixtures
//! (`Workspace::new()`, `ws.file_index.add_file(..)`) compile unchanged.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use al_types::ProcedureSource;

#[derive(Default)]
pub struct MockFileIndex {
    /// (path, source text, object name)
    entries: Mutex<Vec<(PathBuf, String, String)>>,
}

impl MockFileIndex {
    /// Mirrors `FileIndex::add_file(&self, PathBuf, String)`: stores the source
    /// and scrapes the object name from the object-header line.
    pub fn add_file(&self, path: PathBuf, content: String) {
        let object_name = scrape_object_name(&content);
        self.entries
            .lock()
            .unwrap()
            .push((path, content, object_name));
    }
}

/// Empty/`new`-constructed stand-in for the workspace's procedure source.
#[derive(Default)]
pub struct MockSource {
    pub file_index: MockFileIndex,
}

impl MockSource {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProcedureSource for MockSource {
    fn find_by_object_name(&self, name: &str) -> Option<PathBuf> {
        self.file_index
            .entries
            .lock()
            .unwrap()
            .iter()
            .find(|(_, _, obj)| obj.eq_ignore_ascii_case(name))
            .map(|(p, _, _)| p.clone())
    }

    fn iter_paths(&self) -> Vec<PathBuf> {
        self.file_index
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _, _)| p.clone())
            .collect()
    }

    fn get_cached_parse(&self, p: &Path) -> Option<(String, tree_sitter::Tree)> {
        let entries = self.file_index.entries.lock().unwrap();
        let (_, src, _) = entries.iter().find(|(path, _, _)| path == p)?;
        let parsed = al_syntax::AlParser::parse_quick(src);
        Some((src.clone(), parsed.tree))
    }

    fn object_name(&self, p: &Path) -> Option<String> {
        self.file_index
            .entries
            .lock()
            .unwrap()
            .iter()
            .find(|(path, _, _)| path == p)
            .map(|(_, _, obj)| obj.clone())
    }
}

/// Extract the object name from an AL object header (e.g. `codeunit 50999 "Helper"`).
/// Good enough for the interpreter's unit-test fixtures.
fn scrape_object_name(src: &str) -> String {
    let Some(line) = src.lines().find(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    if let Some(start) = line.find('"') {
        let rest = &line[start + 1..];
        if let Some(end) = rest.find('"') {
            return rest[..end].to_string();
        }
    }
    line.split_whitespace()
        .nth(2)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default()
}
