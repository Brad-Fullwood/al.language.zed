//! Go-to-implementations query.
//!
//! Returns all codeunits that implement a given AL interface name.

use url::Url;

use super::{Location, Position, Range};
use al_workspace::Workspace;

/// Find all codeunits that implement the interface whose name is at `position`.
///
/// `None` means the document is not loaded or `position` is not on a name;
/// an empty `Vec` means nothing implements it. Matches the `Option` that
/// `document_symbols` and `folding_ranges` use for the same distinction.
///
/// Sources searched:
/// 1. Symbol index (from .app packages) — codeunits with `implements` populated.
/// 2. Workspace source files — scanned via cached parse trees for `implements_clause` nodes.
#[must_use]
pub fn find_implementations(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Option<Vec<Location>> {
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
    let node = al_syntax::find_node_at_position(&tree, &text, position.into())?;
    let interface_name = super::node_clean_name(node, text.as_bytes())?;

    let interface_lower = interface_name.to_lowercase();
    let mut locations: Vec<Location> = Vec::new();

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
                    range,
                });
            }
        }
    }

    // The cursor's own file is scanned too: a file that declares the
    // interface *and* a codeunit implementing it is ordinary AL, and skipping
    // the whole file meant that codeunit was never listed. Only the
    // declaration under the cursor is excluded, by range.
    let current_path = uri.to_file_path().ok();
    let cursor_range = node.range();
    let mut file_paths: Vec<std::path::PathBuf> = workspace
        .file_index
        .files
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    file_paths.sort();
    for file_path in file_paths {
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        let skip_range = (current_path.as_ref() == Some(&file_path)).then_some(cursor_range);
        for range in find_codeunits_implementing_interface(
            &file_tree,
            &file_text,
            &interface_lower,
            skip_range,
        ) {
            if let Ok(file_uri) = Url::from_file_path(&file_path) {
                locations.push(Location {
                    uri: file_uri,
                    range,
                });
            }
        }
    }

    // DashMap iteration is shard order, so an unsorted result reordered the
    // picker between identical requests.
    locations.sort_by(|left, right| {
        (
            left.uri.as_str(),
            left.range.start.line,
            left.range.start.character,
        )
            .cmp(&(
                right.uri.as_str(),
                right.range.start.line,
                right.range.start.character,
            ))
    });
    Some(locations)
}

/// Every object declaration in the tree whose `implements_clause` names the
/// interface, in document order.
///
/// A declaration containing `skip_range` is left out: that is the interface
/// name the cursor sits on. Returning on the first match meant a file with two
/// implementing codeunits contributed one.
fn find_codeunits_implementing_interface(
    tree: &tree_sitter::Tree,
    text: &str,
    interface_lower: &str,
    skip_range: Option<tree_sitter::Range>,
) -> Vec<Range> {
    let source = text.as_bytes();
    let root = tree.root_node();
    let mut ranges = Vec::new();

    for obj_idx in 0..root.child_count() {
        let Some(obj_node) = root.child(obj_idx) else {
            continue;
        };

        let is_codeunit = obj_node.kind() == "object_declaration"
            && obj_node
                .child(0)
                .and_then(|kw| kw.utf8_text(source).ok())
                .map(al_syntax::language_data::implements_interface_kind)
                .unwrap_or(false);
        if !is_codeunit {
            continue;
        }
        if skip_range.is_some_and(|skip| {
            skip.start_byte >= obj_node.start_byte() && skip.end_byte <= obj_node.end_byte()
        }) {
            continue;
        }

        if find_implements_clause_match(obj_node, source, interface_lower) {
            let ts_range = obj_node.range();
            ranges.push(al_syntax::ts_range_to_syntax(&ts_range, source).into());
        }
    }

    ranges
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
                if al_syntax::clean_identifier(t).to_lowercase() == interface_lower {
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
    use al_symbols::model::{ObjectKind, SymbolEntry};
    use al_workspace::Workspace;
    use std::path::PathBuf;

    fn make_codeunit_entry(name: &str, id: i32, implements: Vec<String>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id,
            name: name.to_string(),
            implements,
            ..Default::default()
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

    /// An unopened document is `None`, distinct from an open document that
    /// nothing implements, which is `Some(vec![])`.
    #[test]
    fn find_implementations_returns_none_for_unopened_document() {
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
            result.is_none(),
            "an unopened document must be None, not an empty list"
        );
    }

    #[test]
    fn find_implementations_finds_workspace_source_codeunit() {
        let ws = Workspace::new();

        let cur_uri = Url::parse("file:///proj/Caller.al").unwrap();
        let caller_src = impl_source("Caller", "IFoo");
        ws.documents.open(cur_uri.clone(), caller_src).unwrap();
        let impl_path = PathBuf::from("/proj/FooImpl.al");
        ws.file_index
            .add_file(impl_path.clone(), impl_source("FooImpl", "IFoo"));

        let pos = iface_position("Caller");
        let result = find_implementations(&ws, &cur_uri, pos).expect("document is open");

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
            .open(cur_uri.clone(), impl_source("Caller", "IFoo"))
            .unwrap();

        let impl_path = PathBuf::from("/proj/FooImpl.al");
        ws.file_index
            .add_file(impl_path.clone(), impl_source("FooImpl", "ifoo"));

        let result = find_implementations(&ws, &cur_uri, iface_position("Caller"))
            .expect("document is open");
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
        ws.documents
            .open(cur_uri.clone(), caller_src.clone())
            .unwrap();
        ws.file_index.add_file(cur_path, caller_src);

        let result = find_implementations(&ws, &cur_uri, iface_position("Caller"))
            .expect("document is open");
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
            .open(cur_uri.clone(), impl_source("Caller", "IUnknown"))
            .unwrap();

        // The only source implements IFoo, which is not the caret's interface.
        ws.file_index.add_file(
            PathBuf::from("/proj/FooImpl.al"),
            impl_source("FooImpl", "IFoo"),
        );

        let result = find_implementations(&ws, &cur_uri, iface_position("Caller"))
            .expect("document is open");
        assert!(
            result.is_empty(),
            "no source implements IUnknown, expected no locations"
        );
    }

    fn parse(src: &str) -> tree_sitter::Tree {
        al_syntax::AlParser::parse_quick(src).tree
    }

    #[test]
    fn find_codeunit_implementing_interface_matches_codeunit() {
        let src = impl_source("FooImpl", "IFoo");
        let tree = parse(&src);
        let ranges = find_codeunits_implementing_interface(&tree, &src, "ifoo", None);
        assert_eq!(
            ranges.len(),
            1,
            "codeunit declaring `implements IFoo` should match interface `ifoo`"
        );
        assert_eq!(ranges[0].start.line, 0);
    }

    #[test]
    fn find_codeunit_implementing_interface_rejects_non_codeunit() {
        // A page object is not a codeunit, so the `is_codeunit` guard must reject
        // it even though the text otherwise mentions the interface name.
        let src = "page 50100 FooPage\n{\n    Caption = 'IFoo';\n}\n";
        let tree = parse(src);
        assert!(
            find_codeunits_implementing_interface(&tree, src, "ifoo", None).is_empty(),
            "a non-codeunit object must never be reported as an implementation"
        );
    }

    #[test]
    fn find_codeunit_implementing_interface_rejects_wrong_interface() {
        let src = impl_source("FooImpl", "IFoo");
        let tree = parse(&src);
        assert!(
            find_codeunits_implementing_interface(&tree, &src, "ibar", None).is_empty(),
            "codeunit implementing IFoo must not match a search for IBar"
        );
    }

    #[test]
    fn find_implements_clause_match_handles_quoted_interface_name() {
        // Interface names with spaces are quoted in AL; matching strips the quotes.
        let src = "codeunit 50100 FooImpl implements \"My Foo\"\n{\n}\n";
        let tree = parse(src);
        assert_eq!(
            find_codeunits_implementing_interface(&tree, src, "my foo", None).len(),
            1,
            "quoted interface name `\"My Foo\"` should match `my foo`"
        );
    }

    const TWO_IMPLEMENTORS: &str = "codeunit 50100 FooImpl implements IFoo\n{\n}\n\ncodeunit 50101 BarImpl implements IFoo\n{\n}\n";

    /// Returning on the first match meant a file with two implementing
    /// codeunits contributed one.
    #[test]
    fn every_implementor_in_a_file_is_listed() {
        let tree = parse(TWO_IMPLEMENTORS);
        let ranges = find_codeunits_implementing_interface(&tree, TWO_IMPLEMENTORS, "ifoo", None);
        assert_eq!(
            ranges.iter().map(|r| r.start.line).collect::<Vec<_>>(),
            vec![0, 4]
        );
    }

    /// The scan used to skip the cursor's whole file, so a file declaring the
    /// interface alongside a codeunit implementing it never listed that
    /// codeunit. Only the declaration under the cursor is excluded.
    #[test]
    fn only_the_declaration_under_the_cursor_is_skipped() {
        let tree = parse(TWO_IMPLEMENTORS);
        let first = tree.root_node().child(0).expect("first object");
        let ranges = find_codeunits_implementing_interface(
            &tree,
            TWO_IMPLEMENTORS,
            "ifoo",
            Some(first.range()),
        );
        assert_eq!(
            ranges.iter().map(|r| r.start.line).collect::<Vec<_>>(),
            vec![4]
        );
    }
}
