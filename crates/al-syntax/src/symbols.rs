//! Document symbol extraction from tree-sitter trees.

#[allow(deprecated)]
use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};
use tracing::debug;
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

    debug!(total = symbols.len(), "extract_document_symbols: complete");
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
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
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
            "variable_declaration" | "label_declaration" | "object_variable_declaration" => {
                collect_var_symbols_recursive(child, source, symbols);
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

    if name == "(unnamed)" {
        debug!(
            node_kind = node.kind(),
            line = node.start_position().row,
            "extract_procedure_symbol: unnamed procedure (possible grammar issue)"
        );
    }

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
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    })
}

/// Page control keywords that appear as metadata_keyword nodes in the grammar.
/// These produce a pattern: metadata_keyword + parenthesized_block + braced_block
const PAGE_CONTROL_KEYWORDS: &[&str] = &[
    "area", "group", "repeater", "field", "part", "action", "separator",
    "cuegroup", "grid", "fixed", "usercontrol", "label", "dataitem",
    "column", "filter", "addfirst", "addlast", "addafter", "addbefore",
    "modify", "moveafter", "movebefore", "actionref",
];

fn control_keyword_to_symbol_kind(keyword: &str) -> SymbolKind {
    match keyword {
        "field" | "column" | "filter" => SymbolKind::FIELD,
        "action" | "actionref" | "separator" => SymbolKind::EVENT,
        "area" | "group" | "repeater" | "cuegroup" | "grid" | "fixed" => SymbolKind::STRUCT,
        "part" | "usercontrol" => SymbolKind::CLASS,
        "dataitem" => SymbolKind::STRUCT,
        "label" => SymbolKind::CONSTANT,
        _ => SymbolKind::NAMESPACE,
    }
}

/// Extract children from braced blocks within sections (fields, keys, etc.)
///
/// Page controls in the grammar appear as sibling sequences:
///   metadata_keyword ("area") + parenthesized_block ("(Content)") + braced_block ("{ ... }")
/// Uses next_sibling() for zero-allocation look-ahead instead of collecting all children.
#[allow(deprecated)]
fn extract_section_body_children(body: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    let mut cursor = body.walk();
    if !cursor.goto_first_child() { return; }

    loop {
        let child = cursor.node();
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
            "metadata_keyword" => {
                if let Some(sym) = try_extract_page_control(child, source) {
                    // Skip siblings consumed by the page control (paren + braced_block)
                    loop {
                        if !cursor.goto_next_sibling() { break; }
                        if cursor.node().kind() == "braced_block" {
                            // consumed the body — advance past it
                            break;
                        }
                    }
                    symbols.push(sym);
                }
            }
            "braced_block" => {
                extract_section_body_children(child, source, symbols);
            }
            _ => {}
        }
        if !cursor.goto_next_sibling() { break; }
    }
}

/// Try to extract a page control symbol from a metadata_keyword node.
/// Looks ahead at next_sibling() for parenthesized_block and braced_block.
#[allow(deprecated)]
fn try_extract_page_control(kw_node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let kw_text = kw_node.utf8_text(source).ok()?;

    if !PAGE_CONTROL_KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(kw_text)) {
        return None;
    }

    // Walk next siblings to find parenthesized_block and braced_block
    let mut paren_node = None;
    let mut body_node = None;
    let mut sibling = kw_node.next_sibling();
    while let Some(sib) = sibling {
        match sib.kind() {
            "parenthesized_block" if paren_node.is_none() => paren_node = Some(sib),
            "braced_block" => { body_node = Some(sib); break; }
            "semicolon" => {}
            _ => break,
        }
        sibling = sib.next_sibling();
    }

    // Extract the control name from the parenthesized_block
    let name = if let Some(paren) = paren_node {
        extract_control_name(paren, source)
    } else {
        kw_text.to_string()
    };

    let sym_kind = control_keyword_to_symbol_kind(kw_text);

    // Compute range from keyword start to body end (or paren end if no body)
    let end_node = body_node.or(paren_node).unwrap_or(kw_node);
    let range = tower_lsp::lsp_types::Range {
        start: tower_lsp::lsp_types::Position {
            line: kw_node.start_position().row as u32,
            character: kw_node.start_position().column as u32,
        },
        end: tower_lsp::lsp_types::Position {
            line: end_node.end_position().row as u32,
            character: end_node.end_position().column as u32,
        },
    };

    let selection_range = paren_node
        .map(|p| ts_range_to_lsp(&p.range()))
        .unwrap_or(ts_range_to_lsp(&kw_node.range()));

    // Extract children from the body braced_block
    let mut nested = Vec::new();
    if let Some(body) = body_node {
        extract_section_body_children(body, source, &mut nested);
    }

    Some(DocumentSymbol {
        name,
        detail: Some(kw_text.to_string()),
        kind: sym_kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: if nested.is_empty() { None } else { Some(nested) },
    })
}

/// Extract the control name from a parenthesized block like (Content), (Records), ("Entry No."; Rec."Entry No.").
fn extract_control_name(paren: Node, source: &[u8]) -> String {
    let mut cursor = paren.walk();
    for child in paren.children(&mut cursor) {
        match child.kind() {
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
                if let Ok(text) = child.utf8_text(source) {
                    return text.trim_matches('"').to_string();
                }
            }
            _ => {}
        }
    }
    // Fallback: show the full paren text without parens
    paren.utf8_text(source)
        .map(|t| t.trim_matches(|c| c == '(' || c == ')').trim().to_string())
        .unwrap_or_default()
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
        detail: if fields.is_empty() {
            None
        } else {
            Some(fields.to_string())
        },
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
    collect_var_symbols_recursive(node, source, symbols);
    collect_label_symbols_from_text(node, source, symbols);
}

#[allow(deprecated)]
fn collect_var_symbols_recursive(node: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    match node.kind() {
        "regular_variable_declaration" => {
            let detail = extract_node_text(node.child_by_field_name("type"), source);
            let range = ts_range_to_lsp(&node.range());
            for (name, selection_range) in extract_regular_variable_names(node, source) {
                symbols.push(DocumentSymbol {
                    name,
                    detail: detail.clone(),
                    kind: SymbolKind::VARIABLE,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range,
                    children: None,
                });
            }
            return;
        }
        "label_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let Some(name) = clean_node_text(name_node, source) else {
                return;
            };
            let detail = extract_node_text(node.child_by_field_name("type"), source);
            symbols.push(DocumentSymbol {
                name,
                detail,
                kind: SymbolKind::VARIABLE,
                tags: None,
                deprecated: None,
                range: ts_range_to_lsp(&node.range()),
                selection_range: ts_range_to_lsp(&name_node.range()),
                children: None,
            });
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_var_symbols_recursive(child, source, symbols);
    }
}

fn extract_regular_variable_names(
    node: Node,
    source: &[u8],
) -> Vec<(String, tower_lsp::lsp_types::Range)> {
    let Some(sep_start) = node.child_by_field_name("sep").map(|sep| sep.start_byte()) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    collect_variable_name_nodes(node, sep_start, source, &mut names);

    if names.is_empty() {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Some(name) = clean_node_text(name_node, source) {
                names.push((name, ts_range_to_lsp(&name_node.range())));
            }
        }
    }

    names
}

fn collect_variable_name_nodes(
    node: Node,
    sep_start: usize,
    source: &[u8],
    names: &mut Vec<(String, tower_lsp::lsp_types::Range)>,
) {
    if node.start_byte() >= sep_start {
        return;
    }

    if node.child_count() == 0 && is_variable_name_node(node.kind()) {
        if let Some(name) = clean_node_text(node, source) {
            names.push((name, ts_range_to_lsp(&node.range())));
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_variable_name_nodes(child, sep_start, source, names);
    }
}

fn is_variable_name_node(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "quoted_identifier"
            | "keyword"
            | "object_keyword"
            | "metadata_keyword"
            | "property_keyword"
            | "kw_function"
    )
}

fn extract_node_text(node: Option<Node>, source: &[u8]) -> Option<String> {
    let node = node?;
    let text = node.utf8_text(source).ok()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn clean_node_text(node: Node, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let clean = text.trim_matches('"').trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

#[allow(deprecated)]
fn collect_label_symbols_from_text(node: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    let Ok(section_text) = node.utf8_text(source) else {
        return;
    };

    let mut seen: std::collections::HashSet<String> = symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::VARIABLE)
        .map(|symbol| symbol.name.to_lowercase())
        .collect();

    for (offset, line) in section_text.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.ends_with(';') {
            continue;
        }
        let Some((name_part, rest)) = trimmed.split_once(':') else {
            continue;
        };
        if !rest.trim_start().starts_with("Label ") && !rest.trim_start().starts_with("Label\t") {
            continue;
        }

        let name = name_part.trim().trim_matches('"').to_string();
        if name.is_empty() || !seen.insert(name.to_lowercase()) {
            continue;
        }

        let line_no = node.start_position().row as u32 + offset as u32;
        let start_col = line.find(name_part).unwrap_or_default() as u32;
        let end_col = start_col + name_part.len() as u32;
        symbols.push(DocumentSymbol {
            name,
            detail: Some("Label".to_string()),
            kind: SymbolKind::VARIABLE,
            tags: None,
            deprecated: None,
            range: ts_range_to_lsp(&node.range()),
            selection_range: tower_lsp::lsp_types::Range {
                start: tower_lsp::lsp_types::Position {
                    line: line_no,
                    character: start_col,
                },
                end: tower_lsp::lsp_types::Position {
                    line: line_no,
                    character: end_col,
                },
            },
            children: None,
        });
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
        assert!(
            children.len() >= 2,
            "Should have at least 2 procedures, got {}",
            children.len()
        );
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

    #[test]
    fn test_extract_symbols_global_variables_keep_names_and_types() {
        let src = r#"codeunit 50100 Test
{
    var
        FirstVar, "Second Var": Record "Sales Header" temporary;
        CaptionLbl: Label 'Caption';

    procedure DoIt()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);
        let obj = &symbols[0];
        let children = obj.children.as_ref().expect("Should have object children");

        let vars: Vec<&DocumentSymbol> = children
            .iter()
            .filter(|child| child.kind == SymbolKind::VARIABLE)
            .collect();

        assert_eq!(vars.len(), 3, "Expected one symbol per declared variable");
        assert_eq!(vars[0].name, "FirstVar");
        assert_eq!(
            vars[0].detail.as_deref(),
            Some(r#"Record "Sales Header" temporary"#)
        );
        assert_eq!(vars[1].name, "Second Var");
        assert_eq!(
            vars[1].detail.as_deref(),
            Some(r#"Record "Sales Header" temporary"#)
        );
        assert_eq!(vars[2].name, "CaptionLbl");
        assert_eq!(vars[2].detail.as_deref(), Some("Label"));
        assert!(vars.iter().all(|var| var.name != "(unnamed)"));
    }
}
