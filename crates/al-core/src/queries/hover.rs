//! Hover information query.

use url::Url;

use super::{Position, Range};
use crate::resolution::{self, ResolvedMemberKind};
use crate::workspace::Workspace;

/// Hover result: markdown content and optional highlight range.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HoverResult {
    pub contents: String,
    pub range: Option<Range>,
}

/// Get hover information at a position in a document.
#[must_use]
pub fn hover(workspace: &Workspace, uri: &Url, position: Position) -> Option<HoverResult> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, lsp_pos)?;
    let source = text.as_bytes();
    // Non-UTF8 node text means the node isn't a valid identifier — skip silently
    let node_text = node.utf8_text(source).unwrap_or("");
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        tracing::debug!("hover: empty clean_name, returning None");
        return None;
    }

    tracing::debug!(name = %clean_name, node_kind = %node.kind(), line = lsp_pos.line, character = lsp_pos.character, "hover: looking up symbol");
    let node_range: Range = al_syntax::ts_range_to_lsp(&node.range(), source).into();

    // Access path resolution (e.g., Rec.Name, Enum::Value)
    if let Some(access) = resolution::access_path_at(&tree, &text, lsp_pos) {
        tracing::debug!(receiver = %access.receiver, member = %access.member, "hover: access path found");
        if let Some(receiver) = resolution::resolve_expression_type(
            workspace,
            uri,
            &text,
            &tree,
            &access.receiver,
            lsp_pos,
        ) {
            if let Some(member) =
                resolution::resolve_member(workspace, uri, &receiver, &access.member)
            {
                let value = match member.kind {
                    ResolvedMemberKind::Variable { scope, .. } => {
                        let type_info = member.type_info.as_ref()?;
                        format!(
                            "```al\n{}: {}\n```\n*({})*",
                            member.name,
                            resolution::format_type_detail(
                                &type_info.type_name,
                                type_info.type_subtype.as_deref()
                            ),
                            scope
                        )
                    }
                    ResolvedMemberKind::Procedure {
                        signature,
                        documentation,
                        ..
                    } => {
                        let mut content = format!("```al\nprocedure {}\n```", signature);
                        if let Some(doc) = documentation {
                            content.push_str("\n\n");
                            content.push_str(&resolution::format_xml_doc(&doc));
                        }
                        content
                    }
                    ResolvedMemberKind::BuiltinMethod {
                        ref signature,
                        ref documentation,
                        ..
                    } => {
                        let overloads = resolution::resolve_builtin_overloads(
                            workspace,
                            &receiver,
                            &access.member,
                        );
                        if overloads.len() > 1 {
                            let mut content = String::new();
                            for (i, overload) in overloads.iter().enumerate() {
                                if let ResolvedMemberKind::BuiltinMethod {
                                    signature: ref sig,
                                    documentation: ref doc,
                                    ..
                                } = overload.kind
                                {
                                    if i > 0 {
                                        content.push_str("\n\n---\n\n");
                                    }
                                    content.push_str(&format!("```al\n{}\n```", sig));
                                    if let Some(d) = doc {
                                        content.push_str("\n\n");
                                        content.push_str(d);
                                    }
                                }
                            }
                            content.push_str(&format!(
                                "\n\n*({} overload{})*",
                                overloads.len(),
                                if overloads.len() == 1 { "" } else { "s" }
                            ));
                            content
                        } else {
                            let mut content = format!("```al\n{}\n```", signature);
                            if let Some(doc) = documentation {
                                content.push_str("\n\n");
                                content.push_str(doc);
                            }
                            content
                        }
                    }
                    ResolvedMemberKind::Field { .. } => {
                        let type_info = member.type_info.as_ref()?;
                        format!(
                            "```al\n{}: {}\n```\n*(field)*",
                            member.name,
                            resolution::format_type_detail(
                                &type_info.type_name,
                                type_info.type_subtype.as_deref()
                            ),
                        )
                    }
                    ResolvedMemberKind::EnumValue { .. } => {
                        let type_info = member.type_info.as_ref()?;
                        format!(
                            "```al\n{}\n```\n*(enum value of {})*",
                            member.name,
                            type_info.type_subtype.as_deref().unwrap_or("Enum")
                        )
                    }
                };
                return Some(HoverResult {
                    contents: value,
                    range: Some(node_range),
                });
            }
        }
    }

    // 1. Check if we're on a procedure name or a local parameter
    if let Some(proc_info) = al_syntax::find_procedure_at(&tree, &text, lsp_pos) {
        if proc_info.name.eq_ignore_ascii_case(clean_name) {
            let mut content = format_procedure_hover(&proc_info);
            if let Some(doc) =
                resolution::extract_doc_comment(&text, proc_info.range.start_point.row)
            {
                content.push_str("\n\n");
                content.push_str(&resolution::format_xml_doc(&doc));
            }
            return Some(HoverResult {
                contents: content,
                range: Some(node_range),
            });
        }

        for param in &proc_info.parameters {
            if param.name.eq_ignore_ascii_case(clean_name) {
                let content = format!("```al\n{}\n```\n*(parameter)*", param);
                return Some(HoverResult {
                    contents: content,
                    range: Some(node_range),
                });
            }
        }
    }

    // 2b. Check local/global variable declarations via TypeResolver
    {
        let resolver = al_syntax::type_resolver::TypeResolver::new(&tree, &text);
        if let Some(decl) = resolver.resolve_type(clean_name, lsp_pos) {
            let label = super::scope_label(&decl.scope);
            let var_prefix = if decl.is_var { "var " } else { "" };
            let subtype = decl
                .type_subtype
                .as_ref()
                .map(|s| format!(" \"{}\"", s))
                .unwrap_or_default();
            let content = format!(
                "```al\n{}{}: {}{}\n```\n*({})*",
                var_prefix, decl.name, decl.type_name, subtype, label
            );
            return Some(HoverResult {
                contents: content,
                range: Some(node_range),
            });
        }
    }

    // 3. Check package symbols from SymbolIndex
    if let Some(entry) = workspace.symbols.find_by_name(clean_name) {
        let content = format_symbol_hover(&entry);
        return Some(HoverResult {
            contents: content,
            range: Some(node_range),
        });
    }

    // 3b. Check built-in global functions (Message, Error, Confirm, etc.)
    if let Some(builtin) = al_syntax::language_data::builtin_function_by_name(clean_name) {
        let mut content = format!("```al\n{}\n```", builtin.signature);
        if !builtin.description.is_empty() {
            content.push_str("\n\n");
            content.push_str(&builtin.description);
        }
        if !builtin.parameters.is_empty() {
            content.push_str("\n\n**Parameters:**");
            for param in &builtin.parameters {
                let req = if param.required { "" } else { " *(optional)*" };
                content.push_str(&format!(
                    "\n- `{}`: {} — {}{}",
                    param.name, param.r#type, param.description, req
                ));
            }
        }
        if let Some(ret) = &builtin.return_type {
            content.push_str(&format!("\n\n**Returns:** `{}`", ret));
        }
        content.push_str(&format!("\n\n*(built-in — {})*", builtin.category));
        return Some(HoverResult {
            contents: content,
            range: Some(node_range),
        });
    }

    // 4. Check built-in types
    {
        let cache = workspace
            .semantic_cache
            .read()
            .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
        if let Some(bt) = cache.get_type(clean_name) {
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
            return Some(HoverResult {
                contents: content,
                range: Some(node_range),
            });
        }

        // Search all types for a method with this name via SemanticCache (O(n) over types,
        // replacing the previous O(n*m) double-loop over workspace.builtins).
        // Note: this is still O(n) over all types — a future improvement would add a
        // reverse index from method name to type in SemanticCache for O(1) lookup.
        let method_hits = cache.find_methods_by_name(clean_name);
        if !method_hits.is_empty() {
            // Group by type name so we can emit a single hover per type with all overloads
            let mut by_type: std::collections::HashMap<&str, Vec<&al_semantic::BuiltinMethod>> =
                std::collections::HashMap::new();
            for (type_name, method) in &method_hits {
                by_type.entry(type_name).or_default().push(method);
            }
            // Pick the first type (stable iteration order not guaranteed, but sufficient for hover)
            if let Some((&type_name, overloads)) = by_type.iter().next() {
                let mut content = String::new();
                for (i, method) in overloads.iter().enumerate() {
                    if i > 0 {
                        content.push_str("\n\n---\n\n");
                    }
                    let sig = format_builtin_method(method);
                    content.push_str(&format!("```al\n{}\n```", sig));
                    if !method.documentation.is_empty() {
                        content.push_str("\n\n");
                        content.push_str(&resolution::format_xml_doc(&method.documentation));
                    }
                }
                if overloads.len() > 1 {
                    content.push_str(&format!(
                        "\n\n*({} overloads on {})*",
                        overloads.len(),
                        type_name
                    ));
                } else {
                    content.push_str(&format!("\n\n*({}.{})*", type_name, clean_name));
                }
                return Some(HoverResult {
                    contents: content,
                    range: Some(node_range),
                });
            }
        }
    }

    // 5. Check workspace object name index
    if let Some(file_path_entry) = workspace.file_index.objects.get(&clean_name.to_lowercase()) {
        let file_path = file_path_entry.value();
        if let Some(cached) = workspace.file_index.object_info.get(file_path) {
            let info = cached.value();
            let content = format!(
                "```al\n{} {} \"{}\"\n```\n*(workspace)*",
                info.kind,
                info.id.map_or(String::new(), |id| id.to_string()),
                info.name
            );
            return Some(HoverResult {
                contents: content,
                range: Some(node_range),
            });
        }
    }

    None
}

/// Full hover: native resolution first, then .NET CodeAnalysis bridge.
///
/// This is the single code path for all entry points (LSP and daemon).
/// SemanticBridge already enforces a 30s internal timeout — no outer wrapper needed.
pub async fn hover_full(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Option<HoverResult> {
    if let Some(result) = hover(workspace, uri, position) {
        return Some(result);
    }

    // Bridge: try .NET CodeAnalysis type_at
    let guard = crate::semantic::get_or_init_bridge(workspace).await?;
    let bridge = guard.as_ref()?;
    let path = uri.to_file_path().ok()?;
    let pos = (position.line + 1, position.character + 1);
    let info = match bridge.type_at(&path, pos).await {
        Ok(v) => v?,
        Err(e) => {
            tracing::debug!(error = %e, "hover_full: bridge error");
            return None;
        }
    };
    let mut contents = format!(
        "```al\n{}\n```\n*({} — CodeAnalysis)*",
        info.name, info.kind
    );
    if let Some(doc) = &info.documentation {
        contents.push_str("\n\n");
        contents.push_str(&resolution::format_xml_doc(doc));
    }
    Some(HoverResult {
        contents,
        range: None,
    })
}

fn format_procedure_hover(proc: &al_syntax::ProcedureInfo) -> String {
    let local = if proc.is_local { "local " } else { "" };
    let params: Vec<String> = proc.parameters.iter().map(|p| p.to_string()).collect();
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
        lines.push(format!("\n**Fields:** {}", entry.fields.len()));
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

fn format_builtin_method(method: &al_semantic::BuiltinMethod) -> String {
    crate::resolution::format_builtin_signature(method)
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
            implements: Vec::new(),
            namespace: String::new(),
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
                properties: vec![],
            }],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        let result = format_symbol_hover(&entry);
        assert!(result.contains("Table"));
        assert!(result.contains("18"));
        assert!(result.contains("Customer"));
        assert!(result.contains("Base Application"));
        assert!(result.contains("Fields:"));
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
