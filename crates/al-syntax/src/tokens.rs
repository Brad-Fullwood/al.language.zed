//! Semantic token extraction from tree-sitter trees.

use tree_sitter::Tree;

/// A semantic token for syntax highlighting.
#[derive(Debug, Clone)]
pub struct SemanticToken {
    pub delta_line: u32,
    pub delta_start: u32,
    pub length: u32,
    pub token_type: u32,
    pub token_modifiers: u32,
}

/// Extract semantic tokens from a parsed tree.
pub fn extract_semantic_tokens(tree: &Tree, text: &str) -> Vec<SemanticToken> {
    let _ = (tree, text);
    todo!("Port semantic tokens from v2")
}
