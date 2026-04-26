//! AST navigation helpers for AL tree-sitter trees.

use crate::traversal::walk_tree;
pub use crate::types::SyntaxPosition as Position;
use tree_sitter::{Node, Tree};

/// Find the most specific node at a given LSP position.
///
/// `pos.character` is a UTF-16 code unit offset, while tree-sitter's
/// `Point.column` is a byte offset. The conversion requires the source
/// line, so the caller must pass the parsed source text along with the
/// tree. ASCII-only lines are equal in both representations; lines with
/// non-ASCII characters need the conversion to land on the correct node.
pub fn find_node_at_position<'a>(tree: &'a Tree, source: &str, pos: Position) -> Option<Node<'a>> {
    let row = pos.line as usize;
    let line = source.lines().nth(row).unwrap_or("");
    let column = crate::utf16_col_to_byte_offset(line, pos.character as usize);
    let point = tree_sitter::Point { row, column };
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

impl std::fmt::Display for ParameterInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_var {
            write!(f, "var ")?;
        }
        write!(f, "{}: {}", self.name, self.type_name)
    }
}

/// Return true if the given node kind is a recognized AL object type keyword.
///
/// Uses the data-driven token_classification lookup instead of a hardcoded list.
fn is_object_type_kind(kind: &str) -> bool {
    if kind == "object_keyword" {
        return true;
    }
    crate::language_data::is_object_keyword_node(kind)
}

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
                    kind_str = kind_str
                        .strip_prefix("kw_")
                        .unwrap_or(&kind_str)
                        .to_string();
                }
            }

            // Extract id from the 'id' field
            if let Some(id_node) = child.child_by_field_name("id") {
                if let Ok(id_text) = id_node.utf8_text(source) {
                    id = id_text.parse::<i64>().ok();
                }
            }

            // Extract name — grammar doesn't assign a field name to the object name,
            // so we use the shared extract_object_name helper.
            if let Some(n) = crate::extract_object_name(child, source) {
                name = n;
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
    if !is_object_type_kind(&kind) && kind != "object_declaration" {
        return None;
    }

    let mut id = None;
    let name = crate::extract_object_name(child, source).unwrap_or_default();

    for i in 0..child.child_count() {
        let c = match child.child(i) {
            Some(c) => c,
            None => continue,
        };
        if c.kind() == "integer" {
            if let Ok(n) = c.utf8_text(source).unwrap_or("0").parse::<i64>() {
                id = Some(n);
            }
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
    let node = find_node_at_position(tree, text, pos)?;
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
    find_refs_iterative(root, source, name, &mut refs);
    refs
}

fn find_refs_iterative(
    root: Node,
    source: &[u8],
    target_name: &str,
    refs: &mut Vec<tree_sitter::Range>,
) {
    walk_tree(root, &mut |node| {
        // Check if this node is an identifier matching the target name
        if matches!(node.kind(), "identifier" | "quoted_identifier" | "name") {
            if let Ok(text) = node.utf8_text(source) {
                let text_clean = text.trim_matches('"');
                if text_clean.eq_ignore_ascii_case(target_name) {
                    refs.push(node.range());
                }
            }
        }
    });
}

/// Count call-site references to a procedure name within the tree.
///
/// Unlike `find_variable_references`, this function is context-aware. It only
/// counts identifier nodes that appear in actual call positions:
///
/// - Bare call: `ProcName()` — identifier is the sole child of `primary_expression`
///   whose sibling in `postfix_expression` is a `call_suffix`.
/// - Member call: `Obj.ProcName()` — identifier is the `member` field of a
///   `member_call_suffix` node.
/// - Scope call: `Enum::Value` — identifier is the `member` field of a
///   `scope_call_suffix` node.
///
/// Specifically excluded:
/// - `Rec.Name` (field access via `member_suffix`, not `member_call_suffix`)
/// - The `name` node inside `procedure_declaration` (the declaration itself)
/// - Variable declarations, parameter lists, type references, etc.
pub fn find_call_references(tree: &Tree, text: &str, name: &str) -> usize {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut count = 0usize;
    count_call_refs_iterative(root, source, name, &mut count);
    count
}

/// Count call-site references to `target_name` in the AST rooted at `root`.
///
/// The `_recursive` suffix is historical — the actual traversal is iterative
/// (delegated to `walk_tree`) and bounded by the AST depth. AL parse trees
/// reach at most ~30 levels deep for procedures + nested expressions, so
/// stack usage is constant w.r.t. document size.
fn count_call_refs_iterative(root: Node, source: &[u8], target_name: &str, count: &mut usize) {
    walk_tree(root, &mut |node| {
        // Check if this node is an identifier matching the target name
        if matches!(node.kind(), "identifier" | "quoted_identifier") {
            if let Ok(text) = node.utf8_text(source) {
                let text_clean = text.trim_matches('"');
                if text_clean.eq_ignore_ascii_case(target_name) && is_call_reference(node, source) {
                    *count += 1;
                }
            }
        }
    });
}

/// Determine whether an identifier node is a call-site reference.
///
/// Walks the ancestor chain to classify the context.
fn is_call_reference(node: Node, _source: &[u8]) -> bool {
    // node: identifier (or quoted_identifier)
    // parent chain: identifier -> name -> ...
    //
    // Case 1: bare call DoSomething()
    //   identifier -> name -> primary_expression -> postfix_expression (has call_suffix sibling)
    //
    // Case 2: member call cu.DoSomething()
    //   identifier -> name -> member_call_suffix (field="member")
    //
    // Case 3: scope call Codeunit::DoSomething()
    //   identifier -> name -> scope_call_suffix (field="member")
    //
    // NOT a call: field access Rec.Name
    //   identifier -> name -> member_suffix (field="member")
    //
    // NOT a call: procedure declaration
    //   identifier -> name -> procedure_declaration (field="name")

    let Some(name_node) = node.parent() else {
        return false;
    };
    // The immediate parent should be a `name` node (or could be directly in member_call_suffix)
    let parent = if name_node.kind() == "name" {
        let Some(p) = name_node.parent() else {
            return false;
        };
        p
    } else {
        name_node
    };

    match parent.kind() {
        // Bare call: primary_expression
        "primary_expression" => {
            // The primary_expression must be a child of postfix_expression,
            // and that postfix_expression must also have a call_suffix child.
            let Some(postfix) = parent.parent() else {
                return false;
            };
            if postfix.kind() != "postfix_expression" {
                return false;
            }
            let mut cursor = postfix.walk();
            let has_call_suffix = postfix
                .children(&mut cursor)
                .any(|c| c.kind() == "call_suffix");
            has_call_suffix
        }
        // Member call: cu.ProcName()
        "member_call_suffix" => {
            // Verify this name is the "member" field (not some other child)
            let field = get_field_name_of_child(parent, name_node);
            field.as_deref() == Some("member")
        }
        // Scope call: Codeunit::ProcName()
        "scope_call_suffix" => {
            let field = get_field_name_of_child(parent, name_node);
            field.as_deref() == Some("member")
        }
        // Declaration: procedure ProcName() — not a call
        "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration" => false,
        // Everything else: not a recognized call context
        _ => false,
    }
}

/// Return the field name that `child` has within `parent`, if any.
fn get_field_name_of_child(parent: Node, child: Node) -> Option<String> {
    let child_id = child.id();
    for i in 0..parent.child_count() {
        if let Some(c) = parent.child(i) {
            if c.id() == child_id {
                return parent.field_name_for_child(i as u32).map(|s| s.to_string());
            }
        }
    }
    None
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
            eprintln!(
                "{}{} [{}] field={:?} text={:?}",
                indent,
                node.kind(),
                node.id(),
                node.parent().and_then(|p| {
                    (0..p.child_count()).find_map(|i| {
                        p.field_name_for_child(i as u32)
                            .filter(|_| p.child(i).map(|c| c.id()) == Some(node.id()))
                    })
                }),
                short
            );
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
        assert!(
            refs.len() >= 2,
            "Expected at least 2 references, got {}",
            refs.len()
        );
    }

    #[test]
    fn test_find_node_at_position() {
        let src = r#"codeunit 50100 Test
{
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let node = find_node_at_position(
            &result.tree,
            src,
            Position {
                line: 0,
                character: 0,
            },
        );
        assert!(node.is_some());
    }

    /// Reproduces tb-003 / 0a40a392e926a846: find_node_at_position must convert
    /// the LSP UTF-16 column to a byte column before constructing the
    /// tree-sitter Point. The fixture contains a non-ASCII identifier (`Ø`) so
    /// the wrong column would land on the wrong node.
    #[test]
    fn test_find_node_at_position_utf16_after_multibyte() {
        let src = "codeunit 50100 Test\n{\n    var\n        ØreName: Text;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        // UTF-16 column 8 is the start of `Ø` (after 8 leading spaces).
        let pos = Position {
            line: 3,
            character: 8,
        };
        let node = find_node_at_position(&result.tree, src, pos)
            .expect("should resolve to a node at the Ø identifier position");
        let bytes = src.as_bytes();
        let txt = std::str::from_utf8(&bytes[node.byte_range()]).unwrap_or("");
        assert!(
            txt.contains('Ø') || node.kind().contains("identifier"),
            "expected to land on the ØreName identifier, got node kind={} text={:?}",
            node.kind(),
            txt
        );
    }

    #[test]
    fn test_find_node_at_position_out_of_range() {
        let mut parser = AlParser::new();
        let result = parser.parse("codeunit 50100 Test { }");
        let pos = Position {
            line: 999,
            character: 0,
        };
        let node = find_node_at_position(&result.tree, "codeunit 50100 Test { }", pos);
        // Out-of-range position should not panic; tree-sitter clamps to nearest node
        assert!(
            node.is_some(),
            "tree-sitter returns the nearest node for out-of-range positions"
        );
    }

    #[test]
    fn test_find_object_declaration_table_with_fields() {
        let mut parser = AlParser::new();
        let source = "table 50100 \"My Table\" { fields { } }";
        let result = parser.parse(source);
        let obj = find_object_declaration(&result.tree, source);
        assert!(obj.is_some(), "Should find table declaration");
        let obj = obj.unwrap();
        assert_eq!(obj.name, "My Table");
    }

    #[test]
    fn test_find_object_declaration_empty() {
        let mut parser = AlParser::new();
        let result = parser.parse("");
        let obj = find_object_declaration(&result.tree, "");
        assert!(obj.is_none());
    }

    #[test]
    fn test_find_variable_references_not_found() {
        let mut parser = AlParser::new();
        let source = "codeunit 50100 Test { procedure DoIt() begin end; }";
        let result = parser.parse(source);
        let refs = find_variable_references(&result.tree, source, "nonExistentVar");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_find_variable_references_case_insensitive() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        myvar: Integer;
    begin
        MYVAR := 42;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let refs = find_variable_references(&result.tree, src, "MyVar");
        // Case-insensitive match should find references
        assert!(
            refs.len() >= 1,
            "Expected at least 1 case-insensitive reference, got {}",
            refs.len()
        );
    }

    #[test]
    fn test_find_procedure_at_outside_proc() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        // Position on the codeunit keyword (outside any procedure)
        let info = find_procedure_at(
            &result.tree,
            src,
            Position {
                line: 0,
                character: 0,
            },
        );
        assert!(
            info.is_none(),
            "Should not find a procedure at the object declaration level"
        );
    }

    #[test]
    fn test_find_procedure_at_inside_proc() {
        let src = r#"codeunit 50100 Test
{
    procedure MyProc()
    begin
        Message('hello');
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        // Position inside the procedure body
        let info = find_procedure_at(
            &result.tree,
            src,
            Position {
                line: 4,
                character: 8,
            },
        );
        assert!(info.is_some(), "Should find procedure at body position");
        let info = info.unwrap();
        assert_eq!(info.name, "MyProc");
    }

    #[test]
    fn test_parameter_info_display() {
        let param = ParameterInfo {
            name: "Input".to_string(),
            type_name: "Text".to_string(),
            is_var: false,
        };
        assert_eq!(format!("{}", param), "Input: Text");

        let var_param = ParameterInfo {
            name: "Output".to_string(),
            type_name: "Integer".to_string(),
            is_var: true,
        };
        assert_eq!(format!("{}", var_param), "var Output: Integer");
    }

    // ── find_call_references tests ──────────────────────────────────────────

    #[test]
    fn test_find_call_references_bare_call() {
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        DoSomething();
    end;

    procedure DoSomething()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let count = find_call_references(&result.tree, src, "DoSomething");
        // One call site; the declaration must NOT be counted
        assert_eq!(count, 1, "Expected 1 call reference, got {}", count);
    }

    #[test]
    fn test_find_call_references_member_call() {
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    var
        cu: Codeunit "Other";
    begin
        cu.DoSomething();
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let count = find_call_references(&result.tree, src, "DoSomething");
        assert_eq!(count, 1, "Expected 1 member call reference, got {}", count);
    }

    #[test]
    fn test_find_call_references_field_access_not_counted() {
        // Rec.Name is a field access (member_suffix), not a call (member_call_suffix).
        // A procedure named "Name" with only field accesses should report 0 call refs.
        let src = r#"codeunit 50100 Test
{
    procedure Name()
    begin
    end;

    procedure Caller()
    begin
        x := Rec.Name;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let count = find_call_references(&result.tree, src, "Name");
        assert_eq!(
            count, 0,
            "Field access Rec.Name must not count as call reference; got {}",
            count
        );
    }

    #[test]
    fn test_find_call_references_declaration_not_counted() {
        let src = r#"codeunit 50100 Test
{
    procedure Init()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let count = find_call_references(&result.tree, src, "Init");
        assert_eq!(
            count, 0,
            "Procedure declaration must not be counted as a call; got {}",
            count
        );
    }

    #[test]
    fn test_find_call_references_common_name_no_false_negatives() {
        // A procedure named "Name" with multiple Rec.Name field accesses must remain
        // detectable as unreferenced — field accesses must not suppress dead code detection.
        let src = r#"codeunit 50100 Test
{
    procedure Name()
    begin
    end;

    procedure Caller()
    begin
        x := Rec.Name;
        y := Rec2.Name;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let count = find_call_references(&result.tree, src, "Name");
        assert_eq!(
            count, 0,
            "Field accesses must not prevent dead code detection; got {}",
            count
        );
    }

    #[test]
    fn test_find_call_references_scope_call() {
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        Codeunit::Run();
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let count = find_call_references(&result.tree, src, "Run");
        assert_eq!(count, 1, "Expected 1 scope call reference, got {}", count);
    }

    #[test]
    fn test_find_call_references_case_insensitive() {
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        DOSOMETHING();
    end;

    procedure DoSomething()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let count = find_call_references(&result.tree, src, "dosomething");
        assert_eq!(
            count, 1,
            "Case-insensitive call reference expected; got {}",
            count
        );
    }
}
