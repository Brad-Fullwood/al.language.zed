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
    let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        return Vec::new();
    };

    let Some(node) = crate::syntax::find_node_at_position(&tree, &text, position.into()) else {
        return Vec::new();
    };
    let Some(interface_name) = super::node_clean_name(node, text.as_bytes()) else {
        return Vec::new();
    };

    let interface_lower = interface_name.to_lowercase();
    let mut locations: Vec<Location> = Vec::new();

    let codeunits = workspace
        .symbols
        .get_by_kind(crate::symbols::model::ObjectKind::Codeunit);
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

    for obj_idx in 0..root.child_count() {
        let obj_node = root.child(obj_idx)?;

        let is_codeunit = obj_node.kind() == "object_declaration"
            && obj_node
                .child(0)
                .and_then(|kw| kw.utf8_text(source).ok())
                .map(crate::syntax::language_data::implements_interface_kind)
                .unwrap_or(false);
        if !is_codeunit {
            continue;
        }

        let has_match = find_implements_clause_match(obj_node, source, interface_lower);
        if has_match {
            let ts_range = obj_node.range();
            return Some(crate::syntax_lsp::ts_range_to_lsp(&ts_range, source).into());
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
    use super::*;
    use crate::symbols::model::{ObjectKind, SymbolEntry};
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    fn make_codeunit_entry(name: &str, id: i32, implements: Vec<String>) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
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
        let index = crate::symbols::index::SymbolIndex::new();
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
        let index = crate::symbols::index::SymbolIndex::new();
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
        let index = crate::symbols::index::SymbolIndex::new();
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

    /// A codeunit source that references an interface via an `implements` clause.
    /// The caret is placed on the interface name so `node_clean_name` yields it.
    fn impl_source(codeunit_name: &str, iface: &str) -> String {
        format!("codeunit 50100 {codeunit_name} implements {iface}\n{{\n}}\n")
    }

    fn iface_position(codeunit_name: &str) -> Position {
        // "codeunit 50100 <name> implements <iface>"
        let prefix = format!("codeunit 50100 {codeunit_name} implements ");
        Position {
            line: 0,
            character: prefix.chars().count() as u32,
        }
    }

    #[test]
    fn find_implementations_returns_empty_for_unopened_document() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent/Closed.al").unwrap();
        let result = find_implementations(
            &ws,
            &uri,
            Position {
                line: 0,
                character: 0,
            },
        );
        assert!(
            result.is_empty(),
            "an unopened document must yield no implementations"
        );
    }

    #[test]
    fn find_implementations_finds_workspace_source_codeunit() {
        let ws = Workspace::new();

        let cur_uri = Url::parse("file:///proj/Caller.al").unwrap();
        let caller_src = impl_source("Caller", "IFoo");
        ws.documents.open(cur_uri.clone(), caller_src);

        let impl_path = PathBuf::from("/proj/FooImpl.al");
        ws.file_index
            .add_file(impl_path.clone(), impl_source("FooImpl", "IFoo"));

        let pos = iface_position("Caller");
        let result = find_implementations(&ws, &cur_uri, pos);

        assert_eq!(
            result.len(),
            1,
            "expected exactly the workspace source implementation, got {result:?}"
        );
        let found = &result[0];
        assert_eq!(
            found.uri,
            Url::from_file_path(&impl_path).unwrap(),
            "located implementation should be the FooImpl source file"
        );
        assert_eq!(found.range.start.line, 0);
    }

    #[test]
    fn find_implementations_is_case_insensitive_in_source_scan() {
        let ws = Workspace::new();

        let cur_uri = Url::parse("file:///proj/Caller.al").unwrap();
        ws.documents
            .open(cur_uri.clone(), impl_source("Caller", "IFoo"));

        let impl_path = PathBuf::from("/proj/FooImpl.al");
        ws.file_index
            .add_file(impl_path.clone(), impl_source("FooImpl", "ifoo"));

        let result = find_implementations(&ws, &cur_uri, iface_position("Caller"));
        assert_eq!(
            result.len(),
            1,
            "interface match must ignore ASCII case in the source scan"
        );
    }

    #[test]
    fn find_implementations_skips_the_current_file() {
        let ws = Workspace::new();

        let cur_path = PathBuf::from("/proj/Caller.al");
        let cur_uri = Url::from_file_path(&cur_path).unwrap();
        let caller_src = impl_source("Caller", "IFoo");
        ws.documents.open(cur_uri.clone(), caller_src.clone());
        ws.file_index.add_file(cur_path, caller_src);

        let result = find_implementations(&ws, &cur_uri, iface_position("Caller"));
        assert!(
            result.is_empty(),
            "the file under the caret must be skipped, got {result:?}"
        );
    }

    #[test]
    fn find_implementations_ignores_unknown_interface_in_source_scan() {
        let ws = Workspace::new();

        let cur_uri = Url::parse("file:///proj/Caller.al").unwrap();
        ws.documents
            .open(cur_uri.clone(), impl_source("Caller", "IUnknown"));

        // The only source implements IFoo, which is not the caret's interface.
        ws.file_index.add_file(
            PathBuf::from("/proj/FooImpl.al"),
            impl_source("FooImpl", "IFoo"),
        );

        let result = find_implementations(&ws, &cur_uri, iface_position("Caller"));
        assert!(
            result.is_empty(),
            "no source implements IUnknown, expected no locations"
        );
    }

    fn parse(src: &str) -> tree_sitter::Tree {
        crate::syntax::AlParser::parse_quick(src).tree
    }

    #[test]
    fn find_codeunit_implementing_interface_matches_codeunit() {
        let src = impl_source("FooImpl", "IFoo");
        let tree = parse(&src);
        let range = find_codeunit_implementing_interface(&tree, &src, "ifoo");
        assert!(
            range.is_some(),
            "codeunit declaring `implements IFoo` should match interface `ifoo`"
        );
        assert_eq!(range.unwrap().start.line, 0);
    }

    #[test]
    fn find_codeunit_implementing_interface_rejects_non_codeunit() {
        // A page object is not a codeunit, so the `is_codeunit` guard must reject
        // it even though the text otherwise mentions the interface name.
        let src = "page 50100 FooPage\n{\n    Caption = 'IFoo';\n}\n";
        let tree = parse(src);
        let range = find_codeunit_implementing_interface(&tree, src, "ifoo");
        assert!(
            range.is_none(),
            "a non-codeunit object must never be reported as an implementation"
        );
    }

    #[test]
    fn find_codeunit_implementing_interface_rejects_wrong_interface() {
        let src = impl_source("FooImpl", "IFoo");
        let tree = parse(&src);
        assert!(
            find_codeunit_implementing_interface(&tree, &src, "ibar").is_none(),
            "codeunit implementing IFoo must not match a search for IBar"
        );
    }

    #[test]
    fn find_implements_clause_match_handles_quoted_interface_name() {
        // Interface names with spaces are quoted in AL; matching strips the quotes.
        let src = "codeunit 50100 FooImpl implements \"My Foo\"\n{\n}\n";
        let tree = parse(src);
        let range = find_codeunit_implementing_interface(&tree, src, "my foo");
        assert!(
            range.is_some(),
            "quoted interface name `\"My Foo\"` should match `my foo`"
        );
    }
}
