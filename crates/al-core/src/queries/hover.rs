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

    // 0a. AL keywords — no hover for structural keywords (begin, end, if, then, etc.)
    // kw_exit gets a special description; all other kw_* nodes return None.
    if node.kind().starts_with("kw_") {
        if node.kind() == "kw_exit" {
            return Some(HoverResult {
                contents: "```al\nexit\n```\n\nExits the current trigger or procedure. When used as `exit(value)`, returns *value* to the caller.".to_string(),
                range: Some(node_range),
            });
        }
        return None;
    }
    // Also skip metadata_keyword, control_keyword, property_keyword etc.
    // BUT: if the keyword is inside a `name_or_keyword` parent, it's being used as an
    // identifier (e.g., `Value: Integer` where `Value` is both a keyword and a variable name).
    // In that case, don't skip — let the hover resolve it as an identifier.
    if node.kind().ends_with("_keyword") || node.kind() == "keyword" || node.kind() == "operator" {
        let in_name_context = node.parent().is_some_and(|p| p.kind() == "name_or_keyword");
        if !in_name_context {
            return None;
        }
    }

    // 0b. Attribute name — return attribute description when node is inside an `attribute` node
    if let Some(attr_hover) = hover_attribute(node, clean_name, &node_range) {
        return Some(attr_hover);
    }

    // 0c. Field declaration name — show field number and type when cursor is on a field name
    //     inside a `field_declaration` node (e.g., field(1; "No."; Code[20]))
    if let Some(content) = hover_field_declaration(node, source) {
        return Some(HoverResult {
            contents: content,
            range: Some(node_range),
        });
    }

    // 0d. Enum value declaration — show value info when cursor is on a value name
    //     inside a value(N; Name) block in an enum definition
    if let Some(content) = hover_enum_value_declaration(node, source) {
        return Some(HoverResult {
            contents: content,
            range: Some(node_range),
        });
    }

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
                            content.push_str(&doc);
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
            let content = format_procedure_hover(&proc_info);
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

    // 1b. Check if clean_name matches ANY procedure in this file (for call-site hover)
    {
        let doc_symbols = al_syntax::extract_document_symbols(&tree, &text);
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if child.name.eq_ignore_ascii_case(clean_name)
                        && matches!(
                            child.kind,
                            tower_lsp::lsp_types::SymbolKind::FUNCTION
                                | tower_lsp::lsp_types::SymbolKind::METHOD
                                | tower_lsp::lsp_types::SymbolKind::EVENT
                        )
                    {
                        let detail = child.detail.as_deref().unwrap_or("()");
                        let content = format!("```al\nprocedure {}{}\n```", child.name, detail);
                        return Some(HoverResult {
                            contents: content,
                            range: Some(node_range),
                        });
                    }
                }
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

    // 3. Check package symbols from SymbolIndex (prefer base objects over extensions)
    {
        let entries = workspace.symbols.get_by_name(clean_name);
        if !entries.is_empty() {
            // Prefer the first non-extension entry; fall back to the first entry.
            let entry = entries
                .iter()
                .find(|e| !e.kind.is_extension())
                .or_else(|| entries.first())
                .unwrap(); // safe: entries is non-empty
            let content = format_symbol_hover(entry);
            return Some(HoverResult {
                contents: content,
                range: Some(node_range),
            });
        }
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
                        content.push_str(&resolution::strip_xml_tags(&method.documentation));
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

    // 6. Global built-in function fallback (used when semantic cache is empty / no .NET bridge)
    // Must run BEFORE enum value lookup because some builtin names (e.g. "Message") also
    // appear as enum values in BC packages.
    if let Some(content) = hover_global_builtin(clean_name) {
        return Some(HoverResult {
            contents: content,
            range: Some(node_range),
        });
    }

    // 7. Enum value lookup — search symbol index for any enum that has a value named clean_name
    if let Some(content) = hover_enum_value(workspace, clean_name) {
        return Some(HoverResult {
            contents: content,
            range: Some(node_range),
        });
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
        contents.push_str(doc);
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

/// Check if the cursor is inside an `enum_value_declaration` node.
fn hover_enum_value_declaration(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    // Walk up to find enum_value_declaration
    let mut current = node;
    let value_decl = loop {
        if current.kind() == "enum_value_declaration" {
            break current;
        }
        current = current.parent()?;
        if matches!(current.kind(), "object_declaration" | "ERROR") {
            return None;
        }
    };

    // Extract id and name from the enum_value_declaration
    let ordinal = value_decl
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("?");
    let name = value_decl
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("?")
        .trim_matches('"');

    // Find the containing enum name from object_declaration ancestor
    let mut up = value_decl;
    let enum_name = loop {
        up = up.parent()?;
        if up.kind() == "object_declaration" {
            let obj_name = (0..up.child_count())
                .find_map(|i| {
                    let c = up.child(i)?;
                    if c.kind() == "quoted_identifier" || c.kind() == "identifier" {
                        c.utf8_text(source).ok()
                    } else {
                        None
                    }
                })
                .unwrap_or("?")
                .trim_matches('"');
            break obj_name.to_string();
        }
    };

    Some(format!(
        "```al\nvalue({}; \"{}\")\n```\n*(enum value of \"{}\", ordinal {})*",
        ordinal, name, enum_name, ordinal
    ))
}

/// Check if the cursor is on a field name inside a `field(NUM; "Name"; Type)` structure.
/// The grammar represents this as metadata_keyword("field") + parenthesized_block.
fn hover_field_declaration(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    // Find the enclosing parenthesized_block
    let paren_block = {
        let mut current = node;
        loop {
            if current.kind() == "parenthesized_block" {
                break current;
            }
            current = current.parent()?;
            if matches!(
                current.kind(),
                "object_declaration" | "object_body" | "ERROR"
            ) {
                return None;
            }
        }
    };

    // Check that the preceding sibling is "field" metadata_keyword
    let prev = paren_block.prev_sibling()?;
    if prev.kind() != "metadata_keyword" {
        return None;
    }
    let kw_text = prev.utf8_text(source).ok()?;
    if !kw_text.eq_ignore_ascii_case("field") {
        return None;
    }

    // Extract field info from parenthesized_block text: (NUM; "Name"; Type)
    let block_text = paren_block.utf8_text(source).ok()?;
    let inner = block_text.trim_start_matches('(').trim_end_matches(')');
    let parts: Vec<&str> = inner.splitn(3, ';').collect();
    if parts.len() >= 3 {
        let num = parts[0].trim();
        let name = parts[1].trim().trim_matches('"');
        let type_text = parts[2].trim().to_string();
        Some(format!(
            "```al\nfield({}; \"{}\"; {})\n```\n*(table field)*",
            num, name, type_text
        ))
    } else {
        None
    }
}

/// Check if the node is the name field of an `attribute` node and return hover text.
fn hover_attribute(
    node: tree_sitter::Node<'_>,
    clean_name: &str,
    node_range: &Range,
) -> Option<HoverResult> {
    // The attribute grammar: attribute -> '[' identifier attribute_argument_list? ']'
    // The cursor lands on an identifier whose parent is the `attribute` node.
    let parent = node.parent()?;
    if parent.kind() != "attribute" {
        return None;
    }
    // Confirm the node is the name field of the attribute
    let name_node = parent.child_by_field_name("name")?;
    if name_node.id() != node.id() {
        return None;
    }

    let description = attribute_description(clean_name);
    let content = format!("```al\n[{}]\n```\n\n{}", clean_name, description);
    Some(HoverResult {
        contents: content,
        range: Some(*node_range),
    })
}

/// Return a description for a known AL attribute, or a generic description.
fn attribute_description(name: &str) -> &'static str {
    match name {
        "IntegrationEvent" => "Marks a procedure as an integration event publisher. Other extensions can subscribe to this event using `[EventSubscriber]`.",
        "BusinessEvent" => "Marks a procedure as a business event publisher. Business events represent meaningful business actions that other extensions can react to.",
        "EventSubscriber" => "Marks a procedure as a subscriber to an integration or business event. The procedure is called when the event is raised.",
        "Test" => "Marks a procedure as a unit test. The test runner executes all procedures with this attribute.",
        "TestPermissions" => "Specifies the permissions used when running the test. Values: `Disabled`, `Restrictive`, `NonRestrictive`, `InheritFromTestCodounit`.",
        "TransactionModel" => "Controls the transaction model for a test: `AutoCommit` or `AutoRollback`.",
        "CommitBehavior" => "Controls commit behavior within the procedure: `Ignore` (commits are silently ignored) or `Error` (commits raise an error).",
        "ErrorBehavior" => "Controls how errors are collected: `ThrowError` (default) or `Collect` (errors are collected and not immediately raised).",
        "InherentPermissions" => "Declares inherent permissions that are always in effect when the procedure runs, regardless of the caller's permissions.",
        "InherentEntitlements" => "Declares inherent entitlements that are always in effect when the procedure runs.",
        "NonDebuggable" => "Prevents the procedure from being stepped into during debugging.",
        "TryFunction" => "Marks a procedure as a try function. Errors inside are caught and the procedure returns `false` instead of raising.",
        "Scope" | "ApplicationScope" => "Specifies the visibility scope of the object or procedure.",
        "ObsoleteState" | "Obsolete" => "Marks the element as obsolete. Specify the reason in the `ObsoleteReason` property.",
        "ObsoleteReason" => "Provides the reason why the element is obsolete and what to use instead.",
        "ObsoleteTag" => "Specifies the version when the element was marked as obsolete.",
        "ExternalBusinessEvent" => "Marks a procedure as an external business event that can be subscribed to from outside the extension.",
        _ => "AL procedure attribute. Modifies the behavior or metadata of the procedure.",
    }
}

/// Search the symbol index for any enum that contains a value matching clean_name.
///
/// Returns hover markdown if found, None otherwise.
/// Only searches workspace-file enums and package index enums — does not scan
/// all entries (enum values are rare enough that the linear scan is acceptable).
fn hover_enum_value(workspace: &Workspace, clean_name: &str) -> Option<String> {
    let lower = clean_name.to_lowercase();

    // Search package symbol index for enums containing this value
    for entry in workspace.symbols.get_by_kind(al_symbols::ObjectKind::Enum) {
        for value in &entry.enum_values {
            if value.name.eq_ignore_ascii_case(&lower) {
                return Some(format!(
                    "```al\n{}\n```\n*(enum value of {} \"{}\", ordinal {})*",
                    value.name, entry.kind, entry.name, value.ordinal
                ));
            }
        }
    }

    None
}

/// Common AL global built-in functions with their signatures and descriptions.
///
/// This is a compile-time fallback used when the semantic cache is empty (no
/// .NET bridge / ALTool not installed). The semantic cache path (step 4) takes
/// priority and will override these when available.
fn hover_global_builtin(name: &str) -> Option<String> {
    let func = al_syntax::language_data::builtin_function_by_name(name)?;
    Some(format!(
        "```al\n{}\n```\n\n{}\n\n*(global built-in)*",
        func.signature, func.description
    ))
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

    // --- exit keyword ---

    #[test]
    fn hover_exit_keyword_returns_description() {
        let ws = crate::workspace::Workspace::new();
        let uri = url::Url::parse("file:///test/src/Test.al").unwrap();
        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    procedure Foo(): Boolean
    begin
        exit(true);
    end;
}"#
            .to_string(),
        );
        // Position on "exit" — line 4, character 8
        let pos = super::super::Position {
            line: 4,
            character: 8,
        };
        let result = hover(&ws, &uri, pos);
        assert!(result.is_some(), "exit keyword should return hover");
        let content = result.unwrap().contents;
        assert!(content.contains("exit"), "content should mention exit");
        assert!(
            content.contains("Exits"),
            "content should describe exit semantics"
        );
    }

    // --- attribute hover ---

    #[test]
    fn hover_integration_event_attribute_returns_description() {
        let ws = crate::workspace::Workspace::new();
        let uri = url::Url::parse("file:///test/src/Test.al").unwrap();
        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    [IntegrationEvent(false, false)]
    procedure OnSomething()
    begin
    end;
}"#
            .to_string(),
        );
        // Position on "IntegrationEvent" — line 2, character 5
        let pos = super::super::Position {
            line: 2,
            character: 5,
        };
        let result = hover(&ws, &uri, pos);
        assert!(
            result.is_some(),
            "IntegrationEvent attribute should return hover"
        );
        let content = result.unwrap().contents;
        assert!(
            content.contains("IntegrationEvent"),
            "content should contain attribute name"
        );
        assert!(
            content.contains("integration event"),
            "content should describe the attribute"
        );
    }

    #[test]
    fn hover_test_attribute_returns_description() {
        let ws = crate::workspace::Workspace::new();
        let uri = url::Url::parse("file:///test/src/Test.al").unwrap();
        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    [Test]
    procedure MyTest()
    begin
    end;
}"#
            .to_string(),
        );
        // Position on "Test" inside the attribute — line 2, character 5
        let pos = super::super::Position {
            line: 2,
            character: 5,
        };
        let result = hover(&ws, &uri, pos);
        assert!(result.is_some(), "Test attribute should return hover");
        let content = result.unwrap().contents;
        assert!(
            content.contains("[Test]"),
            "content should show attribute syntax"
        );
        assert!(content.contains("test"), "content should mention testing");
    }

    // --- symbol index type reference (prefer base over extension) ---

    #[test]
    fn hover_symbol_prefers_base_over_extension() {
        let ws = crate::workspace::Workspace::new();
        let uri = url::Url::parse("file:///test/src/Test.al").unwrap();

        // Add both an extension and a base object with the same name
        let base = al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "Base Application".to_string(),
            extends: None,
            implements: vec![],
            namespace: String::new(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        let ext = al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::TableExtension,
            id: 50100,
            name: "Customer".to_string(),
            package: "My Extension".to_string(),
            extends: Some("Customer".to_string()),
            implements: vec![],
            namespace: String::new(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        // Insert extension first so it would be picked up by find_by_name's first-entry logic
        ws.symbols.add_entries(&[ext, base]);

        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        C: Record "Customer";
    begin
    end;
}"#
            .to_string(),
        );
        // Position on "Customer" — line 4, character 24
        let pos = super::super::Position {
            line: 4,
            character: 24,
        };
        let result = hover(&ws, &uri, pos);
        assert!(result.is_some(), "Customer should return hover");
        let content = result.unwrap().contents;
        assert!(
            content.contains("Base Application"),
            "should prefer base object from Base Application, not the extension"
        );
    }

    // --- global built-in functions ---

    #[test]
    fn hover_message_builtin_returns_description() {
        let ws = crate::workspace::Workspace::new();
        let uri = url::Url::parse("file:///test/src/Test.al").unwrap();
        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin
        Message('Hello');
    end;
}"#
            .to_string(),
        );
        // Position on "Message" — line 4, character 8
        let pos = super::super::Position {
            line: 4,
            character: 8,
        };
        let result = hover(&ws, &uri, pos);
        assert!(result.is_some(), "Message should return hover");
        let content = result.unwrap().contents;
        assert!(
            content.contains("Message"),
            "content should contain function name"
        );
        // Either from semantic cache (when loaded) or from global builtin fallback
        assert!(
            content.contains("dialog")
                || content.contains("message")
                || content.contains("Message"),
            "content should describe Message"
        );
    }

    #[test]
    fn hover_error_builtin_returns_description() {
        let ws = crate::workspace::Workspace::new();
        let uri = url::Url::parse("file:///test/src/Test.al").unwrap();
        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin
        Error('Oops');
    end;
}"#
            .to_string(),
        );
        // Position on "Error" — line 4, character 8
        let pos = super::super::Position {
            line: 4,
            character: 8,
        };
        let result = hover(&ws, &uri, pos);
        assert!(result.is_some(), "Error should return hover");
        let content = result.unwrap().contents;
        assert!(
            content.contains("Error"),
            "content should contain function name"
        );
    }

    // --- enum value lookup ---

    #[test]
    fn hover_enum_value_lookup() {
        let ws = crate::workspace::Workspace::new();
        let uri = url::Url::parse("file:///test/src/Test.al").unwrap();

        // Add an enum with values to the symbol index
        let enum_entry = al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Enum,
            id: 50100,
            name: "Test Status".to_string(),
            package: "My App".to_string(),
            extends: None,
            implements: vec![],
            namespace: String::new(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![
                al_symbols::EnumValueSymbol {
                    ordinal: 0,
                    name: "Open".to_string(),
                },
                al_symbols::EnumValueSymbol {
                    ordinal: 1,
                    name: "Released".to_string(),
                },
            ],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        ws.symbols.add_entries(&[enum_entry]);

        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        Status: Enum "Test Status";
    begin
        Status := Status::Open;
    end;
}"#
            .to_string(),
        );
        // Position on the standalone "Open" part — line 6, character 25
        // The access path resolver handles Enum::Value — this tests direct enum value hover
        // via the enum value lookup path (step 6)

        // Test hover_enum_value directly
        let result = hover_enum_value(&ws, "Open");
        assert!(result.is_some(), "Open should be found as enum value");
        let content = result.unwrap();
        assert!(
            content.contains("Open"),
            "content should contain the value name"
        );
        assert!(
            content.contains("Test Status"),
            "content should mention the enum"
        );
        assert!(content.contains("ordinal"), "content should show ordinal");
    }

    // --- attribute_description ---

    #[test]
    fn attribute_description_known_attributes() {
        assert!(attribute_description("IntegrationEvent").contains("integration event"));
        assert!(attribute_description("BusinessEvent").contains("business event"));
        assert!(attribute_description("EventSubscriber").contains("subscriber"));
        assert!(attribute_description("Test").contains("test"));
        assert!(attribute_description("TryFunction").contains("try"));
    }

    #[test]
    fn attribute_description_unknown_returns_generic() {
        let desc = attribute_description("SomeUnknownAttribute");
        assert!(
            !desc.is_empty(),
            "unknown attribute should return non-empty description"
        );
        assert!(
            desc.contains("attribute"),
            "generic description should mention attribute"
        );
    }

    // --- hover_global_builtin ---

    #[test]
    fn global_builtin_known_functions() {
        assert!(hover_global_builtin("Message").is_some());
        assert!(hover_global_builtin("Error").is_some());
        assert!(hover_global_builtin("Confirm").is_some());
        assert!(hover_global_builtin("StrSubstNo").is_some());
        assert!(hover_global_builtin("Format").is_some());
        assert!(hover_global_builtin("Today").is_some());
        assert!(hover_global_builtin("CreateGuid").is_some());
    }

    #[test]
    fn global_builtin_unknown_returns_none() {
        assert!(hover_global_builtin("SomeRandomIdentifier").is_none());
        assert!(hover_global_builtin("MyCustomProcedure").is_none());
    }

    #[test]
    fn global_builtin_content_includes_signature_and_doc() {
        let result = hover_global_builtin("Message").unwrap();
        assert!(result.contains("Message("), "should include signature");
        assert!(
            result.contains("global built-in"),
            "should mark as global built-in"
        );
    }
}
