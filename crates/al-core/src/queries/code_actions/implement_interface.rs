//! Interface stub-generation source action.

use url::Url;

use super::{detect_indent, single_edit_ws};
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use crate::workspace::Workspace;

pub(super) fn source_action_implement_interface(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Vec<CodeActionEntry> {
    let Some((_, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let source = text.as_bytes();

    // LSP positions use UTF-16 code units; tree-sitter uses byte offsets.
    let Some(cursor_line) = text.lines().nth(range.start.line as usize) else {
        return Vec::new();
    };
    let col_bytes =
        crate::resolution::utf16_col_to_byte_offset(cursor_line, range.start.character as usize);
    let point = tree_sitter::Point::new(range.start.line as usize, col_bytes);
    let obj_node = match find_codeunit_at_point(root, source, point) {
        Some(n) => n,
        None => return Vec::new(),
    };

    let interface_names = extract_interface_names(obj_node, source);
    if interface_names.is_empty() {
        return Vec::new();
    }

    let existing_procs = collect_existing_procedures(obj_node, source);
    let insert_line = find_stub_insertion_line(obj_node);

    let mut actions = Vec::new();

    for iface_name in &interface_names {
        let iface_lower = iface_name.to_lowercase();
        let interfaces = workspace
            .symbols
            .get_by_kind(crate::symbols::model::ObjectKind::Interface);
        let iface_entry = interfaces
            .iter()
            .find(|e| e.name.to_lowercase() == iface_lower);
        let iface_entry = match iface_entry {
            Some(e) => e,
            None => continue,
        };

        let missing: Vec<_> = iface_entry
            .methods
            .iter()
            .filter(|m| {
                !existing_procs
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(&m.name))
            })
            .collect();

        if missing.is_empty() {
            continue;
        }

        let indent = detect_indent(text, insert_line.saturating_sub(1));
        let mut stub_text = String::new();

        for method in &missing {
            stub_text.push('\n');
            stub_text.push_str(&indent);
            stub_text.push_str("procedure ");
            stub_text.push_str(&method.name);
            stub_text.push('(');

            let param_strs: Vec<String> = method
                .parameters
                .iter()
                .map(|p| {
                    if p.is_var {
                        format!("var {}: {}", p.name, p.type_name)
                    } else {
                        format!("{}: {}", p.name, p.type_name)
                    }
                })
                .collect();
            stub_text.push_str(&param_strs.join("; "));
            stub_text.push(')');

            if let Some(ref ret) = method.return_type {
                stub_text.push_str(": ");
                stub_text.push_str(ret);
            }

            stub_text.push('\n');

            stub_text.push_str(&indent);
            stub_text.push_str("begin\n");
            stub_text.push_str(&indent);
            stub_text.push_str("    Error('Not implemented');\n");
            stub_text.push_str(&indent);
            stub_text.push_str("end;\n");
        }

        let edit = TextEdit {
            range: Range {
                start: super::Position {
                    line: insert_line,
                    character: 0,
                },
                end: super::Position {
                    line: insert_line,
                    character: 0,
                },
            },
            new_text: stub_text,
        };

        actions.push(CodeActionEntry {
            title: format!("Implement interface '{}'", iface_name),
            kind: CodeActionKind::Refactor,
            edit: Some(single_edit_ws(uri, vec![edit])),
            is_preferred: false,
        });
    }

    actions
}

fn find_codeunit_at_point<'a>(
    root: tree_sitter::Node<'a>,
    source: &[u8],
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node<'a>> {
    for i in 0..root.child_count() {
        let child = root.child(i)?;
        if child.kind() != "object_declaration" {
            continue;
        }
        let ts_range = child.range();
        if point.row < ts_range.start_point.row || point.row > ts_range.end_point.row {
            continue;
        }
        let is_codeunit = child
            .child(0)
            .and_then(|kw| kw.utf8_text(source).ok())
            .map(crate::syntax::language_data::implements_interface_kind)
            .unwrap_or(false);
        if is_codeunit {
            return Some(child);
        }
    }
    None
}

/// Extract interface names from the object declaration node.
///
/// tree-sitter-al wraps each implemented interface in an `implements_clause`
/// node containing `metadata_keyword("implements")` + `name(...)`.
fn extract_interface_names(obj_node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();

    for i in 0..obj_node.child_count() {
        let Some(child) = obj_node.child(i) else {
            continue;
        };

        if child.kind() == "implements_clause" {
            for j in 0..child.child_count() {
                let Some(inner) = child.child(j) else {
                    continue;
                };
                if inner.kind() == "name"
                    || inner.kind() == "identifier"
                    || inner.kind() == "quoted_identifier"
                {
                    if let Ok(t) = inner.utf8_text(source) {
                        let name = t.trim().trim_matches('"');
                        if !name.is_empty() {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
    }

    names
}

fn collect_existing_procedures(obj_node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut procs = Vec::new();
    // Iterative BFS/DFS to avoid stack overflow on deep trees.
    let mut stack = vec![obj_node];
    while let Some(node) = stack.pop() {
        if node.kind() == "procedure_declaration" || node.kind() == "event_procedure_declaration" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    procs.push(name.trim_matches('"').to_string());
                }
            }
            // Don't descend into procedure bodies
            continue;
        }
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
    procs
}

fn find_stub_insertion_line(obj_node: tree_sitter::Node) -> u32 {
    obj_node.end_position().row as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::code_actions::source_actions;
    use crate::symbols::{ObjectKind, SymbolEntry};
    use crate::workspace::Workspace;
    use url::Url;

    fn open_doc(ws: &crate::workspace::Workspace, uri: &url::Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string());
    }

    fn make_interface_entry(name: &str, methods: Vec<crate::symbols::MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Interface,
            id: 0,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            package: String::new(),
            namespace: String::new(),
            methods,
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_method(
        name: &str,
        params: Vec<crate::symbols::ParameterSymbol>,
        return_type: Option<&str>,
    ) -> crate::symbols::MethodSymbol {
        crate::symbols::MethodSymbol {
            name: name.to_string(),
            parameters: params,
            return_type: return_type.map(|s| s.to_string()),
            attributes: Vec::new(),
            is_local: false,
        }
    }

    fn make_param(name: &str, type_name: &str, is_var: bool) -> crate::symbols::ParameterSymbol {
        crate::symbols::ParameterSymbol {
            name: name.to_string(),
            type_name: type_name.to_string(),
            is_var,
        }
    }

    #[test]
    fn implement_interface_offered_for_codeunit_with_implements() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_interface_entry(
            "IMyInterface",
            vec![
                make_method(
                    "DoSomething",
                    vec![make_param("Input", "Text", false)],
                    None,
                ),
                make_method("GetValue", vec![], Some("Integer")),
            ],
        )]);

        let al_code = r#"codeunit 50100 "My Codeunit" implements IMyInterface
{
}
"#;
        let uri = Url::parse("file:///test/Impl.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 0,
                character: 10,
            },
            end: super::super::Position {
                line: 0,
                character: 10,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(
            !impl_actions.is_empty(),
            "Should offer 'Implement interface' action"
        );
        assert!(impl_actions[0].title.contains("IMyInterface"));

        let edit = impl_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;

        assert!(
            new_text.contains("procedure DoSomething"),
            "Should have DoSomething stub"
        );
        assert!(new_text.contains("Input: Text"), "Should have parameter");
        assert!(
            new_text.contains("procedure GetValue"),
            "Should have GetValue stub"
        );
        assert!(new_text.contains(": Integer"), "Should have return type");
        assert!(
            new_text.contains("Error('Not implemented')"),
            "Should have Error placeholder"
        );
    }

    #[test]
    fn implement_interface_skips_already_implemented_methods() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_interface_entry(
            "IMyInterface",
            vec![
                make_method("DoSomething", vec![], None),
                make_method("GetValue", vec![], Some("Integer")),
            ],
        )]);

        let al_code = r#"codeunit 50100 "My Codeunit" implements IMyInterface
{
    procedure DoSomething()
    begin
        Message('Already done');
    end;
}
"#;
        let uri = Url::parse("file:///test/Impl.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 0,
                character: 10,
            },
            end: super::super::Position {
                line: 0,
                character: 10,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(
            !impl_actions.is_empty(),
            "Should still offer action for remaining methods"
        );

        let edit = impl_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;

        assert!(
            !new_text.contains("procedure DoSomething"),
            "Should skip existing method"
        );
        assert!(
            new_text.contains("procedure GetValue"),
            "Should generate missing method"
        );
    }

    #[test]

    fn implement_interface_not_offered_when_all_methods_present() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_interface_entry(
            "IMyInterface",
            vec![make_method("DoSomething", vec![], None)],
        )]);

        let al_code = r#"codeunit 50100 "My Codeunit" implements IMyInterface
{
    procedure DoSomething()
    begin
        Message('Done');
    end;
}
"#;
        let uri = Url::parse("file:///test/Impl.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 0,
                character: 10,
            },
            end: super::super::Position {
                line: 0,
                character: 10,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(
            impl_actions.is_empty(),
            "Should NOT offer when all methods already exist"
        );
    }

    #[test]

    fn implement_interface_not_offered_for_non_implementing_codeunit() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    begin
        Message('Hello');
    end;
}
"#;
        let uri = Url::parse("file:///test/NoImpl.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 0,
                character: 10,
            },
            end: super::super::Position {
                line: 0,
                character: 10,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(
            impl_actions.is_empty(),
            "Should NOT offer for codeunits without implements"
        );
    }

    #[test]
    fn implement_interface_handles_var_parameters() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_interface_entry(
            "IProcessor",
            vec![make_method(
                "Process",
                vec![
                    make_param("Input", "Text", false),
                    make_param("Output", "Text", true),
                ],
                Some("Boolean"),
            )],
        )]);

        let al_code = r#"codeunit 50100 "My Processor" implements IProcessor
{
}
"#;
        let uri = Url::parse("file:///test/Proc.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 0,
                character: 10,
            },
            end: super::super::Position {
                line: 0,
                character: 10,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(!impl_actions.is_empty());

        let edit = impl_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;

        assert!(
            new_text.contains("var Output: Text"),
            "Should include var modifier"
        );
        assert!(
            new_text.contains("Input: Text"),
            "Should include non-var param"
        );
        assert!(new_text.contains(": Boolean"), "Should include return type");
    }
}
