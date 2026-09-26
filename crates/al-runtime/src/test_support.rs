//! Test-only `ProcedureSource` mock for the AL interpreter.
//!
//! The production implementation (`impl ProcedureSource for FileIndex`) lives
//! in `al-source`, which depends on this crate — so al-runtime's own unit
//! tests cannot use it without a dependency cycle. This lightweight mock
//! parses AL source on demand via `al-syntax` (a dev-dependency) and serves
//! the four `ProcedureSource` lookups the dispatcher needs. It is aliased to
//! the name `Workspace` in the test modules so the original fixtures
//! (`Workspace::new()`, `ws.file_index.add_file(..)`) compile unchanged.

use al_syntax::IdentifierText;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use al_types::ProcedureSource;

#[derive(Default)]
pub struct MockFileIndex {
    /// (path, source text, the names of every object the file declares)
    entries: Mutex<Vec<(PathBuf, String, Vec<String>)>>,
}

impl MockFileIndex {
    /// Mirrors `FileIndex::add_file(&self, PathBuf, String)`: stores the source
    /// and the name of every object it declares, as the file index does.
    pub fn add_file(&self, path: PathBuf, content: String) {
        let mut object_names = declared_object_names(&content);
        if object_names.is_empty() {
            object_names.push(scrape_object_name(&content));
        }
        self.entries
            .lock()
            .unwrap()
            .push((path, content, object_names));
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
            .find(|(_, _, objects)| objects.iter().any(|obj| obj.eq_ignore_ascii_case(name)))
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
            .and_then(|(_, _, objects)| objects.first().cloned())
    }
}

/// The name of every object declaration in `src`, in order.
fn declared_object_names(src: &str) -> Vec<String> {
    let parsed = al_syntax::parser::AlParser::parse_quick(src);
    let root = parsed.tree.root_node();
    let mut cursor = root.walk();
    let names = root
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "object_declaration")
        .filter_map(|node| node.child_by_field_name("name"))
        .filter_map(|name| name.utf8_text(src.as_bytes()).ok())
        .map(|name| name.unquote_identifier().into_owned())
        .collect();
    names
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
        .map(|s| s.unquote_identifier().into_owned())
        .unwrap_or_default()
}
