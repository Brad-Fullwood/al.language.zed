#![allow(clippy::useless_conversion)]

//! Go-to-implementations query.
//!
//! Returns all codeunits that implement a given AL interface name.

use url::Url;

use super::{Location, Position, Range};
use crate::workspace::Workspace;

/// Find all codeunits that implement the interface whose name is at `position`.
///
/// Sources searched:
/// 1. Symbol index (from .app packages) — codeunits with `implements` populated.
/// 2. Workspace source files — scanned via cached parse trees for `implements_clause` nodes.
#[must_use]
pub fn find_implementations(workspace: &Workspace, uri: &Url, position: Position) -> Vec<Location> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        return Vec::new();
    };

    let Some(node) = al_syntax::find_node_at_position(&tree, lsp_pos) else {
        return Vec::new();
    };
    let Some(interface_name) = super::node_clean_name(node, text.as_bytes()) else {
        return Vec::new();
    };

    let interface_lower = interface_name.to_lowercase();
    let mut locations: Vec<Location> = Vec::new();

    // 1. Search symbol index (from .app packages)
    let codeunits = workspace
        .symbols
        .get_by_kind(al_symbols::model::ObjectKind::Codeunit);
    for entry in &codeunits {
        if entry
            .implements
            .iter()
            .any(|iface| iface.to_lowercase() == interface_lower)
        {
            if let Some((file_uri, range)) =
                super::get_or_create_virtual_file(workspace, entry, None)
            {
                locations.push(Location {
                    uri: file_uri,
                    range: range.into(),
                });
            }
        }
    }

    // 2. Search workspace source files via cached parse trees
    let current_path = uri.to_file_path().ok(); // SILENT: non-file URIs legitimately have no path
    for file_entry in workspace.file_index.files.iter() {
        let file_path = file_entry.key().clone();
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        if let Some(range) =
            find_codeunit_implementing_interface(&file_tree, &file_text, &interface_lower)
        {
            if let Ok(file_uri) = Url::from_file_path(&file_path) {
                locations.push(Location {
                    uri: file_uri,
                    range,
                });
            }
        }
    }

    locations
}

/// Walk the parse tree looking for an `implements_clause` whose interface name matches.
/// Returns the range of the codeunit object declaration (first child of the root).
fn find_codeunit_implementing_interface(
    tree: &tree_sitter::Tree,
    text: &str,
    interface_lower: &str,
) -> Option<Range> {
    let source = text.as_bytes();
    let root = tree.root_node();

    // Walk top-level children (each is an object declaration)
    for obj_idx in 0..root.child_count() {
        let obj_node = root.child(obj_idx)?;

        // Only consider codeunit declarations
        let is_codeunit = obj_node.kind() == "object_declaration"
            && obj_node
                .child(0)
                .and_then(|kw| kw.utf8_text(source).ok())
                .map(|kw| kw.eq_ignore_ascii_case("codeunit"))
                .unwrap_or(false);
        if !is_codeunit {
            continue;
        }

        // Look for an `implements_clause` child
        let has_match = find_implements_clause_match(obj_node, source, interface_lower);
        if has_match {
            let ts_range = obj_node.range();
            return Some(al_syntax::ts_range_to_lsp(&ts_range, source).into());
        }
    }

    None
}

/// Search direct children of an `object_declaration` node for an `implements_clause`
/// that names the given interface.
///
/// `implements_clause` is always a direct child of `object_declaration`, so a
/// single-level scan is sufficient — recursion is not needed.
fn find_implements_clause_match(
    obj_node: tree_sitter::Node<'_>,
    source: &[u8],
    interface_lower: &str,
) -> bool {
    for i in 0..obj_node.child_count() {
        let Some(child) = obj_node.child(i) else {
            continue;
        };
        if child.kind() != "implements_clause" {
            continue;
        }
        // Walk the children of the clause looking for a matching interface name token.
        for j in 0..child.child_count() {
            let Some(token) = child.child(j) else {
                continue;
            };
            if let Ok(t) = token.utf8_text(source) {
                if t.trim_matches('"').to_lowercase() == interface_lower {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use al_symbols::model::{ObjectKind, SymbolEntry};

    fn make_codeunit_entry(name: &str, id: i32, implements: Vec<String>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id,
            name: name.to_string(),
            extends: None,
            implements,
            package: String::new(),
            namespace: String::new(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    #[test]
    fn test_find_implementations_from_symbol_index() {
        let index = al_symbols::index::SymbolIndex::new();
        let entry = make_codeunit_entry("MyImpl", 50001, vec!["IFoo".to_string()]);
        index.add_entries(&[entry]);

        let codeunits = index.get_by_kind(ObjectKind::Codeunit);
        assert_eq!(codeunits.len(), 1);

        let interface_lower = "ifoo";
        let matches: Vec<_> = codeunits
            .iter()
            .filter(|e| {
                e.implements
                    .iter()
                    .any(|i| i.to_lowercase() == interface_lower)
            })
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "MyImpl");
    }

    #[test]
    fn test_find_implementations_case_insensitive() {
        let index = al_symbols::index::SymbolIndex::new();
        let entry = make_codeunit_entry("CaseImpl", 50002, vec!["IBar".to_string()]);
        index.add_entries(&[entry]);

        let codeunits = index.get_by_kind(ObjectKind::Codeunit);
        let interface_lower = "ibar";
        let matches: Vec<_> = codeunits
            .iter()
            .filter(|e| {
                e.implements
                    .iter()
                    .any(|i| i.to_lowercase() == interface_lower)
            })
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "Case-insensitive match should find the codeunit"
        );
    }

    #[test]
    fn test_no_implementations_for_unknown_interface() {
        let index = al_symbols::index::SymbolIndex::new();
        let entry = make_codeunit_entry("OtherImpl", 50003, vec!["IFoo".to_string()]);
        index.add_entries(&[entry]);

        let codeunits = index.get_by_kind(ObjectKind::Codeunit);
        let interface_lower = "iunknown";
        let matches: Vec<_> = codeunits
            .iter()
            .filter(|e| {
                e.implements
                    .iter()
                    .any(|i| i.to_lowercase() == interface_lower)
            })
            .collect();
        assert!(
            matches.is_empty(),
            "Should find no implementations for an unknown interface"
        );
    }
}
