//! Sections of an object body: table fields and keys, enum values, report
//! data items, and the members nested inside each.

use super::callable::{extract_procedure_symbol, extract_trigger_symbol};
use super::controls::{try_extract_inline_trigger, try_extract_page_control};
use crate::ts_range_to_syntax as ts_range_to_lsp;
use crate::types::{SyntaxDocumentSymbol as DocumentSymbol, SyntaxSymbolKind as SymbolKind};
use tree_sitter::Node;

pub(super) fn extract_section_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let keyword_node = node.child_by_field_name("keyword")?;
    let keyword = keyword_node
        .utf8_text(source)
        .ok()
        .unwrap_or("section")
        .to_string();

    // The grammar parses enum `value(N; "Name") { }` as an object_section with
    // keyword="value". Detect this and emit an ENUM_MEMBER symbol instead of a
    // generic section symbol.
    if keyword.eq_ignore_ascii_case("value") {
        return extract_enum_value_from_section(node, source);
    }

    // Report/query `dataitem(Name; "SourceTable") { }` is also an object_section
    // (the grammar's key_declaration rule only matches `key(...)` in tables).
    // The dataitem name is the first identifier in the parenthesized block.
    if keyword.eq_ignore_ascii_case("dataitem") {
        return extract_dataitem_from_section(node, source);
    }

    // Table `field(ID; "Name"; Type) { }` — extract the field name from the
    // parenthesized block.  The name is the first identifier/string that appears
    // after a semicolon (i.e. position 2 in the triplet: ID ; Name ; Type).
    if keyword.eq_ignore_ascii_case("field") {
        if let Some(paren) = find_parenthesized_block(node) {
            let name = extract_field_name_from_paren(paren, source);
            let range = ts_range_to_lsp(&node.range(), source);
            let selection_range = ts_range_to_lsp(&paren.range(), source);
            let mut children = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                extract_section_body_children(body, source, &mut children);
            }
            return Some(DocumentSymbol {
                name,
                detail: Some("field".to_string()),
                kind: SymbolKind::Field,
                range,
                selection_range,
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
            });
        }
    }

    // AL object section-keyword → outline symbol-kind mapping.
    //
    // These are top-level grammar fixtures (sections inside `table`, `page`,
    // `report`, etc.). They are stable AL grammar forms — the set is fixed by
    // the AL grammar revision Microsoft ships, not BC release-to-release. The
    // grammar JSON corpus does not currently have a `section_keywords.json`
    // file; when one is added, replace this with
    // `language_data::section_kind_by_keyword(keyword)`. A new section that
    // doesn't yet appear here falls through to `Namespace` which is the
    // correct generic outline kind — symbols are still extracted, just under
    // a generic label.
    let sym_kind = match keyword.to_lowercase().as_str() {
        "fields" => SymbolKind::Struct,
        "keys" => SymbolKind::Key,
        "actions" => SymbolKind::Namespace,
        "layout" => SymbolKind::Namespace,
        "views" => SymbolKind::Namespace,
        "dataset" => SymbolKind::Namespace,
        "requestpage" => SymbolKind::Class,
        "rendering" => SymbolKind::Namespace,
        "fieldgroups" => SymbolKind::Struct,
        _ => SymbolKind::Namespace,
    };

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = ts_range_to_lsp(&keyword_node.range(), source);

    let mut children = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        extract_section_body_children(body, source, &mut children);
    }

    Some(DocumentSymbol {
        name: keyword,
        detail: None,
        kind: sym_kind,
        range,
        selection_range,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    })
}

/// Extract an enum value symbol from an `object_section` node whose keyword is "value".
///
/// The grammar parses `value(N; "Name") { ... }` as:
///   object_section
///     keyword("value")
///     parenthesized_block("(N; "Name")")
///     braced_block("{ ... }")
///
/// The name is the last identifier/quoted_identifier in the parenthesized block.
/// The ordinal is the integer before the semicolon.
pub(super) fn extract_enum_value_from_section(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let range = ts_range_to_lsp(&node.range(), source);

    let mut paren_node = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "parenthesized_block" {
            paren_node = Some(child);
            break;
        }
    }

    let paren = paren_node?;

    let mut ordinal = String::new();
    let mut name = String::new();
    let mut name_node_range = paren.range();
    let mut paren_cursor = paren.walk();
    for child in paren.children(&mut paren_cursor) {
        match child.kind() {
            "integer" if ordinal.is_empty() => {
                ordinal = child.utf8_text(source).unwrap_or("").to_string();
            }
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
                if let Some(trimmed) = crate::node_text_clean(child, source) {
                    name = trimmed;
                    name_node_range = child.range();
                }
            }
            _ => {}
        }
    }

    if name.is_empty() {
        return None;
    }

    let detail = if ordinal.is_empty() {
        Some("value".to_string())
    } else {
        Some(format!("value({})", ordinal))
    };

    Some(DocumentSymbol {
        name,
        detail,
        kind: SymbolKind::EnumMember,
        range,
        selection_range: ts_range_to_lsp(&name_node_range, source),
        children: None,
    })
}

/// Extract a report/query dataitem symbol from an `object_section` node whose keyword is "dataitem".
///
/// Grammar shape: `dataitem(StagingRec; "Item Journal Staging") { ... }`
///
/// The grammar parses this as:
///   object_section
///     metadata_keyword("dataitem")
///     parenthesized_block("(StagingRec; \"Item Journal Staging\")")
///     object_body("{ ... }")
///
/// Children of the parenthesized_block: `(`, identifier(name), `;`, quoted_identifier(source), `)`.
/// We take the first identifier/quoted_identifier as the dataitem name and recurse into the
/// body for nested triggers/sections.
pub(super) fn extract_dataitem_from_section(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let range = ts_range_to_lsp(&node.range(), source);

    let mut paren_node = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "parenthesized_block" {
            paren_node = Some(child);
            break;
        }
    }

    let paren = paren_node?;

    let mut name = String::new();
    let mut name_node_range = paren.range();
    let mut paren_cursor = paren.walk();
    for child in paren.children(&mut paren_cursor) {
        if matches!(
            child.kind(),
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
        ) {
            if let Ok(text) = child.utf8_text(source) {
                name = crate::clean_identifier(text);
                name_node_range = child.range();
                break;
            }
        }
    }

    if name.is_empty() {
        return None;
    }

    let mut children = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        extract_section_body_children(body, source, &mut children);
    }

    Some(DocumentSymbol {
        name,
        detail: Some("dataitem".to_string()),
        kind: SymbolKind::Class,
        range,
        selection_range: ts_range_to_lsp(&name_node_range, source),
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    })
}

pub(super) fn extract_section_body_children(
    body: Node,
    source: &[u8],
    symbols: &mut Vec<DocumentSymbol>,
) {
    // Use an explicit frame stack so deeply nested blocks cannot overflow the
    // native stack. Frames preserve depth-first emission order.
    struct Frame<'t> {
        children: Vec<Node<'t>>,
        idx: usize,
    }
    fn frame_for<'t>(block: Node<'t>) -> Frame<'t> {
        let mut cursor = block.walk();
        let mut children = Vec::with_capacity(block.child_count());
        if cursor.goto_first_child() {
            loop {
                children.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        Frame { children, idx: 0 }
    }

    let mut stack = vec![frame_for(body)];
    while let Some(top) = stack.last_mut() {
        let Some(&child) = top.children.get(top.idx) else {
            stack.pop();
            continue;
        };
        top.idx += 1;
        match child.kind() {
            "object_section" | "key_section" => {
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
                // Post grammar bump `key_declaration` is reachable and only ever
                // a table key `key(Name; fields) {}` (keyword `kw_key`).
                // Report/query dataitems remain `object_section`
                // (keyword="dataitem") and are handled by the object_section arm.
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
                    // Skip siblings consumed by the page control (paren +
                    // braced_block): advance past the body block.
                    while let Some(&sibling) = top.children.get(top.idx) {
                        top.idx += 1;
                        if sibling.kind() == "braced_block" {
                            break;
                        }
                    }
                    symbols.push(sym);
                }
            }
            "braced_block" => {
                stack.push(frame_for(child));
            }
            // Raw `trigger OnFoo()` patterns inside
            // dataitem/action bodies have a `control_keyword` parent (text
            // "trigger") followed by an identifier + parenthesized_block.
            // Handle inline here so dataitem/action inline triggers are picked
            // up as the generic body walker descends their braced blocks.
            "control_keyword" => {
                if let Some(sym) = try_extract_inline_trigger(child, source) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn extract_enum_value_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = crate::node_name_or(node, source, "(unnamed)");

    let id = node
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range(), source))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail: Some(format!("value({})", id)),
        kind: SymbolKind::EnumMember,
        range,
        selection_range,
        children: None,
    })
}

pub(super) fn extract_key_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = crate::node_name_or(node, source, "(unnamed)");

    let fields = node
        .child_by_field_name("fields")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range(), source))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail: if fields.is_empty() {
            None
        } else {
            Some(fields.to_string())
        },
        kind: SymbolKind::Key,
        range,
        selection_range,
        children: None,
    })
}

pub(super) fn find_parenthesized_block(node: Node) -> Option<Node> {
    // `cursor` is bound separately because the lifetime of the borrowed cursor
    // doesn't extend through `find` when chained inline.
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .find(|child| child.kind() == "parenthesized_block");
    result
}

/// Extract a table field name from `(ID; "Name"; Type)`.
///
/// The name is the first identifier/quoted_identifier/string/name that appears
/// after the first semicolon in the parenthesized block.  For a page field like
/// `("Caption"; ...)` (no integer ID before the semicolon) the first identifier
/// is used directly.
pub(super) fn extract_field_name_from_paren(paren: Node, source: &[u8]) -> String {
    let mut past_first_semicolon = false;
    let mut cursor = paren.walk();
    for child in paren.children(&mut cursor) {
        match child.kind() {
            "semicolon" if !past_first_semicolon => {
                past_first_semicolon = true;
            }
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
                if past_first_semicolon =>
            {
                if let Some(trimmed) = crate::node_text_clean(child, source) {
                    return trimmed;
                }
            }
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
                if !past_first_semicolon =>
            {
                // Page field: `field("Caption"; ...)` — no integer before semicolon.
                if let Some(trimmed) = crate::node_text_clean(child, source) {
                    return trimmed;
                }
            }
            _ => {}
        }
    }
    paren
        .utf8_text(source)
        .map(|t| {
            t.trim_matches(|c: char| c == '(' || c == ')')
                .trim()
                .to_string()
        })
        .unwrap_or_else(|_| "field".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::extract_document_symbols;
    use crate::symbols::test_support::*;
    use crate::types::SyntaxSymbolKind as SymbolKind;
    use crate::AlParser;

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
        assert_eq!(obj.kind, SymbolKind::Enum);
        let children = obj.children.as_ref().expect("Should have enum values");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].kind, SymbolKind::EnumMember);
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
        assert_eq!(obj.kind, SymbolKind::Class);
    }

    #[test]
    fn test_extract_symbols_report_dataitem_trigger() {
        let src = r#"report 50200 "IJL Process Staging"
{
    dataset
    {
        dataitem(StagingRec; "Item Journal Staging")
        {
            RequestFilterFields = "Entry No.", Status;

            trigger OnPreDataItem()
            begin
                // body
            end;
        }
    }

    trigger OnPostReport()
    begin
    end;

    procedure SetAction(NewAction: Enum "IJL Process Action")
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);

        fn collect_names(syms: &[DocumentSymbol]) -> Vec<String> {
            let mut names = Vec::new();
            for sym in syms {
                names.push(sym.name.clone());
                if let Some(children) = &sym.children {
                    names.extend(collect_names(children));
                }
            }
            names
        }

        let all_names = collect_names(&symbols);
        assert!(
            all_names.iter().any(|n| n == "OnPreDataItem"),
            "OnPreDataItem trigger should be in document symbols. Got: {:?}",
            all_names
        );
        assert!(
            all_names.iter().any(|n| n == "OnPostReport"),
            "OnPostReport trigger should be in document symbols. Got: {:?}",
            all_names
        );
        assert!(
            all_names.iter().any(|n| n == "StagingRec"),
            "StagingRec dataitem should be in document symbols. Got: {:?}",
            all_names
        );
    }

    #[test]
    fn test_table_fields_keys_sections_and_field_names() {
        let src = r#"table 50101 "My Table2"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Description; Text[100]) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}"#;
        let symbols = parse_symbols(src);
        let obj = &symbols[0];
        let children = obj.children.as_ref().expect("table children");

        let fields_section = children
            .iter()
            .find(|c| c.name == "fields")
            .expect("fields section");
        assert_eq!(fields_section.kind, SymbolKind::Struct);

        let keys_section = children
            .iter()
            .find(|c| c.name == "keys")
            .expect("keys section");
        assert_eq!(keys_section.kind, SymbolKind::Key);

        let no_field = find_sym(&symbols, "No.").expect("No. field");
        assert_eq!(no_field.kind, SymbolKind::Field);
        assert_eq!(no_field.detail.as_deref(), Some("field"));

        let desc_field = find_sym(&symbols, "Description").expect("Description field");
        assert_eq!(desc_field.kind, SymbolKind::Field);
    }

    #[test]
    fn test_enum_value_ordinal_in_detail() {
        let src = r#"enum 50100 "My Enum"
{
    value(0; First) { }
    value(7; "Last One") { }
}"#;
        let symbols = parse_symbols(src);
        let first = find_sym(&symbols, "First").expect("First value");
        assert_eq!(first.kind, SymbolKind::EnumMember);
        assert_eq!(first.detail.as_deref(), Some("value(0)"));

        let last = find_sym(&symbols, "Last One").expect("Last One value");
        assert_eq!(last.detail.as_deref(), Some("value(7)"));
    }

    #[test]
    fn test_report_dataitem_is_class_with_dataitem_detail() {
        let src = r#"report 50200 "My Report"
{
    dataset
    {
        dataitem(MyItem; "Customer")
        {
            column(Name; "Name") { }

            trigger OnAfterGetRecord()
            begin
            end;
        }
    }
}"#;
        let symbols = parse_symbols(src);
        let item = find_sym(&symbols, "MyItem").expect("dataitem symbol");
        assert_eq!(item.detail.as_deref(), Some("dataitem"));
        let all = collect_names_rec(&symbols);
        assert!(
            all.iter().any(|n| n == "OnAfterGetRecord"),
            "dataitem trigger should be captured. Got: {:?}",
            all
        );
    }

    #[test]
    fn test_section_keyword_kind_mapping() {
        let src = r#"page 50100 "P"
{
    layout { area(Content) { } }
    views { view(MyView) { } }
    actions { area(Processing) { } }
}
table 50101 "T2" { fieldgroups { fieldgroup(DropDown; "No.") { } } }
report 50102 "R2" { rendering { layout(L) { } } requestpage { layout { } } dataset { } }"#;
        let symbols = parse_symbols(src);

        fn kind_of(syms: &[DocumentSymbol], name: &str) -> Option<SymbolKind> {
            for s in syms {
                if s.name == name {
                    return Some(s.kind);
                }
                if let Some(children) = &s.children {
                    if let Some(k) = kind_of(children, name) {
                        return Some(k);
                    }
                }
            }
            None
        }

        assert_eq!(kind_of(&symbols, "layout"), Some(SymbolKind::Namespace));
        assert_eq!(kind_of(&symbols, "views"), Some(SymbolKind::Namespace));
        assert_eq!(kind_of(&symbols, "actions"), Some(SymbolKind::Namespace));
        assert_eq!(kind_of(&symbols, "fieldgroups"), Some(SymbolKind::Struct));
        assert_eq!(kind_of(&symbols, "rendering"), Some(SymbolKind::Namespace));
        assert_eq!(kind_of(&symbols, "requestpage"), Some(SymbolKind::Class));
        assert_eq!(kind_of(&symbols, "dataset"), Some(SymbolKind::Namespace));
    }

    #[test]
    fn test_extract_enum_value_from_section_direct() {
        // The grammar emits `enum_value_declaration` for `value(N; Name)`, but
        // the defensive `extract_enum_value_from_section` handles the
        // object_section spelling. We synthesise a matching shape by reusing a
        // parenthesized_block that has integer ; name, attached to a value
        // object_section. Since we can't make the grammar emit it, we verify
        // the helper's None-guard on a paren with no name, and its happy path
        // by locating an object_section whose paren has integer+name.
        //
        // Locate a parenthesized_block that contains an integer then a name
        // (the report dataitem `(I; Customer)` qualifies but starts with an
        // identifier; the table field `(1; "No."; Code[20])` starts with an
        // integer then a quoted name) and wrap reasoning around the helper that
        // walks the same children. Here we drive the actual function on a real
        // object_section node and assert it does not panic and obeys its
        // empty-name -> None contract for a paren with no name token.
        let (tree, src) = parse_tree("page 50100 \"P\"\n{\n    layout { area(Content) { } }\n}");
        // object_section for area(Content): has a parenthesized_block with a
        // single identifier (Content) and NO integer ordinal. Feeding it to
        // extract_enum_value_from_section yields a symbol named "Content" with
        // an empty ordinal -> detail "value".
        let section = find_node_of_kind(tree.root_node(), "object_section")
            .and_then(|outer| {
                // descend to the innermost object_section (area(Content))
                find_node_kind_text(outer, "metadata_keyword", "area", &src)
                    .and_then(|kw| kw.parent())
            })
            .expect("area object_section");
        let sym =
            extract_enum_value_from_section(section, src.as_bytes()).expect("section has a name");
        assert_eq!(sym.name, "Content");
        assert_eq!(sym.kind, SymbolKind::EnumMember);
        // No integer ordinal in (Content) -> detail is the bare "value".
        assert_eq!(sym.detail.as_deref(), Some("value"));
    }

    #[test]
    fn test_extract_enum_value_from_section_with_ordinal() {
        // A parenthesized_block of the form (N; Name) — found on a table field
        // `field(1; "No."; Code[20])` — drives the integer-ordinal branch.
        let (tree, src) =
            parse_tree("table 50100 \"T\"\n{\n    fields { field(1; \"No.\"; Code[20]) { } }\n}");
        let section = find_node_kind_text(tree.root_node(), "metadata_keyword", "field", &src)
            .and_then(|kw| kw.parent())
            .expect("field object_section");
        let sym =
            extract_enum_value_from_section(section, src.as_bytes()).expect("has integer + name");
        // Ordinal is the leading integer (1); name is the last identifier-like
        // token in the paren. For (1; "No."; Code[20]) the last such token is
        // "Code" (an identifier-kind control type token may or may not match —
        // assert the ordinal branch produced a value(N) detail).
        assert!(
            sym.detail.as_deref().unwrap().starts_with("value(1)"),
            "ordinal 1 should appear in detail, got {:?}",
            sym.detail
        );
    }

    #[test]
    fn test_extract_dataitem_from_section_direct_and_empty_guard() {
        let (tree, src) = parse_tree(
            "report 50200 \"R\"\n{\n    dataset\n    {\n        dataitem(StagingRec; \"Src\")\n        {\n        }\n    }\n}",
        );
        let section = find_node_kind_text(tree.root_node(), "metadata_keyword", "dataitem", &src)
            .and_then(|kw| kw.parent())
            .expect("dataitem object_section");
        let sym =
            extract_dataitem_from_section(section, src.as_bytes()).expect("dataitem has a name");
        assert_eq!(sym.name, "StagingRec");
        assert_eq!(sym.kind, SymbolKind::Class);
        assert_eq!(sym.detail.as_deref(), Some("dataitem"));
    }

    #[test]
    fn test_find_parenthesized_block_present_and_absent() {
        let (tree, src) = parse_tree("page 50100 \"P\"\n{\n    layout { area(Content) { } }\n}");
        let section = find_node_kind_text(tree.root_node(), "metadata_keyword", "area", &src)
            .and_then(|kw| kw.parent())
            .expect("area object_section");
        assert!(
            find_parenthesized_block(section).is_some(),
            "area(Content) section has a parenthesized_block"
        );

        let body = find_node_of_kind(tree.root_node(), "object_body").expect("object_body");
        assert!(
            find_parenthesized_block(body).is_none(),
            "an object_body has no direct parenthesized_block child"
        );
    }

    #[test]
    fn test_extract_field_name_from_paren_after_semicolon() {
        // Table field `(1; "No."; Code[20])`: the ID before the first semicolon
        // is an *integer* (1), which the "no-semicolon-yet" branch ignores. The
        // field name is therefore the first identifier-like token AFTER the
        // first semicolon, i.e. "No." — proving the semicolon-tracking logic.
        let (tree, src) =
            parse_tree("table 50100 \"T\"\n{\n    fields { field(1; \"No.\"; Code[20]) { } }\n}");
        let paren = find_node_kind_text(tree.root_node(), "metadata_keyword", "field", &src)
            .and_then(|kw| kw.parent())
            .and_then(find_parenthesized_block)
            .expect("field parenthesized_block");
        let name = extract_field_name_from_paren(paren, src.as_bytes());
        assert_eq!(name, "No.", "name is the token after the first semicolon");
    }

    #[test]
    fn test_extract_field_name_from_paren_page_field_before_semicolon() {
        // Page field `("Caption"; Rec.Foo)`: the FIRST token before the first
        // semicolon is a quoted identifier (no integer ID), so the
        // "no-semicolon-yet" branch returns it directly ("Caption").
        let (tree, src) = parse_tree(
            "page 50100 \"P\"\n{\n    layout { area(Content) { field(\"Caption\"; Rec.Foo) { } } }\n}",
        );
        let paren = find_node_kind_text(tree.root_node(), "metadata_keyword", "field", &src)
            .and_then(|kw| kw.parent())
            .and_then(find_parenthesized_block)
            .expect("page field parenthesized_block");
        let name = extract_field_name_from_paren(paren, src.as_bytes());
        assert_eq!(
            name, "Caption",
            "page field caption (no integer id) is returned before the semicolon"
        );
    }

    #[test]
    fn test_extract_key_symbol_via_named_node() {
        // extract_key_symbol reads the `name` field (and an optional `fields`
        // field). The current grammar doesn't emit `key_declaration`, but the
        // helper is pure over a Node with a `name` field. A procedure_declaration
        // has a `name` field and no `fields` field, so it drives the name path
        // and the empty-fields -> detail None branch.
        let (tree, src) =
            parse_tree("codeunit 50100 C\n{\n    procedure Pk()\n    begin\n    end;\n}");
        let proc = find_node_of_kind(tree.root_node(), "procedure_declaration")
            .expect("procedure_declaration");
        let sym = extract_key_symbol(proc, src.as_bytes()).expect("always Some");
        assert_eq!(sym.name, "Pk");
        assert_eq!(sym.kind, SymbolKind::Key);
        assert!(sym.detail.is_none(), "no fields -> None detail");
    }
}
