//! Node-text helpers shared by every expression-evaluation file in this module.
//!
//! Both wrap a single `tree_sitter::Node` accessor call; kept as named functions
//! only so the call sites read as intent ("the text of this node", "the Nth named
//! child") rather than the raw tree-sitter API.

use tree_sitter::Node;

pub(super) fn utf8_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

pub(super) fn named_child(node: Node<'_>, index: usize) -> Option<Node<'_>> {
    node.named_child(index)
}
