//! Folding ranges query.

use url::Url;

use crate::workspace::Workspace;

/// A folding range in a document.
#[derive(Debug, Clone)]
pub struct FoldingRangeEntry {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: FoldingKind,
}

/// Folding range kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldingKind {
    Region,
    Comment,
    Imports,
}

/// Get folding ranges for a document.
/// Returns tower-lsp FoldingRange directly since al-syntax produces that type.
pub fn folding_ranges(workspace: &Workspace, uri: &Url) -> Option<Vec<tower_lsp::lsp_types::FoldingRange>> {
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let ranges = al_syntax::extract_folding_ranges(&tree, &text);
    Some(ranges)
}
