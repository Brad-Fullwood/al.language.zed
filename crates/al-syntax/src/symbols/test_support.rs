//! Fixtures shared by the `symbols` submodule tests.

use crate::symbols::extract_document_symbols;
use crate::types::SyntaxDocumentSymbol as DocumentSymbol;
use crate::AlParser;
use tree_sitter::Node;

pub(super) fn collect_names_rec(syms: &[DocumentSymbol]) -> Vec<String> {
    let mut names = Vec::new();
    for sym in syms {
        names.push(sym.name.clone());
        if let Some(children) = &sym.children {
            names.extend(collect_names_rec(children));
        }
    }
    names
}

pub(super) fn find_sym<'a>(syms: &'a [DocumentSymbol], name: &str) -> Option<&'a DocumentSymbol> {
    for sym in syms {
        if sym.name == name {
            return Some(sym);
        }
        if let Some(children) = &sym.children {
            if let Some(found) = find_sym(children, name) {
                return Some(found);
            }
        }
    }
    None
}

pub(super) fn parse_symbols(src: &str) -> Vec<DocumentSymbol> {
    let mut parser = AlParser::new();
    let result = parser.parse(src);
    extract_document_symbols(&result.tree, src)
}

pub(super) fn parse_tree(src: &str) -> (tree_sitter::Tree, String) {
    let mut parser = AlParser::new();
    let result = parser.parse(src);
    (result.tree, src.to_string())
}

pub(super) fn find_node_of_kind<'a>(root: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == kind {
            return Some(n);
        }
        stack.extend(n.children(&mut cursor));
    }
    None
}

/// Depth-first search for the first node of `kind` whose own text equals
/// `text` (case-insensitive, quote-trimmed).
pub(super) fn find_node_kind_text<'a>(
    root: Node<'a>,
    kind: &str,
    text: &str,
    src: &str,
) -> Option<Node<'a>> {
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == kind {
            if let Ok(t) = n.utf8_text(src.as_bytes()) {
                if crate::clean_identifier(t).eq_ignore_ascii_case(text) {
                    return Some(n);
                }
            }
        }
        stack.extend(n.children(&mut cursor));
    }
    None
}
