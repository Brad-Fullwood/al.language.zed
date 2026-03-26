//! Shared tree-sitter traversal helpers.
//!
//! Centralises the manual `did_visit` cursor loop that would otherwise appear
//! verbatim in every module that needs to walk a tree.

use tree_sitter::Node;

/// Walk every node in the subtree rooted at `root`, calling `visitor` on each
/// node in pre-order (parent before children).
///
/// The visitor receives each node exactly once.  The traversal uses an
/// iterative cursor loop to avoid stack overflows on deeply-nested trees.
pub fn walk_tree(root: Node, visitor: &mut impl FnMut(Node)) {
    walk_tree_until(root, &mut |node| {
        visitor(node);
        true
    });
}

/// Walk the subtree rooted at `root`, calling `visitor` on each node in
/// pre-order.  If `visitor` returns `false` the traversal stops immediately.
///
/// Returns `true` if the entire subtree was visited, `false` if the visitor
/// requested early termination.
pub fn walk_tree_until(root: Node, visitor: &mut impl FnMut(Node) -> bool) -> bool {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            if !visitor(cursor.node()) {
                return false;
            }
        }
        if !did_visit && cursor.goto_first_child() {
            did_visit = false;
            continue;
        }
        if cursor.goto_next_sibling() {
            did_visit = false;
            continue;
        }
        if cursor.goto_parent() {
            did_visit = true;
            continue;
        }
        break;
    }
    true
}
