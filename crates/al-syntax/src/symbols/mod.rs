//! Document symbol extraction from tree-sitter trees.

mod callable;
mod controls;
mod kinds;
mod labels;
mod object;
mod section;
mod variables;

#[cfg(test)]
mod test_support;

use crate::types::SyntaxDocumentSymbol as DocumentSymbol;
use object::{extract_namespace_symbol, extract_object_symbol};
use tracing::debug;
use tree_sitter::Tree;

/// Extract document symbols for the outline view.
///
/// Produces a hierarchical symbol tree:
/// - Top-level object (table, page, codeunit, etc.)
///   - Procedures and triggers
///     - Local variables
///     - Executable scopes (`begin`, `if`, `case`, loops, and `with`)
///   - Fields (for tables)
///   - Controls and actions (for pages)
///   - Data items (for reports)
///   - Keys
///   - Enum values
pub fn extract_document_symbols(tree: &Tree, text: &str) -> Vec<DocumentSymbol> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut symbols = Vec::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "object_declaration" => {
                if let Some(sym) = extract_object_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "namespace_or_using_declaration" => {
                if let Some(sym) = extract_namespace_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
    }

    debug!(total = symbols.len(), "extract_document_symbols: complete");
    symbols
}
