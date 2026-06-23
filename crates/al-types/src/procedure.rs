//! Procedure-source abstraction for the AL interpreter.
//!
//! The pure-Rust test interpreter (`al-runtime`, tier 1) needs to look
//! procedures up by object name and walk their cached parse trees, but must
//! not depend on the workspace hub (tier 4) or the file index (`al-source`,
//! tier 1). This trait is the seam: `al-runtime` consumes `dyn ProcedureSource`
//! and `al-source` provides `impl ProcedureSource for FileIndex`.

use std::path::{Path, PathBuf};

/// A read-only source of AL procedures, keyed by file path and object name.
pub trait ProcedureSource {
    /// Path of the file declaring the object named `name` (case-insensitive).
    fn find_by_object_name(&self, name: &str) -> Option<PathBuf>;
    /// Every indexed file path (used when a call has no explicit receiver).
    fn iter_paths(&self) -> Vec<PathBuf>;
    /// Cached `(source_text, parse_tree)` for `p`, if it has been indexed.
    fn get_cached_parse(&self, p: &Path) -> Option<(String, tree_sitter::Tree)>;
    /// Object name (original casing) declared in `p`, if known.
    fn object_name(&self, p: &Path) -> Option<String>;
}
