//! AST navigation helpers for AL tree-sitter trees.

pub use tower_lsp::lsp_types::Position;
use tree_sitter::{Node, Tree};

/// Find the most specific node at a given position.
pub fn find_node_at_position(tree: &Tree, pos: Position) -> Option<Node<'_>> {
    let point = tree_sitter::Point {
        row: pos.line as usize,
        column: pos.character as usize,
    };
    let root = tree.root_node();
    root.descendant_for_point_range(point, point)
}

/// Information about an AL object declaration.
#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub kind: String,
    pub id: Option<i64>,
    pub name: String,
    pub range: tree_sitter::Range,
}

/// Information about a procedure.
#[derive(Debug, Clone)]
pub struct ProcedureInfo {
    pub name: String,
    pub range: tree_sitter::Range,
    pub parameters: Vec<ParameterInfo>,
    pub return_type: Option<String>,
    pub is_local: bool,
}

/// Information about a procedure parameter.
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    pub name: String,
    pub type_name: String,
    pub is_var: bool,
}

/// All recognized AL object type node kinds in the tree-sitter grammar.
const OBJECT_TYPE_KINDS: &[&str] = &[
    "kw_table",
    "kw_page",
    "kw_codeunit",
    "kw_report",
    "kw_query",
    "kw_xmlport",
    "kw_enum",
    "kw_interface",
    "kw_permissionset",
    "kw_profile",
    "kw_pagecustomization",
    "kw_controladdin",
    "kw_tableextension",
    "kw_pageextension",
    "kw_reportextension",
    "kw_enumextension",
    "kw_permissionsetextension",
    "kw_entitlement",
    "kw_profileextension",
    "kw_dotnet",
    "object_keyword",
];

/// Find the object declaration in the tree.
///
/// Handles all AL object types: table, page, codeunit, report, query, xmlport,
/// enum, interface, permissionset, profile, pagecustomization, controladdin,
/// tableextension, pageextension, reportextension, enumextension,
/// permissionsetextension, entitlement, profileextension, dotnet.
pub fn find_object_declaration(tree: &Tree, text: &str) -> Option<ObjectInfo> {
    let root = tree.root_node();
    let source = text.as_bytes();

    // Search for object_declaration node
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "object_declaration" {
            let mut kind_str = String::new();
            let mut id = None;
            let mut name = String::new();

            // Extract kind from the 'kind' field
            if let Some(kind_node) = child.child_by_field_name("kind") {
                kind_str = kind_node.kind().to_string();
                // If object_keyword, get the text
                if kind_str == "object_keyword" {
                    if let Ok(t) = kind_node.utf8_text(source) {
                        kind_str = t.to_lowercase();
                    }
                } else {
                    // Strip kw_ prefix
                    kind_str = kind_str.strip_prefix("kw_").unwrap_or(&kind_str).to_string();
                }
            }

            // Extract id from the 'id' field
            if let Some(id_node) = child.child_by_field_name("id") {
                if let Ok(id_text) = id_node.utf8_text(source) {
                    id = id_text.parse::<i64>().ok();
                }
            }

            // Extract name — grammar doesn't assign a field name to the object name,
            // so we look for identifier/quoted_identifier/string children
            let mut obj_cursor = child.walk();
            for c in child.children(&mut obj_cursor) {
                match c.kind() {
                    "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
                        if let Ok(n) = c.utf8_text(source) {
                            let trimmed = n.trim_matches('"');
                            if !trimmed.is_empty() {
                                name = trimmed.to_string();
                            }
                        }
                    }
                    _ => {}
                }
            }

            return Some(ObjectInfo {
                kind: kind_str,
                id,
                name,
                range: child.range(),
            });
        }
    }

    // Fallback: walk root children directly for compatibility
    let child = root.child(0)?;
    let kind = child.kind().to_string();

    // Only accept known object types
    if !OBJECT_TYPE_KINDS.contains(&kind.as_str()) && kind != "object_declaration" {
        return None;
    }

    let mut id = None;
    let mut name = String::new();

    for i in 0..child.child_count() {
        let c = child.child(i).unwrap();
        match c.kind() {
            "integer" => {
                if let Ok(n) = c.utf8_text(source).unwrap_or("0").parse::<i64>() {
                    id = Some(n);
                }
            }
            "identifier" | "string" | "quoted_identifier" | "name" | "name_or_keyword" => {
                let n = c.utf8_text(source).unwrap_or("");
                if !n.is_empty() {
                    name = n.trim_matches('"').to_string();
                }
            }
            _ => {}
        }
    }

    Some(ObjectInfo {
        kind,
        id,
        name,
        range: child.range(),
    })
}

/// Find a procedure at the given position.
pub fn find_procedure_at(tree: &Tree, text: &str, pos: Position) -> Option<ProcedureInfo> {
    let node = find_node_at_position(tree, pos)?;
    let source = text.as_bytes();

    // Walk up to find the procedure/trigger node
    let mut current = node;
    loop {
        if current.kind() == "procedure_declaration" || current.kind() == "trigger_declaration" {
            let name = current
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .trim_matches('"')
                .to_string();

            let parameters = extract_parameters(current, source);
            let return_type = extract_return_type(current, source);
            let is_local = check_is_local(current, source);

            return Some(ProcedureInfo {
                name,
                range: current.range(),
                parameters,
                return_type,
                is_local,
            });
        }
        current = current.parent()?;
    }
}

/// Find all references to a variable by name within the tree.
pub fn find_variable_references(tree: &Tree, text: &str, name: &str) -> Vec<tree_sitter::Range> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut refs = Vec::new();
    find_refs_recursive(root, source, name, &mut refs);
    refs
}

fn find_refs_recursive(
    node: Node,
    source: &[u8],
    target_name: &str,
    refs: &mut Vec<tree_sitter::Range>,
) {
    // Check if this node is an identifier matching the target name
    if matches!(node.kind(), "identifier" | "quoted_identifier" | "name") {
        if let Ok(text) = node.utf8_text(source) {
            let text_clean = text.trim_matches('"');
            if text_clean.eq_ignore_ascii_case(target_name) {
                refs.push(node.range());
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_refs_recursive(child, source, target_name, refs);
    }
}

/// Extract parameters from a procedure/trigger declaration node.
fn extract_parameters(node: Node, source: &[u8]) -> Vec<ParameterInfo> {
    let mut params = Vec::new();

    let param_list = match node.child_by_field_name("parameters") {
        Some(pl) => pl,
        None => return params,
    };

    let mut cursor = param_list.walk();
    for child in param_list.children(&mut cursor) {
        if child.kind() == "parameter" {
            let is_var = {
                let mut pc = child.walk();
                let result = child.children(&mut pc).any(|c| c.kind() == "kw_var");
                result
            };

            let name = child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .trim_matches('"')
                .to_string();

            let type_name = child
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();

            params.push(ParameterInfo {
                name,
                type_name,
                is_var,
            });
        }
    }
    params
}

/// Extract return type from a procedure/trigger declaration.
fn extract_return_type(node: Node, source: &[u8]) -> Option<String> {
    node.child_by_field_name("return_type")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())
}

/// Check if a procedure is local (has `local` modifier).
fn check_is_local(node: Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "member_modifier" || child.kind() == "kw_local" {
            if let Ok(text) = child.utf8_text(source) {
                if text.eq_ignore_ascii_case("local") {
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
    use crate::AlParser;

    #[test]
    fn test_debug_tree_structure() {
        let src = r#"codeunit 50100 "My Codeunit"
{
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let root = result.tree.root_node();
        fn dump(node: tree_sitter::Node, src: &str, depth: usize) {
            let indent = "  ".repeat(depth);
            let text = node.utf8_text(src.as_bytes()).unwrap_or("??");
            let short = if text.len() > 50 { &text[..50] } else { text };
            eprintln!("{}{} [{}] field={:?} text={:?}",
                indent, node.kind(), node.id(),
                node.parent().and_then(|p| {
                    (0..p.child_count()).find_map(|i| {
                        p.field_name_for_child(i as u32).filter(|_| p.child(i).map(|c| c.id()) == Some(node.id()))
                    })
                }),
                short);
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                dump(child, src, depth + 1);
            }
        }
        dump(root, src, 0);
    }

    #[test]
    fn test_find_object_declaration_codeunit() {
        let src = r#"codeunit 50100 "My Codeunit"
{
    procedure DoSomething()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let obj = find_object_declaration(&result.tree, src);
        assert!(obj.is_some());
        let obj = obj.unwrap();
        assert_eq!(obj.kind, "codeunit");
        assert_eq!(obj.id, Some(50100));
        assert_eq!(obj.name, "My Codeunit");
    }

    #[test]
    fn test_find_object_declaration_table() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let obj = find_object_declaration(&result.tree, src);
        assert!(obj.is_some());
        let obj = obj.unwrap();
        assert_eq!(obj.kind, "table");
        assert_eq!(obj.id, Some(50100));
        assert_eq!(obj.name, "My Table");
    }

    #[test]
    fn test_find_variable_references() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        MyVar: Integer;
    begin
        MyVar := 42;
        Message('%1', MyVar);
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let refs = find_variable_references(&result.tree, src, "MyVar");
        // Should find multiple references to MyVar
        assert!(refs.len() >= 2, "Expected at least 2 references, got {}", refs.len());
    }

    #[test]
    fn test_find_node_at_position() {
        let src = r#"codeunit 50100 Test
{
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let node = find_node_at_position(&result.tree, Position { line: 0, character: 0 });
        assert!(node.is_some());
    }
}
