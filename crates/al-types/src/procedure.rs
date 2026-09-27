//! Procedure-source abstraction for the AL interpreter.

use std::path::{Path, PathBuf};

/// A read-only source of AL procedures, keyed by file path and object name.
pub trait ProcedureSource {
    /// Path of the file declaring the object named `name` (case-insensitive),
    /// of any kind. A table and its card page often share a name, so a caller
    /// that knows the kind uses [`find_object_of_kind`](Self::find_object_of_kind).
    fn find_by_object_name(&self, name: &str) -> Option<PathBuf>;
    /// Path of the file declaring the object named `name` whose kind is one of
    /// `kinds` (AL keywords such as `table`, case-insensitive).
    fn find_object_of_kind(&self, name: &str, kinds: &[&str]) -> Option<PathBuf>;
    /// Every indexed file path (used when a call has no explicit receiver).
    fn iter_paths(&self) -> Vec<PathBuf>;
    /// Cached `(source_text, parse_tree)` for `p`, if it has been indexed.
    fn get_cached_parse(&self, p: &Path) -> Option<(String, tree_sitter::Tree)>;
    /// Object name (original casing) declared in `p`, if known.
    fn object_name(&self, p: &Path) -> Option<String>;
}
