//! Hover handler.
//!
//! Provides hover information by looking up the symbol at the cursor position:
//! 1. Local variables / parameters in the current procedure
//! 2. Package symbols from the SymbolIndex
//! 3. Built-in types and methods from the semantic bridge

use tower_lsp::lsp_types::*;

use crate::parsing;
use crate::server::AlServer;

/// Handle textDocument/hover.
pub(crate) fn handle_hover(server: &AlServer, uri: &Url, position: Position) -> Option<Hover> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    // 1. Check if we're on a procedure name or a local parameter
    if let Some(proc_info) = al_syntax::find_procedure_at(&tree, &text, position) {
        if proc_info.name.eq_ignore_ascii_case(clean_name) {
            let content = format_procedure_hover(&proc_info);
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: content,
                }),
                range: Some(al_syntax::ts_range_to_lsp(&node.range())),
            });
        }

        // 2. Check local parameter declarations in the current procedure
        for param in &proc_info.parameters {
            if param.name.eq_ignore_ascii_case(clean_name) {
                let content = format!(
                    "```al\n{}{}: {}\n```\n*(parameter)*",
                    format_param_prefix(param.is_var), param.name, param.type_name
                );
                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: content,
                    }),
                    range: Some(al_syntax::ts_range_to_lsp(&node.range())),
                });
            }
        }
    }

    // 3. Check package symbols from SymbolIndex
    let symbols = server.symbols.get_by_name(clean_name);
    if !symbols.is_empty() {
        let entry = &symbols[0];
        let content = format_symbol_hover(entry);
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: content,
            }),
            range: Some(al_syntax::ts_range_to_lsp(&node.range())),
        });
    }

    // 4. Check built-in types
    {
        let builtins = server.builtins.read().unwrap().clone();
        for bt in builtins.iter() {
            if bt.name.eq_ignore_ascii_case(clean_name) {
                let methods_list: Vec<String> = bt
                    .methods
                    .iter()
                    .take(10)
                    .map(|m| format!("- `{}`", format_builtin_method(m)))
                    .collect();
                let methods_str = if methods_list.is_empty() {
                    String::new()
                } else {
                    format!("\n\n**Methods:**\n{}", methods_list.join("\n"))
                };
                let content = format!("```al\n{}\n```\n*(built-in type)*{}", bt.name, methods_str);
                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: content,
                    }),
                    range: Some(al_syntax::ts_range_to_lsp(&node.range())),
                });
            }

            // Check if it's a method on a built-in type
            for method in &bt.methods {
                if method.name.eq_ignore_ascii_case(clean_name) {
                    let sig = format_builtin_method(method);
                    let doc = if method.documentation.is_empty() {
                        String::new()
                    } else {
                        format!("\n\n{}", method.documentation)
                    };
                    let content =
                        format!("```al\n{}\n```\n*({}.{})*{}", sig, bt.name, method.name, doc);
                    return Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: content,
                        }),
                        range: Some(al_syntax::ts_range_to_lsp(&node.range())),
                    });
                }
            }
        }
    }

    // 5. Check workspace object name index for matching objects
    if let Some(file_path_entry) = server.workspace_objects.get(&clean_name.to_lowercase()) {
        let file_path = file_path_entry.value();
        if let Some(file_text) = server.workspace_files.get(file_path) {
            let mut parser = server.parser.lock().unwrap();
            let result = parser.parse(file_text.value());
            if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, file_text.value()) {
                let content = format!(
                    "```al\n{} {} \"{}\"\n```\n*(workspace)*",
                    obj_info.kind,
                    obj_info.id.map_or(String::new(), |id| id.to_string()),
                    obj_info.name
                );
                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: content,
                    }),
                    range: Some(al_syntax::ts_range_to_lsp(&node.range())),
                });
            }
        }
    }

    None
}

/// Format a "var " prefix for parameter display.
fn format_param_prefix(is_var: bool) -> &'static str {
    if is_var { "var " } else { "" }
}

/// Format a procedure's hover information.
fn format_procedure_hover(proc: &al_syntax::ProcedureInfo) -> String {
    let local = if proc.is_local { "local " } else { "" };
    let params: Vec<String> = proc
        .parameters
        .iter()
        .map(|p| format!("{}{}: {}", format_param_prefix(p.is_var), p.name, p.type_name))
        .collect();
    let params_str = params.join("; ");
    let return_str = proc
        .return_type
        .as_ref()
        .map(|r| format!(": {}", r))
        .unwrap_or_default();

    format!(
        "```al\n{}procedure {}({}){}\n```",
        local, proc.name, params_str, return_str
    )
}

/// Format a symbol entry from the index for hover display.
fn format_symbol_hover(entry: &al_symbols::SymbolEntry) -> String {
    let mut lines = Vec::new();

    let id_str = if entry.id != 0 {
        format!(" {}", entry.id)
    } else {
        String::new()
    };

    lines.push(format!(
        "```al\n{}{} \"{}\"\n```",
        entry.kind, id_str, entry.name
    ));
    lines.push(format!("*({} package)*", entry.package));

    if !entry.fields.is_empty() {
        let field_count = entry.fields.len();
        lines.push(format!("\n**Fields:** {}", field_count));
    }

    if !entry.methods.is_empty() {
        let method_names: Vec<&str> = entry
            .methods
            .iter()
            .filter(|m| !m.is_local)
            .take(8)
            .map(|m| m.name.as_str())
            .collect();
        if !method_names.is_empty() {
            lines.push(format!("\n**Methods:** {}", method_names.join(", ")));
        }
    }

    if !entry.enum_values.is_empty() {
        let values: Vec<&str> = entry
            .enum_values
            .iter()
            .take(10)
            .map(|v| v.name.as_str())
            .collect();
        lines.push(format!("\n**Values:** {}", values.join(", ")));
    }

    lines.join("\n")
}

/// Format a built-in method signature.
fn format_builtin_method(method: &al_semantic::BuiltinMethod) -> String {
    let params: Vec<String> = method
        .parameters
        .iter()
        .map(|p| format!("{}{}: {}", format_param_prefix(p.is_var), p.name, p.type_name))
        .collect();
    let return_str = method
        .return_type
        .as_ref()
        .map(|r| format!(": {}", r))
        .unwrap_or_default();

    format!("{}({}){}", method.name, params.join("; "), return_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_syntax::ProcedureInfo;

    #[test]
    fn test_format_procedure_hover_simple() {
        let proc = ProcedureInfo {
            name: "DoSomething".to_string(),
            range: tree_sitter::Range {
                start_byte: 0,
                end_byte: 0,
                start_point: tree_sitter::Point { row: 0, column: 0 },
                end_point: tree_sitter::Point { row: 0, column: 0 },
            },
            parameters: vec![],
            return_type: None,
            is_local: false,
        };
        let result = format_procedure_hover(&proc);
        assert!(result.contains("procedure DoSomething()"));
        assert!(!result.contains("local"));
    }

    #[test]
    fn test_format_procedure_hover_with_params_and_return() {
        let proc = ProcedureInfo {
            name: "Calculate".to_string(),
            range: tree_sitter::Range {
                start_byte: 0,
                end_byte: 0,
                start_point: tree_sitter::Point { row: 0, column: 0 },
                end_point: tree_sitter::Point { row: 0, column: 0 },
            },
            parameters: vec![
                al_syntax::ParameterInfo {
                    name: "Input".to_string(),
                    type_name: "Integer".to_string(),
                    is_var: false,
                },
                al_syntax::ParameterInfo {
                    name: "Result".to_string(),
                    type_name: "Decimal".to_string(),
                    is_var: true,
                },
            ],
            return_type: Some("Boolean".to_string()),
            is_local: true,
        };
        let result = format_procedure_hover(&proc);
        assert!(result.contains("local procedure Calculate"));
        assert!(result.contains("Input: Integer"));
        assert!(result.contains("var Result: Decimal"));
        assert!(result.contains(": Boolean"));
    }

    #[test]
    fn test_format_symbol_hover() {
        let entry = al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            package: "Base Application".to_string(),
            methods: vec![al_symbols::MethodSymbol {
                name: "GetBalance".to_string(),
                parameters: vec![],
                return_type: Some("Decimal".to_string()),
                attributes: vec![],
                is_local: false,
            }],
            fields: vec![al_symbols::FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
            }],
            controls: vec![],
            enum_values: vec![],
        };
        let result = format_symbol_hover(&entry);
        assert!(result.contains("Table"));
        assert!(result.contains("18"));
        assert!(result.contains("Customer"));
        assert!(result.contains("Base Application"));
        assert!(result.contains("Fields:"), "Should contain 'Fields:', got: {}", result);
        assert!(result.contains("GetBalance"));
    }

    #[test]
    fn test_format_builtin_method() {
        let method = al_semantic::BuiltinMethod {
            name: "CopyStr".to_string(),
            parameters: vec![
                al_semantic::MethodParameter {
                    name: "String".to_string(),
                    type_name: "Text".to_string(),
                    is_var: false,
                },
                al_semantic::MethodParameter {
                    name: "Position".to_string(),
                    type_name: "Integer".to_string(),
                    is_var: false,
                },
            ],
            return_type: Some("Text".to_string()),
            documentation: "Copies a substring.".to_string(),
        };
        let result = format_builtin_method(&method);
        assert_eq!(result, "CopyStr(String: Text; Position: Integer): Text");
    }
}
