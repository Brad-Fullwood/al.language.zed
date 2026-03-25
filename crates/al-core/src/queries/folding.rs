//! Folding ranges query.

use url::Url;

use crate::workspace::Workspace;

/// Get folding ranges for a document.
/// Returns tower-lsp FoldingRange directly since al-syntax produces that type.
pub fn folding_ranges(workspace: &Workspace, uri: &Url) -> Option<Vec<tower_lsp::lsp_types::FoldingRange>> {
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let ranges = al_syntax::extract_folding_ranges(&tree, &text);
    Some(ranges)
}
