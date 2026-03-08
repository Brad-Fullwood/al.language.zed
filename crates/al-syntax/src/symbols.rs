//! Document symbol extraction from tree-sitter trees.

#[allow(deprecated)]
use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};
use tree_sitter::{Node, Tree};

use crate::ts_range_to_lsp;

/// Extract document symbols for the outline view.
///
/// Produces a hierarchical symbol tree:
/// - Top-level object (table, page, codeunit, etc.)
///   - Procedures and triggers
///   - Fields (for tables)
///   - Controls and actions (for pages)
///   - Data items (for reports)
///   - Keys
///   - Enum values
pub fn extract_document_symbols(tree: &Tree, text: &str) -> Vec<DocumentSymbol> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut symbols = Vec::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "object_declaration" => {
                if let Some(sym) = extract_object_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "namespace_or_using_declaration" => {
                if let Some(sym) = extract_namespace_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
    }

    symbols
}

/// Map an AL object kind node to an LSP SymbolKind.
fn object_kind_to_symbol_kind(kind: &str) -> SymbolKind {
    match kind {
        "kw_table" | "kw_tableextension" => SymbolKind::STRUCT,
        "kw_page" | "kw_pageextension" | "kw_pagecustomization" => SymbolKind::CLASS,
        "kw_codeunit" => SymbolKind::MODULE,
        "kw_report" | "kw_reportextension" => SymbolKind::FILE,
        "kw_query" => SymbolKind::INTERFACE,
        "kw_xmlport" => SymbolKind::INTERFACE,
        "kw_enum" | "kw_enumextension" => SymbolKind::ENUM,
        "kw_interface" => SymbolKind::INTERFACE,
        "kw_permissionset" | "kw_permissionsetextension" => SymbolKind::NAMESPACE,
        "kw_profile" | "kw_profileextension" => SymbolKind::NAMESPACE,
        "kw_controladdin" => SymbolKind::CLASS,
        "kw_entitlement" => SymbolKind::NAMESPACE,
        "kw_dotnet" => SymbolKind::NAMESPACE,
        _ => SymbolKind::OBJECT,
    }
}

/// Extract the object kind as a human-readable string.
fn object_kind_display(kind: &str) -> &str {
    match kind {
        "kw_table" => "table",
        "kw_tableextension" => "tableextension",
        "kw_page" => "page",
        "kw_pageextension" => "pageextension",
        "kw_pagecustomization" => "pagecustomization",
        "kw_codeunit" => "codeunit",
        "kw_report" => "report",
        "kw_reportextension" => "reportextension",
        "kw_query" => "query",
        "kw_xmlport" => "xmlport",
        "kw_enum" => "enum",
        "kw_enumextension" => "enumextension",
        "kw_interface" => "interface",
        "kw_permissionset" => "permissionset",
        "kw_permissionsetextension" => "permissionsetextension",
        "kw_profile" => "profile",
        "kw_profileextension" => "profileextension",
        "kw_controladdin" => "controladdin",
        "kw_entitlement" => "entitlement",
        "kw_dotnet" => "dotnet",
        other => other,
    }
}

#[allow(deprecated)]
fn extract_object_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let kind_node = node.child_by_field_name("kind")?;
    let kind_str = kind_node.kind();
    let sym_kind = object_kind_to_symbol_kind(kind_str);

    // Grammar doesn't assign a field name to the object name,
    // so we look for identifier/quoted_identifier/string children
    let mut name = "(unnamed)".to_string();
    let mut name_node_range = None;
    {
        let mut obj_cursor = node.walk();
        for c in node.children(&mut obj_cursor) {
            match c.kind() {
                "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
                    if let Ok(n) = c.utf8_text(source) {
                        let trimmed = n.trim_matches('"');
                        if !trimmed.is_empty() {
                            name = trimmed.to_string();
                            name_node_range = Some(c.range());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let id_text = node
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let detail = if !id_text.is_empty() {
        Some(format!("{} {}", object_kind_display(kind_str), id_text))
    } else {
        Some(object_kind_display(kind_str).to_string())
    };

    let range = ts_range_to_lsp(&node.range());

    // Selection range is the name node or kind node
    let selection_range = name_node_range
        .map(|r| ts_range_to_lsp(&r))
        .unwrap_or(ts_range_to_lsp(&kind_node.range()));

    // Extract children from the object body
    let mut children = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        extract_body_children(body, source, &mut children);
    }

    Some(DocumentSymbol {
        name,
        detail,
        kind: sym_kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: if children.is_empty() { None } else { Some(children) },
    })
}

#[allow(deprecated)]
fn extract_namespace_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unknown)")
        .to_string();

    let keyword = node
        .child_by_field_name("keyword")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("using");

    let range = ts_range_to_lsp(&node.range());

    Some(DocumentSymbol {
        name,
        detail: Some(keyword.to_string()),
        kind: SymbolKind::NAMESPACE,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    })
}

/// Extract children symbols from an object body.
#[allow(deprecated)]
fn extract_body_children(body: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "procedure_declaration" | "event_procedure_declaration" => {
                if let Some(sym) = extract_procedure_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "trigger_declaration" => {
                if let Some(sym) = extract_trigger_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "event_declaration" => {
                if let Some(sym) = extract_event_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "object_section" => {
                if let Some(sym) = extract_section_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "enum_value_declaration" => {
                if let Some(sym) = extract_enum_value_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "key_declaration" => {
                if let Some(sym) = extract_key_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "object_var_section" | "var_section" => {
                extract_var_section_children(child, source, symbols);
            }
            _ => {}
        }
    }
}

#[allow(deprecated)]
fn extract_procedure_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"')
        .to_string();

    let params = node
        .child_by_field_name("parameters")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("()");

    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|n| n.utf8_text(source).ok());

    let detail = if let Some(rt) = return_type {
        Some(format!("{}: {}", params, rt))
    } else {
        Some(params.to_string())
    };

    let range = ts_range_to_lsp(&node.range());
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range()))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail,
        kind: SymbolKind::FUNCTION,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    })
}

#[allow(deprecated)]
fn extract_trigger_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"')
        .to_string();

    let range = ts_range_to_lsp(&node.range());
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range()))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail: Some("trigger".to_string()),
        kind: SymbolKind::EVENT,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    })
}

#[allow(deprecated)]
fn extract_event_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"')
        .to_string();

    let range = ts_range_to_lsp(&node.range());
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range()))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail: Some("event".to_string()),
        kind: SymbolKind::EVENT,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    })
}

#[allow(deprecated)]
fn extract_section_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let keyword = node
        .child_by_field_name("keyword")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("section")
        .to_string();

    let sym_kind = match keyword.to_lowercase().as_str() {
        "fields" => SymbolKind::STRUCT,
        "keys" => SymbolKind::KEY,
        "actions" => SymbolKind::NAMESPACE,
        "layout" => SymbolKind::NAMESPACE,
        "views" => SymbolKind::NAMESPACE,
        "dataset" => SymbolKind::NAMESPACE,
        "requestpage" => SymbolKind::CLASS,
        "rendering" => SymbolKind::NAMESPACE,
        "fieldgroups" => SymbolKind::STRUCT,
        _ => SymbolKind::NAMESPACE,
    };

    let range = ts_range_to_lsp(&node.range());
    let selection_range = node
        .child_by_field_name("keyword")
        .map(|n| ts_range_to_lsp(&n.range()))
        .unwrap_or(range);

    // Extract children from section body
    let mut children = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        extract_section_body_children(body, source, &mut children);
    }

    Some(DocumentSymbol {
        name: keyword,
        detail: None,
        kind: sym_kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: if children.is_empty() { None } else { Some(children) },
    })
}

/// Extract children from braced blocks within sections (fields, keys, etc.)
#[allow(deprecated)]
fn extract_section_body_children(body: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "object_section" => {
                if let Some(sym) = extract_section_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "procedure_declaration" | "event_procedure_declaration" => {
                if let Some(sym) = extract_procedure_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "trigger_declaration" => {
                if let Some(sym) = extract_trigger_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "key_declaration" => {
                if let Some(sym) = extract_key_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "enum_value_declaration" => {
                if let Some(sym) = extract_enum_value_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "braced_block" => {
                // Recurse into nested braced blocks (common in page layouts)
                extract_section_body_children(child, source, symbols);
            }
            _ => {}
        }
    }
}

#[allow(deprecated)]
fn extract_enum_value_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"')
        .to_string();

    let id = node
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let range = ts_range_to_lsp(&node.range());
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range()))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail: Some(format!("value({})", id)),
        kind: SymbolKind::ENUM_MEMBER,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    })
}

#[allow(deprecated)]
fn extract_key_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"')
        .to_string();

    let fields = node
        .child_by_field_name("fields")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let range = ts_range_to_lsp(&node.range());
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range()))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail: if fields.is_empty() { None } else { Some(fields.to_string()) },
        kind: SymbolKind::KEY,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    })
}

/// Extract variable symbols from a var section.
#[allow(deprecated)]
fn extract_var_section_children(node: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declaration"
            || child.kind() == "regular_variable_declaration"
            || child.kind() == "label_declaration"
            || child.kind() == "object_variable_declaration"
        {
            let name = child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("(unnamed)")
                .trim_matches('"')
                .to_string();

            let type_name = child
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("");

            let range = ts_range_to_lsp(&child.range());

            symbols.push(DocumentSymbol {
                name,
                detail: Some(type_name.to_string()),
                kind: SymbolKind::VARIABLE,
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlParser;

    #[test]
    fn test_extract_symbols_codeunit() {
        let src = r#"codeunit 50100 "My Codeunit"
{
    procedure DoSomething()
    begin
    end;

    procedure DoAnother(x: Integer): Boolean
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);
        assert_eq!(symbols.len(), 1, "Should have one top-level object");
        let obj = &symbols[0];
        assert_eq!(obj.name, "My Codeunit");
        assert_eq!(obj.kind, SymbolKind::MODULE);
        let children = obj.children.as_ref().expect("Should have children");
        assert!(children.len() >= 2, "Should have at least 2 procedures, got {}", children.len());
    }

    #[test]
    fn test_extract_symbols_enum() {
        let src = r#"enum 50100 "My Enum"
{
    value(0; "First") { }
    value(1; "Second") { }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);
        assert_eq!(symbols.len(), 1);
        let obj = &symbols[0];
        assert_eq!(obj.kind, SymbolKind::ENUM);
        let children = obj.children.as_ref().expect("Should have enum values");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].kind, SymbolKind::ENUM_MEMBER);
    }

    #[test]
    fn test_extract_symbols_table() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
    }

    trigger OnInsert()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);
        assert_eq!(symbols.len(), 1);
        let obj = &symbols[0];
        assert_eq!(obj.name, "My Table");
        assert_eq!(obj.kind, SymbolKind::STRUCT);
    }
}
