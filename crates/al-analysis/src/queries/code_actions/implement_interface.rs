//! Interface stub-generation source action.

use url::Url;

use super::{detect_indent, quote_al_identifier, single_edit_ws};
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use al_workspace::Workspace;

pub(super) fn source_action_implement_interface(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Vec<CodeActionEntry> {
    let Some((_, tree)) = al_source::parsing::get_or_parse(&workspace.documents, uri) else {
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
    let Some(insert_at) = find_stub_insertion_point(obj_node, text) else {
        return Vec::new();
    };
    let object_indent = detect_indent(text, obj_node.start_position().row as u32);
    let indent = format!("{object_indent}    ");
    // A single-line object (`codeunit 50100 X { }`) has no line of its own for
    // the closing brace, so keep it on the same line after the stubs.
    let closing_prefix = if obj_node.start_position().row == obj_node.end_position().row {
        object_indent.as_str()
    } else {
        ""
    };

    let mut actions = Vec::new();

    for iface_name in &interface_names {
        let methods = collect_interface_methods(workspace, iface_name);
        if methods.is_empty() {
            continue;
        }

        let missing: Vec<_> = methods
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

        let mut stub_text = String::new();

        for method in &missing {
            stub_text.push('\n');
            stub_text.push_str(&indent);
            stub_text.push_str("procedure ");
            stub_text.push_str(&quote_al_identifier(&method.name));
            stub_text.push('(');

            let param_strs: Vec<String> = method
                .parameters
                .iter()
                .map(|p| {
                    let name = quote_al_identifier(&p.name);
                    if p.is_var {
                        format!("var {}: {}", name, p.type_name)
                    } else {
                        format!("{}: {}", name, p.type_name)
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
        stub_text.push_str(closing_prefix);

        let edit = TextEdit {
            range: Range {
                start: super::Position {
                    line: insert_at.0,
                    character: insert_at.1,
                },
                end: super::Position {
                    line: insert_at.0,
                    character: insert_at.1,
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

/// Depth cap for `extends` chains, so a malformed symbol set that makes two
/// interfaces extend each other cannot loop forever.
const MAX_INTERFACE_INHERITANCE_DEPTH: usize = 16;

/// Collect every method an implementer must provide for `iface_name`,
/// including those inherited through `interface IB extends IA`.
///
/// Methods declared by the interface itself win over an inherited method with
/// the same name (an override), and the result is ordered
/// most-derived-interface first so generated stubs read in declaration order.
fn collect_interface_methods(
    workspace: &Workspace,
    iface_name: &str,
) -> Vec<al_symbols::MethodSymbol> {
    let interfaces = workspace
        .symbols
        .get_by_kind(al_symbols::model::ObjectKind::Interface);

    let mut methods: Vec<al_symbols::MethodSymbol> = Vec::new();
    let mut seen_methods: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_interfaces: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pending = vec![iface_name.to_string()];

    for _ in 0..MAX_INTERFACE_INHERITANCE_DEPTH {
        let mut next = Vec::new();
        for name in pending.drain(..) {
            let lower = name.to_lowercase();
            if !seen_interfaces.insert(lower.clone()) {
                continue;
            }
            let Some(entry) = interfaces.iter().find(|e| e.name.to_lowercase() == lower) else {
                continue;
            };
            for method in &entry.methods {
                if seen_methods.insert(method.name.to_lowercase()) {
                    methods.push(method.clone());
                }
            }
            // AL spells interface inheritance `interface IB extends IA`; some
            // symbol sources record it under `implements` instead.
            if let Some(base) = &entry.extends {
                next.push(base.clone());
            }
            next.extend(entry.implements.iter().cloned());
        }
        if next.is_empty() {
            break;
        }
        pending = next;
    }

    methods
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
            .map(al_syntax::language_data::implements_interface_kind)
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

/// Position (line, UTF-16 column) immediately *before* the object's closing
/// brace.
///
/// The old implementation returned only the closing brace's row and the edit
/// inserted at column 0 of that row. For a single-line object like
/// `codeunit 50100 X { }` — where the closing brace shares the declaration row
/// — that put the stubs *before* the declaration, producing invalid AL.
/// Anchoring on the brace column is correct for both layouts.
fn find_stub_insertion_point(obj_node: tree_sitter::Node, text: &str) -> Option<(u32, u32)> {
    let end = obj_node.end_position();
    let line = text.lines().nth(end.row)?;
    // The object node ends immediately after its closing `}`.
    if let Some(brace_byte) = end.column.checked_sub(1) {
        if line.as_bytes().get(brace_byte) == Some(&b'}') {
            return Some((
                end.row as u32,
                al_syntax::byte_col_to_utf16_col(line, brace_byte),
            ));
        }
    }
    // Unterminated object (parse error): append at the end of the last line
    // rather than corrupting the declaration.
    Some((
        end.row as u32,
        al_syntax::byte_col_to_utf16_col(line, line.len()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::code_actions::source_actions;
    use al_symbols::{ObjectKind, SymbolEntry};
    use al_workspace::Workspace;
    use url::Url;

    fn open_doc(ws: &al_workspace::Workspace, uri: &url::Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string()).unwrap();
    }

    fn make_interface_entry(name: &str, methods: Vec<al_symbols::MethodSymbol>) -> SymbolEntry {
        make_interface_entry_extending(name, None, methods)
    }

    fn make_interface_entry_extending(
        name: &str,
        extends: Option<&str>,
        methods: Vec<al_symbols::MethodSymbol>,
    ) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Interface,
            id: 0,
            name: name.to_string(),
            extends: extends.map(str::to_string),
            implements: Vec::new(),
            package: String::new(),
            namespace: String::new(),
            methods,
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_method(
        name: &str,
        params: Vec<al_symbols::ParameterSymbol>,
        return_type: Option<&str>,
    ) -> al_symbols::MethodSymbol {
        al_symbols::MethodSymbol {
            name: name.to_string(),
            parameters: params,
            return_type: return_type.map(|s| s.to_string()),
            attributes: Vec::new(),
            is_local: false,
        }
    }

    fn make_param(name: &str, type_name: &str, is_var: bool) -> al_symbols::ParameterSymbol {
        al_symbols::ParameterSymbol {
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

    fn cursor(line: u32, character: u32) -> Range {
        Range {
            start: super::super::Position { line, character },
            end: super::super::Position { line, character },
        }
    }

    /// `find_stub_insertion_line` returned only the closing brace's row and the
    /// edit inserted at column 0, so a single-line object had its stubs placed
    /// *before* the declaration.
    #[test]
    fn implement_interface_on_single_line_object_produces_valid_al() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_interface_entry(
            "IMyInterface",
            vec![make_method("DoSomething", vec![], None)],
        )]);

        let al_code = "codeunit 50100 X implements IMyInterface { }\n";
        let uri = Url::parse("file:///test/ImplSingleLine.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let actions = source_action_implement_interface(&ws, &uri, al_code, cursor(0, 10));
        let action = actions
            .iter()
            .find(|a| a.title.contains("Implement interface"))
            .expect("action offered for single-line object");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            action,
            "implement_interface single line",
        );
        assert!(
            updated.starts_with("codeunit 50100 X implements IMyInterface {"),
            "declaration must stay first: {updated}"
        );
        assert!(updated.contains("procedure DoSomething()"), "{updated}");
    }

    #[test]
    fn implement_interface_multi_line_edit_reparses_cleanly() {
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

        let al_code = "codeunit 50100 \"My Codeunit\" implements IMyInterface\n{\n}\n";
        let uri = Url::parse("file:///test/ImplMultiLine.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let actions = source_action_implement_interface(&ws, &uri, al_code, cursor(0, 10));
        let action = actions
            .iter()
            .find(|a| a.title.contains("Implement interface"))
            .expect("action offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            action,
            "implement_interface multi line",
        );
        assert!(
            updated.contains("procedure DoSomething(Input: Text)"),
            "{updated}"
        );
        assert!(
            updated.contains("procedure GetValue(): Integer"),
            "{updated}"
        );
    }

    /// Methods inherited via `interface IB extends IA` must be stubbed too.
    #[test]
    fn implement_interface_includes_base_interface_methods() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_interface_entry("IBase", vec![make_method("BaseMethod", vec![], None)]),
            make_interface_entry_extending(
                "IDerived",
                Some("IBase"),
                vec![make_method("DerivedMethod", vec![], None)],
            ),
        ]);

        let al_code = "codeunit 50100 X implements IDerived\n{\n}\n";
        let uri = Url::parse("file:///test/ImplInherit.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let actions = source_action_implement_interface(&ws, &uri, al_code, cursor(0, 10));
        let action = actions
            .iter()
            .find(|a| a.title.contains("IDerived"))
            .expect("action offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            action,
            "implement_interface inheritance",
        );
        assert!(updated.contains("procedure DerivedMethod()"), "{updated}");
        assert!(
            updated.contains("procedure BaseMethod()"),
            "inherited method must be stubbed: {updated}"
        );
    }

    /// Names that are not bare AL identifiers must be quoted in the stub.
    #[test]
    fn implement_interface_quotes_names_that_need_quoting() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_interface_entry(
            "IQuoted",
            vec![make_method(
                "Post Document",
                vec![make_param("Sales Header", "Record \"Sales Header\"", true)],
                None,
            )],
        )]);

        let al_code = "codeunit 50100 X implements IQuoted\n{\n}\n";
        let uri = Url::parse("file:///test/ImplQuoted.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let actions = source_action_implement_interface(&ws, &uri, al_code, cursor(0, 10));
        let action = actions
            .iter()
            .find(|a| a.title.contains("IQuoted"))
            .expect("action offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            action,
            "implement_interface quoting",
        );
        assert!(
            updated.contains(
                "procedure \"Post Document\"(var \"Sales Header\": Record \"Sales Header\")"
            ),
            "{updated}"
        );
    }

    #[test]
    fn quote_al_identifier_leaves_bare_names_alone() {
        assert_eq!(quote_al_identifier("DoSomething"), "DoSomething");
        assert_eq!(quote_al_identifier("_private1"), "_private1");
        assert_eq!(quote_al_identifier("Post Document"), "\"Post Document\"");
        // A keyword must be quoted to be usable as a member name.
        assert_eq!(quote_al_identifier("Begin"), "\"Begin\"");
    }
}
