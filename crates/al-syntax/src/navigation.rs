//! AST navigation helpers for AL tree-sitter trees.

use super::traversal::walk_tree;
pub use super::types::SyntaxPosition as Position;
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
    let column = super::utf16_col_to_byte_offset(line, pos.character as usize);
    let point = tree_sitter::Point { row, column };
    let root = tree.root_node();
    root.descendant_for_point_range(point, point)
}

#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub kind: String,
    pub id: Option<i64>,
    pub name: String,
    pub range: tree_sitter::Range,
}

#[derive(Debug, Clone)]
pub struct ProcedureInfo {
    pub name: String,
    pub range: tree_sitter::Range,
    pub parameters: Vec<ParameterInfo>,
    pub return_type: Option<String>,
    pub is_local: bool,
}

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
    super::language_data::is_object_keyword_node(kind)
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

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "object_declaration" {
            let mut kind_str = String::new();
            let mut id = None;
            let mut name = String::new();

            if let Some(kind_node) = child.child_by_field_name("kind") {
                kind_str = kind_node.kind().to_string();
                if kind_str == "object_keyword" {
                    if let Ok(t) = kind_node.utf8_text(source) {
                        kind_str = t.to_lowercase();
                    }
                } else {
                    kind_str = kind_str
                        .strip_prefix("kw_")
                        .unwrap_or(&kind_str)
                        .to_string();
                }
            }

            if let Some(id_node) = child.child_by_field_name("id") {
                if let Ok(id_text) = id_node.utf8_text(source) {
                    id = id_text.parse::<i64>().ok();
                }
            }

            if let Some(n) = super::extract_object_name(child, source) {
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

    // Some grammar variants expose the object type directly at the root.
    let child = root.child(0)?;
    let kind = child.kind().to_string();

    if !is_object_type_kind(&kind) && kind != "object_declaration" {
        return None;
    }

    let mut id = None;
    let name = super::extract_object_name(child, source).unwrap_or_default();

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

pub fn find_procedure_at(tree: &Tree, text: &str, pos: Position) -> Option<ProcedureInfo> {
    let node = find_node_at_position(tree, text, pos)?;
    let source = text.as_bytes();

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

pub fn find_variable_references(tree: &Tree, text: &str, name: &str) -> Vec<tree_sitter::Range> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut refs = Vec::new();
    find_refs_iterative(root, source, name, &mut refs);
    // The AL grammar wraps some identifiers in a `name` node whose span is
    // identical to the inner `identifier`/`quoted_identifier`. Both kinds match
    // the predicate in `find_refs_iterative`, so the same source span was
    // collected twice — double-counting every reference and inflating
    // `rename`'s reported edit count to exactly 2×. A
    // reference is unique by its byte span; drop exact-span duplicates.
    let mut seen = std::collections::HashSet::new();
    refs.retain(|r| seen.insert((r.start_byte, r.end_byte)));
    refs
}

/// Find references to an event raised through `[EventSubscriber(...)]`
/// attributes whose target event name equals `event_name`.
///
/// In AL a subscriber names its target event with a *string literal* — e.g.
/// `[EventSubscriber(ObjectType::Codeunit, "Pub", 'OnFooEvent', '', false, false)]`
/// — not an identifier.
///
/// Scope is deliberately narrow to avoid false positives: only the event-name
/// argument (the 3rd positional argument, mirroring
/// `insight::calls::parse_subscriber_target_from_attrs`) of an `EventSubscriber`
/// attribute is matched, so unrelated string literals that happen to equal
/// `event_name` are never reported.
pub fn find_event_subscriber_references(
    tree: &Tree,
    text: &str,
    event_name: &str,
) -> Vec<tree_sitter::Range> {
    let source = text.as_bytes();
    let mut refs = Vec::new();
    walk_tree(tree.root_node(), &mut |node| {
        if node.kind() != "attribute" {
            return;
        }
        let attr_name = node
            .child_by_field_name("name")
            .or_else(|| node.child(0))
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("");
        if !attr_name.trim().eq_ignore_ascii_case("EventSubscriber") {
            return;
        }
        let mut ac = node.walk();
        let Some(arg_list) = node
            .children(&mut ac)
            .find(|n| n.kind() == "attribute_argument_list")
        else {
            return;
        };
        // The event name is the 3rd positional argument (index 2), matching
        // `parse_subscriber_target_from_attrs` (ObjectType, Object, Event, …).
        let mut lc = arg_list.walk();
        let event_arg = arg_list
            .children(&mut lc)
            .filter(|n| n.kind() == "attribute_argument")
            .nth(2);
        if let Some(arg) = event_arg {
            if let Ok(arg_text) = arg.utf8_text(source) {
                if crate::clean_attr_arg(arg_text).eq_ignore_ascii_case(event_name) {
                    refs.push(arg.range());
                }
            }
        }
    });
    refs
}

fn find_refs_iterative(
    root: Node,
    source: &[u8],
    target_name: &str,
    refs: &mut Vec<tree_sitter::Range>,
) {
    walk_tree(root, &mut |node| {
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

/// Collect every call-site identifier name (lowercased) reachable from `tree`.
///
/// Single-pass companion to `find_call_references`: instead of asking
/// "is this one name called here?" N times, walk the tree once and collect
/// the full set of called names. Call-site classification is the same as
/// `find_call_references` (bare/member/scope calls only — field access is
/// excluded). Names are lowercased so callers can do case-insensitive
/// membership checks without per-query allocation.
///
pub fn collect_call_site_names(tree: &Tree, text: &str) -> std::collections::HashSet<String> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut names = std::collections::HashSet::new();
    walk_tree(root, &mut |node| {
        if matches!(node.kind(), "identifier" | "quoted_identifier") {
            if let Ok(t) = node.utf8_text(source) {
                if is_call_reference(node, source) {
                    names.insert(t.trim_matches('"').to_ascii_lowercase());
                }
            }
        }
    });
    names
}

/// Collect every call-site identifier and its exact syntax range.
///
/// This is the range-preserving companion to [`collect_call_site_names`]. It
/// applies the same call-position filter, so field access, declarations, type
/// references, and variable names are not returned.
pub fn collect_call_sites(tree: &Tree, text: &str) -> Vec<(String, tree_sitter::Range)> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut calls = Vec::new();
    walk_tree(root, &mut |node| {
        if matches!(node.kind(), "identifier" | "quoted_identifier")
            && is_call_reference(node, source)
        {
            if let Ok(name) = node.utf8_text(source) {
                calls.push((name.trim_matches('"').to_ascii_lowercase(), node.range()));
            }
        }
    });
    calls
}

/// Count call-site references to `target_name` in the AST rooted at `root`.
fn count_call_refs_iterative(root: Node, source: &[u8], target_name: &str, count: &mut usize) {
    walk_tree(root, &mut |node| {
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
        "member_call_suffix" => {
            // Verify this name is the "member" field (not some other child)
            let field = get_field_name_of_child(parent, name_node);
            field.as_deref() == Some("member")
        }
        "scope_call_suffix" => {
            let field = get_field_name_of_child(parent, name_node);
            field.as_deref() == Some("member")
        }
        "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration" => false,
        _ => false,
    }
}

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

/// Collect names used as the member portion of an AST member access
/// (`Receiver.Member`). Comments, string literals, declaration headers, and
/// unrelated punctuation never enter the result because only `member_suffix`
/// nodes are inspected.
pub fn collect_member_access_names(tree: &Tree, source: &str) -> std::collections::HashSet<String> {
    let bytes = source.as_bytes();
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "member_suffix" {
            if let Some(member) = node.child_by_field_name("member") {
                if let Some(name) = super::node_text_clean(member, bytes) {
                    names.insert(name.to_ascii_lowercase());
                }
            }
            continue;
        }
        // Some declaration headers (notably page/report `field(...)`
        // arguments) are intentionally represented by the grammar as generic
        // parenthesized blocks rather than expression nodes. Their `Rec.Field`
        // shape is still structural: an operator node containing exactly `.`
        // followed by a name token. Include that form without falling back to
        // raw source scanning.
        if node.kind() == "operator" && node.utf8_text(bytes).ok().map(str::trim) == Some(".") {
            if let Some(member) = node.next_named_sibling() {
                if matches!(
                    member.kind(),
                    "name"
                        | "name_or_keyword"
                        | "identifier"
                        | "quoted_identifier"
                        | "object_keyword"
                        | "type_keyword"
                        | "metadata_keyword"
                        | "property_keyword"
                        | "keyword"
                ) {
                    if let Some(name) = super::node_text_clean(member, bytes) {
                        names.insert(name.to_ascii_lowercase());
                    }
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    names
}

/// Collect bare names that occur as primary expressions. This is useful for
/// object-local semantic checks where an unqualified identifier can denote a
/// member of the current object. Member suffixes are intentionally excluded so
/// a local `Name` is not confused with `Customer.Name`.
pub fn collect_primary_expression_names(
    tree: &Tree,
    source: &str,
) -> std::collections::HashSet<String> {
    let bytes = source.as_bytes();
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "primary_expression" {
            if let Some(child) = node.named_child(0) {
                if matches!(
                    child.kind(),
                    "name"
                        | "name_or_keyword"
                        | "identifier"
                        | "quoted_identifier"
                        | "object_keyword"
                        | "type_keyword"
                        | "metadata_keyword"
                        | "property_keyword"
                        | "keyword"
                ) {
                    if let Some(name) = super::node_text_clean(child, bytes) {
                        names.insert(name.to_ascii_lowercase());
                    }
                    continue;
                }
            }
        }
        if matches!(node.kind(), "for_statement" | "foreach_statement") {
            if let Some(iterator) = node.child_by_field_name("iterator") {
                if let Some(name) = super::node_text_clean(iterator, bytes) {
                    names.insert(name.to_ascii_lowercase());
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlParser;

    #[test]
    fn ast_name_collectors_ignore_literals_and_declaration_headers() {
        let src = r#"table 50100 "T"
{
    fields
    {
        field(1; "No. Series"; Code[20]) { }
        field(2; Amount; Decimal) { }
    }

    procedure Run()
    begin
        Rec."No. Series" := '';
        Validate(Amount);
        Message('Rec.Hidden and Phantom');
    end;
}"#;
        let mut parser = AlParser::new();
        let parsed = parser.parse(src);
        let members = collect_member_access_names(&parsed.tree, src);
        assert!(members.contains("no. series"), "{members:?}");
        assert!(!members.contains("hidden"), "{members:?}");

        let primary = collect_primary_expression_names(&parsed.tree, src);
        assert!(primary.contains("amount"), "{primary:?}");
        assert!(!primary.contains("phantom"), "{primary:?}");

        let page_src = r#"page 50101 "P"
{
    layout { area(Content) { field("No."; Rec."No.") { } } }
}"#;
        let page = parser.parse(page_src);
        let page_members = collect_member_access_names(&page.tree, page_src);
        assert!(page_members.contains("no."), "{page_members:?}");
    }

    #[test]
    fn find_variable_references_returns_each_span_once() {
        let src = r#"codeunit 50100 "T"
{
    var GlobalCounter: Integer;
    procedure A() begin GlobalCounter := GlobalCounter + 1; end;
    procedure B() begin GlobalCounter := 0; end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let refs = find_variable_references(&result.tree, src, "GlobalCounter");
        let mut spans: Vec<(usize, usize)> =
            refs.iter().map(|r| (r.start_byte, r.end_byte)).collect();
        let before = spans.len();
        spans.sort_unstable();
        spans.dedup();
        assert_eq!(
            before,
            spans.len(),
            "find_variable_references returned duplicate spans: {refs:?}"
        );
        // The declaration + three uses = four distinct spans.
        assert_eq!(
            spans.len(),
            4,
            "expected 4 distinct references, got {spans:?}"
        );
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
        assert!(
            !refs.is_empty(),
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

    #[test]
    fn collect_call_sites_preserves_exact_call_ranges() {
        let src = r#"codeunit 50100 Test
{
    procedure Run()
    var
        Customer: Record Customer;
    begin
        Customer.FindFirst();
        Message(Customer.Name);
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let calls = collect_call_sites(&result.tree, src);
        let find = calls
            .iter()
            .find(|(name, _)| name == "findfirst")
            .expect("member call should be collected");
        assert_eq!(
            &src.as_bytes()[find.1.start_byte..find.1.end_byte],
            b"FindFirst"
        );
        assert!(
            calls.iter().any(|(name, _)| name == "message"),
            "bare calls must also be collected"
        );
        assert!(
            calls.iter().all(|(name, _)| name != "name"),
            "field access must not be classified as a call"
        );
    }
}
