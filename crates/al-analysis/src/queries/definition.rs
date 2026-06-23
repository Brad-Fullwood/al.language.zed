#![allow(clippy::useless_conversion)]

//! Go-to-definition query.

use al_symbols::SymbolEntry;
use url::Url;

use super::{Location, Position, Range};
use crate::resolution::{self, ResolvedMemberKind};
use al_workspace::Workspace;

#[must_use]
pub fn definition(workspace: &Workspace, uri: &Url, position: Position) -> Option<Vec<Location>> {
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, &text, position.into())?;
    let source = text.as_bytes();
    let clean_name = super::node_clean_name(node, source)?;

    if let Some(access) = resolution::access_path_at(&tree, &text, position) {
        if let Some(receiver) = resolution::resolve_expression_type(
            workspace,
            uri,
            &text,
            &tree,
            &access.receiver,
            position,
        ) {
            if let Some(member) =
                resolution::resolve_member(workspace, uri, &receiver, &access.member)
            {
                match member.kind {
                    ResolvedMemberKind::Variable {
                        range: Some(range), ..
                    }
                    | ResolvedMemberKind::Procedure {
                        range: Some(range), ..
                    }
                    | ResolvedMemberKind::Field { range: Some(range) }
                    | ResolvedMemberKind::EnumValue { range: Some(range) } => {
                        return Some(vec![Location {
                            uri: member.uri.unwrap_or_else(|| uri.clone()),
                            range: range.into(),
                        }]);
                    }
                    _ => {
                        if let Some(entry) = find_package_entry_for_type(
                            workspace,
                            &receiver.type_name,
                            receiver.type_subtype.as_deref(),
                        ) {
                            if let Some((file_uri, range)) = super::get_or_create_virtual_file(
                                workspace,
                                &entry,
                                Some(&access.member),
                            ) {
                                return Some(vec![Location {
                                    uri: file_uri,
                                    range: range.into(),
                                }]);
                            }
                        }
                    }
                }
            }
        }
    }

    let looks_like_object_name = node.kind() == "quoted_identifier"
        || clean_name.contains(' ')
        || is_object_modifier_target(node);
    if looks_like_object_name {
        if let Some((obj_uri, range)) =
            resolution::resolve_workspace_object_definition(workspace, clean_name)
        {
            return Some(vec![Location {
                uri: obj_uri,
                range: range.into(),
            }]);
        }
        let pkg_entries = workspace.symbols.get_by_name(clean_name);
        if let Some(entry) = pkg_entries.into_iter().find(|e| !e.kind.is_extension()) {
            if let Some((file_uri, range)) =
                super::get_or_create_virtual_file(workspace, &entry, None)
            {
                return Some(vec![Location {
                    uri: file_uri,
                    range: range.into(),
                }]);
            }
        }
    }

    let resolver = al_syntax::TypeResolver::new(&tree, &text);
    if let Some(decl) = resolver.resolve_type(clean_name, position.into()) {
        let def_range: Range =
            al_syntax::ts_range_to_syntax(&decl.range, text.as_bytes()).into();
        if def_range.start != position {
            return Some(vec![Location {
                uri: uri.clone(),
                range: def_range,
            }]);
        }
    }

    if let Some(decl_range) = find_same_file_procedure_decl(&tree, source, clean_name) {
        let def_range: Range = al_syntax::ts_range_to_syntax(&decl_range, source).into();
        if def_range.start != position {
            return Some(vec![Location {
                uri: uri.clone(),
                range: def_range,
            }]);
        }
    }

    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let first = &refs[0];
        let def_range: Range = al_syntax::ts_range_to_syntax(first, text.as_bytes()).into();
        if def_range.start != position {
            return Some(vec![Location {
                uri: uri.clone(),
                range: def_range,
            }]);
        }
    }

    let current_path = uri.to_file_path().ok();

    if let Some(obj_path_entry) = workspace.file_index.objects.get(&clean_name.to_lowercase()) {
        let file_path = obj_path_entry.value().clone();
        let is_current = current_path.as_ref().is_some_and(|cp| *cp == file_path);
        if !is_current {
            if let Some(obj_info_entry) = workspace.file_index.object_info.get(&file_path) {
                let obj_info = obj_info_entry.value();
                if let Some(file_text_entry) = workspace.file_index.files.get(&file_path) {
                    if let Ok(file_uri) = Url::from_file_path(&file_path) {
                        return Some(vec![Location {
                            uri: file_uri,
                            range: al_syntax::ts_range_to_syntax(
                                &obj_info.range,
                                file_text_entry.value().as_bytes(),
                            )
                            .into(),
                        }]);
                    }
                }
            }
        }
    }

    if let Some(proc_entries) = workspace.file_index.lookup_procedures(clean_name) {
        for info in &proc_entries {
            if current_path.as_ref() == Some(&info.file) {
                continue;
            }
            if let Ok(file_uri) = Url::from_file_path(&info.file) {
                return Some(vec![Location {
                    uri: file_uri,
                    range: info.selection_range.into(),
                }]);
            }
        }
    }

    let symbols = workspace.symbols.get_by_name(clean_name);
    if let Some(entry) = symbols.into_iter().find(|e| !e.kind.is_extension()) {
        if let Some((file_uri, range)) = super::get_or_create_virtual_file(workspace, &entry, None)
        {
            return Some(vec![Location {
                uri: file_uri,
                range: range.into(),
            }]);
        }
    }

    None
}

fn find_same_file_procedure_decl(
    tree: &tree_sitter::Tree,
    source: &[u8],
    target: &str,
) -> Option<tree_sitter::Range> {
    let mut cursor = tree.walk();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name_node.utf8_text(source) {
                        if name_text.trim_matches('"').eq_ignore_ascii_case(target) {
                            return Some(name_node.range());
                        }
                    }
                }
            }
            _ => {}
        }
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// True when `node` is the target of an `extends`/`implements` clause, e.g. the
/// `Customer` in `tableextension … extends Customer`. That target is an object
/// reference, so it must resolve to the object — not to a same-file identifier
/// match such as the `Customer` segment of a `using …;` line.
fn is_object_modifier_target(node: tree_sitter::Node) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "object_modifier" | "implements_clause" => return true,
            // Reached the object body — we're past the header, so not a target.
            "object_body" => return false,
            _ => {}
        }
        cur = n.parent();
    }
    false
}

fn find_package_entry_for_type(
    workspace: &Workspace,
    type_name: &str,
    subtype: Option<&str>,
) -> Option<std::sync::Arc<SymbolEntry>> {
    let obj_name = subtype.or({
        if al_syntax::language_data::object_type_by_keyword(type_name).is_none() {
            Some(type_name)
        } else {
            None
        }
    })?;
    workspace
        .symbols
        .get_by_name(obj_name)
        .into_iter()
        .find(|e| !e.kind.is_extension())
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{ObjectKind, SymbolEntry};
    use al_workspace::Workspace;

    fn test_uri() -> Url {
        Url::parse("file:///test/src/Test.al").unwrap()
    }

    fn open_doc(ws: &Workspace, uri: &Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string());
    }

    fn make_entry(kind: ObjectKind, id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id,
            name: name.to_string(),
            package: "TestPkg".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn unknown_identifier_returns_none() {
        let ws = Workspace::new();
        let uri = test_uri();
        open_doc(
            &ws,
            &uri,
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin
        UnknownThing := 1;
    end;
}"#,
        );
        let pos = Position {
            line: 4,
            character: 8,
        };
        let result = definition(&ws, &uri, pos);
        assert!(result.is_none(), "unknown identifier should return None");
    }

    #[test]
    fn local_variable_resolved_by_type_resolver() {
        let ws = Workspace::new();
        let uri = test_uri();
        open_doc(
            &ws,
            &uri,
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        MyVar: Integer;
    begin
        MyVar := 42;
    end;
}"#,
        );
        let pos = Position {
            line: 6,
            character: 8,
        };
        let result = definition(&ws, &uri, pos);
        assert!(result.is_some(), "should resolve local variable");
        let locs = result.unwrap();
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, uri);
        assert!(
            locs[0].range.start.line <= 4,
            "should point to declaration, got line {}",
            locs[0].range.start.line
        );
    }

    #[test]
    fn definition_returns_none_for_empty_workspace() {
        let ws = Workspace::new();
        let uri = test_uri();
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = definition(&ws, &uri, pos);
        assert!(result.is_none(), "empty workspace should return None");
    }

    #[test]
    fn symbol_index_hit_for_known_object() {
        let ws = Workspace::new();
        let uri = test_uri();

        ws.symbols
            .add_entries(&[make_entry(ObjectKind::Table, 18, "Customer")]);

        open_doc(
            &ws,
            &uri,
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        Cust: Record "Customer";
    begin
    end;
}"#,
        );
        let pos = Position {
            line: 4,
            character: 24,
        };
        let result = definition(&ws, &uri, pos);
        assert!(result.is_some(), "known package object should return Some");
    }

    #[test]
    fn file_index_hit_for_workspace_object() {
        let ws = Workspace::new();
        let uri = test_uri();

        let table_path = std::path::PathBuf::from("/test/src/MyTable.al");
        ws.file_index.add_file(
            table_path.clone(),
            r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
}"#
            .to_string(),
        );

        open_doc(
            &ws,
            &uri,
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        Rec: Record "My Table";
    begin
    end;
}"#,
        );
        let pos = Position {
            line: 4,
            character: 24,
        };
        let result = definition(&ws, &uri, pos);
        assert!(
            result.is_some(),
            "workspace object should be found via file index"
        );
        let locs = result.unwrap();
        assert_eq!(locs[0].uri, Url::from_file_path(&table_path).unwrap());
    }

    #[test]
    fn procedure_reverse_index_hit() {
        let ws = Workspace::new();
        let uri = test_uri();

        let cu_path = std::path::PathBuf::from("/test/src/Helper.al");
        ws.file_index.add_file(
            cu_path.clone(),
            r#"codeunit 50101 "Helper"
{
    procedure DoSomething()
    begin
    end;
}"#
            .to_string(),
        );

        open_doc(
            &ws,
            &uri,
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin
        DoSomething();
    end;
}"#,
        );
        let pos = Position {
            line: 4,
            character: 8,
        };
        let result = definition(&ws, &uri, pos);
        assert!(
            result.is_some(),
            "cross-file procedure should be found via reverse index"
        );
        let locs = result.unwrap();
        assert_eq!(locs[0].uri, Url::from_file_path(&cu_path).unwrap());
    }

    #[test]
    fn find_package_entry_skips_extensions() {
        let ws = Workspace::new();
        let mut ext = make_entry(ObjectKind::TableExtension, 50100, "Customer");
        ext.extends = Some("Customer".to_string());
        let base = make_entry(ObjectKind::Table, 18, "Customer");
        ws.symbols.add_entries(&[ext, base]);

        let result = find_package_entry_for_type(&ws, "Record", Some("Customer"));
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().kind,
            ObjectKind::Table,
            "should prefer base object over extension"
        );
    }

    #[test]
    fn find_package_entry_returns_none_for_unknown() {
        let ws = Workspace::new();
        let result = find_package_entry_for_type(&ws, "Record", Some("Nonexistent"));
        assert!(result.is_none());
    }

    use crate::queries::Position;

    fn open(ws: &Workspace, src: &str) -> Url {
        let uri = Url::parse("file:///tmp/def-test.al").unwrap();
        ws.documents.open(uri.clone(), src.to_string());
        uri
    }

    /// F-039 regression: caller `Add(1, 2)` appears BEFORE the `procedure Add`
    /// declaration. Hover the call-site `Add` and `definition()` must return
    /// the declaration line, not the call-site itself.
    #[test]
    fn definition_jumps_to_forward_declared_procedure() {
        let src = "codeunit 50100 \"Test\"\n{\n    procedure Caller()\n    begin\n        Add(1, 2);\n    end;\n\n    procedure Add(A: Integer; B: Integer): Integer\n    begin\n        exit(A + B);\n    end;\n}\n";
        let ws = Workspace::new();
        let uri = open(&ws, src);
        let result = definition(
            &ws,
            &uri,
            Position {
                line: 4,
                character: 8,
            },
        )
        .expect("definition should resolve");
        let loc = result.first().expect("at least one location");
        assert_eq!(
            loc.range.start.line, 7,
            "expected declaration on line 7, got {:?}",
            loc.range
        );
    }

    #[test]
    fn definition_does_not_return_callsite_for_known_procedure() {
        let src = "codeunit 50100 \"Test\"\n{\n    procedure Caller()\n    begin\n        Helper();\n    end;\n\n    procedure Helper()\n    begin\n    end;\n}\n";
        let ws = Workspace::new();
        let uri = open(&ws, src);
        let result = definition(
            &ws,
            &uri,
            Position {
                line: 4,
                character: 8,
            },
        )
        .expect("definition should resolve");
        let loc = &result[0];
        assert_ne!(loc.range.start.line, 4);
        assert_eq!(loc.range.start.line, 7);
    }

    #[test]
    fn find_package_entry_uses_type_name_when_not_record() {
        let ws = Workspace::new();
        ws.symbols
            .add_entries(&[make_entry(ObjectKind::Codeunit, 50100, "MyHelper")]);

        let result = find_package_entry_for_type(&ws, "MyHelper", None);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "MyHelper");
    }
}
