//! Hover information query.

use url::Url;

use super::{Position, Range};
use crate::resolution::{self, ResolvedMemberKind};
use al_workspace::Workspace;

/// Hover result: markdown content and optional highlight range.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HoverResult {
    pub contents: String,
    pub range: Option<Range>,
}

#[must_use]
pub fn hover(workspace: &Workspace, uri: &Url, position: Position) -> Option<HoverResult> {
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, &text, position.into())?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).unwrap_or("");
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        tracing::debug!("hover: empty clean_name, returning None");
        return None;
    }

    tracing::debug!(
        name = %clean_name, node_kind = %node.kind(),
        line = position.line, character = position.character,
        "hover: looking up symbol"
    );
    let node_range: Range = al_syntax::ts_range_to_syntax(&node.range(), source).into();

    if let Some(access) = resolution::access_path_at(&tree, &text, position) {
        tracing::debug!(receiver = %access.receiver, member = %access.member, "hover: access path found");
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

    if let Some(proc_info) = al_syntax::find_procedure_at(&tree, &text, position.into()) {
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

        // Cursor inside a `parameter` node (e.g. on the type identifier
        // `Integer` of `A: Integer`). Walk up to the parameter ancestor and
        // look up the corresponding ParameterInfo by name.
        let mut anc = Some(node);
        while let Some(n) = anc {
            if n.kind() == "parameter" {
                if let Some(name_node) = n.child_by_field_name("name") {
                    if let Ok(name_text) = name_node.utf8_text(source) {
                        let pname = name_text.trim_matches('"');
                        for param in &proc_info.parameters {
                            if param.name.eq_ignore_ascii_case(pname) {
                                let content = format!("```al\n{}\n```\n*(parameter)*", param);
                                return Some(HoverResult {
                                    contents: content,
                                    range: Some(node_range),
                                });
                            }
                        }
                    }
                }
                break;
            }
            anc = n.parent();
        }
    }

    {
        let resolver = al_syntax::type_resolver::TypeResolver::new(&tree, &text);
        if let Some(decl) = resolver.resolve_type(clean_name, position.into()) {
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

    if let Some(entry) = workspace.symbols.find_by_name(clean_name) {
        let content = format_symbol_hover(&entry);
        return Some(HoverResult {
            contents: content,
            range: Some(node_range),
        });
    }

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

    {
        let cache = workspace
            .semantic_cache
            .read()
            .unwrap_or_else(|e| e.into_inner());
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

        // Search all types for a method with this name via SemanticCache.
        // SemanticCache::find_methods_by_name uses the pre-built `method_index`
        // (lowercased method name → list of (type_key, method_idx)) so the
        // lookup is O(1) plus O(k) for the k overloads. This replaces the
        // earlier O(n*m) double-loop over workspace.builtins.
        let method_hits = cache.find_methods_by_name(clean_name);
        if !method_hits.is_empty() {
            let mut by_type: std::collections::HashMap<&str, Vec<&al_semantic::BuiltinMethod>> =
                std::collections::HashMap::new();
            for (type_name, method) in &method_hits {
                by_type.entry(type_name).or_default().push(method);
            }
            // Sort by type name for deterministic results — HashMap iteration
            // order is randomised per process, so without sorting hover would
            // jump between types for the same identifier across LSP restarts.
            let mut sorted_types: Vec<(&str, &Vec<&al_semantic::BuiltinMethod>)> =
                by_type.iter().map(|(k, v)| (*k, v)).collect();
            sorted_types.sort_by_key(|(k, _)| *k);
            if sorted_types.len() > 1 {
                let candidates: Vec<&str> = sorted_types.iter().map(|(t, _)| *t).collect();
                tracing::debug!(
                    method = clean_name,
                    selected = ?sorted_types.first().map(|(t, _)| *t),
                    candidates = ?candidates,
                    "hover: multiple types own this method, picking lexicographically first"
                );
            }
            if let Some((type_name, overloads)) = sorted_types.into_iter().next() {
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

    // Clone the file_path out of the first DashMap entry and drop the ref
    // before doing the second lookup, so we are never holding two shard
    // locks across the format! call.
    let file_path = workspace.file_index.object_path(clean_name);
    if let Some(file_path) = file_path {
        if let Some(cached) = workspace.file_index.object_info.get(&file_path) {
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

    let guard = al_workspace::get_or_init_bridge(workspace).await?;
    let bridge = guard.as_ref()?;
    let bridge_generation = bridge.generation();
    let path = uri.to_file_path().ok()?;
    // The bridge uses the same zero-based coordinates as LSP.
    let pos = (position.line, position.character);
    // Open-document text takes precedence over on-disk content.
    let unsaved_text = workspace.documents.get_text(uri);
    let configured_package_cache = workspace.config.read().await.package_cache_path.clone();
    let package_cache = match configured_package_cache {
        Some(path) => Some(path),
        None => workspace
            .project
            .read()
            .await
            .as_ref()
            .map(|project| project.packages_dir.clone()),
    };
    let info = match bridge
        .type_at_with_package_cache(
            &path,
            pos,
            unsaved_text.as_deref(),
            package_cache.as_deref(),
        )
        .await
    {
        Ok(v) => v?,
        Err(e) => {
            tracing::debug!(error = %e, "hover_full: bridge error");
            let restart = matches!(
                e,
                al_semantic::SemanticError::Timeout(_)
                    | al_semantic::SemanticError::Poisoned
                    | al_semantic::SemanticError::HostInit(_)
            );
            drop(guard);
            if restart {
                if let Err(restart_error) =
                    al_workspace::restart_bridge_if_current(workspace, bridge_generation).await
                {
                    tracing::warn!(error = %restart_error, "hover_full: bridge restart failed");
                }
            }
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

    use crate::queries::Position;
    use al_workspace::Workspace;
    use url::Url;

    fn open_doc(ws: &Workspace, src: &str) -> Url {
        let uri = Url::parse("file:///tmp/hover-test.al").unwrap();
        ws.documents.open(uri.clone(), src.to_string());
        uri
    }

    const PARAM_FIXTURE: &str = "codeunit 50150 \"Test\"\n{\n    procedure Add(A: Integer; B: Integer): Integer\n    begin\n        exit(A + B);\n    end;\n}\n";

    #[test]
    fn hover_on_parameter_name_returns_parameter_info() {
        let ws = Workspace::new();
        let uri = open_doc(&ws, PARAM_FIXTURE);
        let r = hover(
            &ws,
            &uri,
            Position {
                line: 2,
                character: 18,
            },
        )
        .expect("hover on parameter name should return a result");
        assert!(r.contents.contains("A: Integer"), "got: {:?}", r.contents);
        assert!(r.contents.contains("(parameter)"));
    }

    #[test]
    fn hover_on_parameter_type_returns_parameter_info() {
        let ws = Workspace::new();
        let uri = open_doc(&ws, PARAM_FIXTURE);
        let r = hover(
            &ws,
            &uri,
            Position {
                line: 2,
                character: 22,
            },
        )
        .expect("hover on parameter type identifier should return a result");
        assert!(r.contents.contains("A: Integer"), "got: {:?}", r.contents);
        assert!(r.contents.contains("(parameter)"));
    }

    #[test]
    fn hover_on_unknown_identifier_returns_none() {
        let ws = Workspace::new();
        let src = "codeunit 50150 \"Test\"\n{\n    procedure Foo()\n    begin\n        TotallyUnknownThing;\n    end;\n}\n";
        let uri = open_doc(&ws, src);
        let r = hover(
            &ws,
            &uri,
            Position {
                line: 4,
                character: 12,
            },
        );
        assert!(
            r.is_none(),
            "expected None for unknown identifier, got {:?}",
            r
        );
    }

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
            synthetic: false,
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

    fn table_entry() -> al_symbols::SymbolEntry {
        al_symbols::SymbolEntry {
            synthetic: false,
            kind: al_symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: vec![
                al_symbols::MethodSymbol {
                    name: "PublicMethod".to_string(),
                    parameters: vec![],
                    return_type: None,
                    attributes: vec![],
                    is_local: false,
                },
                al_symbols::MethodSymbol {
                    name: "SecretLocalMethod".to_string(),
                    parameters: vec![],
                    return_type: None,
                    attributes: vec![],
                    is_local: true,
                },
            ],
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        }
    }

    #[test]
    fn format_symbol_hover_filters_out_local_methods() {
        let entry = table_entry();
        let result = format_symbol_hover(&entry);
        assert!(
            result.contains("PublicMethod"),
            "public method should be shown: {result:?}"
        );
        assert!(
            !result.contains("SecretLocalMethod"),
            "local method must be filtered out: {result:?}"
        );
    }

    #[test]
    fn format_symbol_hover_renders_enum_values() {
        let entry = al_symbols::SymbolEntry {
            synthetic: false,
            kind: al_symbols::ObjectKind::Enum,
            id: 50100,
            name: "Color".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "MyApp".to_string(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![
                al_symbols::EnumValueSymbol {
                    name: "Red".to_string(),
                    ordinal: 0,
                },
                al_symbols::EnumValueSymbol {
                    name: "Green".to_string(),
                    ordinal: 1,
                },
            ],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        let result = format_symbol_hover(&entry);
        assert!(result.contains("Enum"), "got: {result:?}");
        assert!(result.contains("50100"), "got: {result:?}");
        assert!(result.contains("**Values:** Red, Green"), "got: {result:?}");
    }

    #[test]
    fn format_symbol_hover_omits_id_when_zero() {
        let mut entry = table_entry();
        entry.id = 0;
        let result = format_symbol_hover(&entry);
        assert!(
            result.contains("Table \"Customer\""),
            "id 0 should be omitted: {result:?}"
        );
        assert!(
            !result.contains("Table 0"),
            "id 0 must not render a literal 0: {result:?}"
        );
    }

    #[test]
    fn hover_resolves_package_symbol_from_index() {
        let ws = Workspace::new();
        let src = "codeunit 50150 \"Test\"\n{\n    procedure Foo()\n    var\n        C: Record Customer;\n    begin\n        C.Init();\n    end;\n}\n";
        let uri = open_doc(&ws, src);
        ws.symbols.add_entries(std::slice::from_ref(&table_entry()));

        let r = hover(
            &ws,
            &uri,
            Position {
                line: 4,
                character: 18,
            },
        )
        .expect("hover on a known package symbol should resolve");
        assert!(r.contents.contains("Customer"), "got: {:?}", r.contents);
        assert!(
            r.contents.contains("Base Application package"),
            "got: {:?}",
            r.contents
        );
        assert!(r.range.is_some());
    }

    #[test]
    fn hover_resolves_builtin_global_function() {
        // Guard: this path only exists if the static language data ships the
        // function. If "Message" ever stops loading, this test is meaningless,
        // so assert the precondition explicitly.
        let builtin = al_syntax::language_data::builtin_function_by_name("Message")
            .expect("Message builtin should be present in language data");

        let ws = Workspace::new();
        let src = "codeunit 50150 \"Test\"\n{\n    procedure Foo()\n    begin\n        Message('hi');\n    end;\n}\n";
        let uri = open_doc(&ws, src);

        let r = hover(
            &ws,
            &uri,
            Position {
                line: 4,
                character: 10,
            },
        )
        .expect("hover on a built-in global function should resolve");
        assert!(
            r.contents.contains(&builtin.signature),
            "expected signature {:?} in {:?}",
            builtin.signature,
            r.contents
        );
        assert!(
            r.contents.contains("(built-in"),
            "expected built-in marker, got: {:?}",
            r.contents
        );
    }

    #[test]
    fn hover_resolves_local_variable_declaration() {
        let ws = Workspace::new();
        let src = "codeunit 50150 \"Test\"\n{\n    procedure Foo()\n    var\n        Counter: Integer;\n    begin\n        Counter := 1;\n    end;\n}\n";
        let uri = open_doc(&ws, src);

        let r = hover(
            &ws,
            &uri,
            Position {
                line: 6,
                character: 8,
            },
        )
        .expect("hover on a declared local variable should resolve");
        assert!(
            r.contents.contains("Counter: Integer"),
            "got: {:?}",
            r.contents
        );
    }

    #[test]
    fn hover_resolves_workspace_object_from_file_index() {
        let ws = Workspace::new();
        let helper_src = "codeunit 50160 \"MyHelper\"\n{\n    procedure Run() begin end;\n}\n";
        ws.file_index.add_file(
            std::path::PathBuf::from("/tmp/MyHelper.al"),
            helper_src.to_string(),
        );

        let src = "codeunit 50150 \"Caller\"\n{\n    var\n        H: Codeunit \"MyHelper\";\n}\n";
        let uri = open_doc(&ws, src);

        let r = hover(
            &ws,
            &uri,
            Position {
                line: 3,
                character: 22,
            },
        )
        .expect("hover on a workspace object should resolve via the file index");
        assert!(r.contents.contains("MyHelper"), "got: {:?}", r.contents);
        assert!(
            r.contents.contains("(workspace)"),
            "expected workspace marker, got: {:?}",
            r.contents
        );
    }

    #[test]
    fn hover_on_empty_document_returns_none() {
        let ws = Workspace::new();
        let uri = open_doc(&ws, "");
        let r = hover(
            &ws,
            &uri,
            Position {
                line: 0,
                character: 0,
            },
        );
        assert!(
            r.is_none(),
            "empty document should yield no hover, got {r:?}"
        );
    }

    #[tokio::test]
    async fn hover_full_returns_native_result_without_bridge() {
        let ws = Workspace::new();
        let uri = open_doc(&ws, PARAM_FIXTURE);
        let r = hover_full(
            &ws,
            &uri,
            Position {
                line: 2,
                character: 18,
            },
        )
        .await
        .expect("hover_full should return the native hover result");
        assert!(r.contents.contains("A: Integer"), "got: {:?}", r.contents);
    }
}
