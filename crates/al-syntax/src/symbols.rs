//! Document symbol extraction from tree-sitter trees.

use tower_lsp::lsp_types::DocumentSymbol;
use tree_sitter::Tree;

/// Extract document symbols for the outline view.
pub fn extract_document_symbols(tree: &Tree, text: &str) -> Vec<DocumentSymbol> {
    let _ = (tree, text);
    todo!("Port symbol extraction from v2")
}
