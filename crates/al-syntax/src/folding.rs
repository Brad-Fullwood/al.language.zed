//! Folding range extraction from tree-sitter trees.

use tower_lsp::lsp_types::FoldingRange;
use tree_sitter::Tree;

/// Extract folding ranges from a parsed tree.
pub fn extract_folding_ranges(tree: &Tree, text: &str) -> Vec<FoldingRange> {
    let _ = (tree, text);
    todo!("Port folding ranges from v2")
}
