//! Page controls and actions, and the triggers written inline on them.

use super::section::extract_section_body_children;
use crate::ts_range_to_syntax as ts_range_to_lsp;
use crate::types::{SyntaxDocumentSymbol as DocumentSymbol, SyntaxSymbolKind as SymbolKind};
use tree_sitter::Node;

pub(super) fn control_keyword_to_symbol_kind(keyword: &str) -> SymbolKind {
    match crate::language_data::page_control_by_keyword(keyword)
        .map(|e| e.lsp_symbol_kind.as_str())
        .unwrap_or("Namespace")
    {
        "Field" => SymbolKind::Field,
        "Event" => SymbolKind::Event,
        "Struct" => SymbolKind::Struct,
        "Class" => SymbolKind::Class,
        "Constant" => SymbolKind::Constant,
        _ => SymbolKind::Namespace,
    }
}

/// Try to extract a "trigger Name()" symbol starting at a `control_keyword`
/// node. Returns None when the node isn't the "trigger" keyword or the
/// expected siblings aren't present.
pub(super) fn try_extract_inline_trigger(kw_node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let text = kw_node.utf8_text(source).ok()?;
    if !text.eq_ignore_ascii_case("trigger") {
        return None;
    }
    let name_node = kw_node.next_sibling()?;
    if !matches!(
        name_node.kind(),
        "identifier" | "name" | "name_or_keyword" | "quoted_identifier"
    ) {
        return None;
    }
    let name = crate::node_text_clean(name_node, source)?;
    let trigger_kw_range = kw_node.range();
    let range = ts_range_to_lsp(
        &tree_sitter::Range {
            start_byte: trigger_kw_range.start_byte,
            end_byte: name_node.range().end_byte,
            start_point: trigger_kw_range.start_point,
            end_point: name_node.range().end_point,
        },
        source,
    );
    let selection_range = ts_range_to_lsp(&name_node.range(), source);
    Some(DocumentSymbol {
        name,
        detail: Some("trigger".to_string()),
        kind: SymbolKind::Event,
        range,
        selection_range,
        children: None,
    })
}

pub(super) fn try_extract_page_control(kw_node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let kw_text = kw_node.utf8_text(source).ok()?;

    if !crate::language_data::is_page_control_keyword(kw_text) {
        return None;
    }

    let mut paren_node = None;
    let mut body_node = None;
    let mut sibling = kw_node.next_sibling();
    while let Some(sib) = sibling {
        match sib.kind() {
            "parenthesized_block" if paren_node.is_none() => paren_node = Some(sib),
            "braced_block" => {
                body_node = Some(sib);
                break;
            }
            "semicolon" => {}
            _ => break,
        }
        sibling = sib.next_sibling();
    }

    let name = if let Some(paren) = paren_node {
        extract_control_name(paren, source)
    } else {
        kw_text.to_string()
    };

    let sym_kind = control_keyword_to_symbol_kind(kw_text);

    let end_node = body_node.or(paren_node).unwrap_or(kw_node);
    let range = ts_range_to_lsp(
        &tree_sitter::Range {
            start_byte: kw_node.start_byte(),
            end_byte: end_node.end_byte(),
            start_point: kw_node.start_position(),
            end_point: end_node.end_position(),
        },
        source,
    );

    let selection_range = paren_node
        .map(|p| ts_range_to_lsp(&p.range(), source))
        .unwrap_or(ts_range_to_lsp(&kw_node.range(), source));

    // Extract children from the body braced_block. The helper folds in the
    // raw-trigger pass inline
    // (recognises `trigger OnFoo()` patterns parsed as control_keyword +
    // identifier rather than a trigger_declaration node).
    let mut nested = Vec::new();
    if let Some(body) = body_node {
        extract_section_body_children(body, source, &mut nested);
    }

    Some(DocumentSymbol {
        name,
        detail: Some(kw_text.to_string()),
        kind: sym_kind,
        range,
        selection_range,
        children: if nested.is_empty() {
            None
        } else {
            Some(nested)
        },
    })
}

/// Extract the control name from a parenthesized block like (Content), (Records), ("Entry No."; Rec."Entry No.").
pub(super) fn extract_control_name(paren: Node, source: &[u8]) -> String {
    let mut cursor = paren.walk();
    for child in paren.children(&mut cursor) {
        match child.kind() {
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
                if let Ok(text) = child.utf8_text(source) {
                    return crate::clean_identifier(text);
                }
            }
            _ => {}
        }
    }
    paren
        .utf8_text(source)
        .map(|t| t.trim_matches(|c| c == '(' || c == ')').trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::section::find_parenthesized_block;
    use super::*;
    use crate::symbols::test_support::*;
    use crate::types::SyntaxSymbolKind as SymbolKind;

    #[test]
    fn test_page_field_quoted_name_and_nested_trigger() {
        let src = r#"page 50100 "My Page"
{
    layout
    {
        area(Content)
        {
            field("Customer Name"; Rec."Customer Name")
            {
                ApplicationArea = All;
            }
        }
    }
    actions
    {
        area(Processing)
        {
            action(Post)
            {
                trigger OnAction()
                begin
                end;
            }
        }
    }
}"#;
        let symbols = parse_symbols(src);
        let all = collect_names_rec(&symbols);

        assert!(
            all.iter().any(|n| n == "Customer Name"),
            "page field name should be unquoted. Got: {:?}",
            all
        );
        let field = find_sym(&symbols, "Customer Name").unwrap();
        assert_eq!(field.kind, SymbolKind::Field);

        let trig = find_sym(&symbols, "OnAction").expect("OnAction trigger");
        assert_eq!(trig.kind, SymbolKind::Event);
        assert_eq!(trig.detail.as_deref(), Some("trigger"));
    }

    #[test]
    fn test_table_field_nested_trigger_is_child_event() {
        // A `trigger OnValidate()` inside a table field's braced body is parsed
        // as a raw `control_keyword("trigger")` + identifier, surfaced via
        // try_extract_inline_trigger and nested under the field symbol.
        let src = r#"table 50100 "T"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            trigger OnValidate()
            begin
            end;
        }
    }
}"#;
        let symbols = parse_symbols(src);
        let field = find_sym(&symbols, "No.").expect("No. field");
        assert_eq!(field.kind, SymbolKind::Field);
        let field_children = field
            .children
            .as_ref()
            .expect("field should have its nested trigger as a child");
        let trig = field_children
            .iter()
            .find(|c| c.name == "OnValidate")
            .expect("OnValidate trigger nested under field");
        assert_eq!(trig.kind, SymbolKind::Event);
        assert_eq!(trig.detail.as_deref(), Some("trigger"));
        assert!(trig.selection_range.start.character > trig.range.start.character);
    }

    #[test]
    fn test_page_field_without_integer_id_uses_first_identifier() {
        // A page field `field("Caption"; Rec.Foo)` has no integer ID before the
        // first semicolon. extract_field_name_from_paren must fall through to
        // the "no-semicolon-yet" identifier branch and use the first token.
        let src = r#"page 50100 "P"
{
    layout
    {
        area(Content)
        {
            field("Caption"; Rec.Foo) { }
        }
    }
}"#;
        let symbols = parse_symbols(src);
        let field = find_sym(&symbols, "Caption").expect("Caption field");
        assert_eq!(field.kind, SymbolKind::Field);
        assert_eq!(field.detail.as_deref(), Some("field"));
    }

    #[test]
    fn test_control_keyword_to_symbol_kind_mapping() {
        assert_eq!(control_keyword_to_symbol_kind("area"), SymbolKind::Struct);
        assert_eq!(control_keyword_to_symbol_kind("field"), SymbolKind::Field);
        assert_eq!(control_keyword_to_symbol_kind("part"), SymbolKind::Class);
        assert_eq!(control_keyword_to_symbol_kind("action"), SymbolKind::Event);
        assert_eq!(
            control_keyword_to_symbol_kind("label"),
            SymbolKind::Constant
        );
        assert_eq!(
            control_keyword_to_symbol_kind("definitely_not_a_control"),
            SymbolKind::Namespace
        );
    }

    #[test]
    fn test_try_extract_page_control_direct() {
        // `area(Content) { ... }` — the metadata_keyword "area" is a real node;
        // its sibling chain is parenthesized_block then the body. We feed the
        // keyword node directly to try_extract_page_control.
        let (tree, src) = parse_tree(
            "page 50100 \"P\"\n{\n    layout\n    {\n        area(Content)\n        {\n        }\n    }\n}",
        );
        let kw = find_node_kind_text(tree.root_node(), "metadata_keyword", "area", &src)
            .expect("area metadata_keyword node");
        let sym = try_extract_page_control(kw, src.as_bytes())
            .expect("area is a page-control keyword -> Some(symbol)");
        // Name comes from the parenthesized_block (Content).
        assert_eq!(sym.name, "Content");
        // "area" maps to Struct via page_controls.json.
        assert_eq!(sym.kind, SymbolKind::Struct);
        assert_eq!(sym.detail.as_deref(), Some("area"));
    }

    #[test]
    fn test_try_extract_page_control_rejects_non_control_keyword() {
        // A metadata_keyword that is NOT a page-control keyword (e.g. "layout"
        // is a section, "dataset" etc.) must return None so the caller doesn't
        // emit a bogus control symbol. We use "fields" which is a section
        // keyword, not a control keyword.
        let (tree, src) = parse_tree("table 50100 \"T\"\n{\n    fields\n    {\n    }\n}");
        let kw = find_node_kind_text(tree.root_node(), "metadata_keyword", "fields", &src)
            .expect("fields metadata_keyword");
        assert!(
            try_extract_page_control(kw, src.as_bytes()).is_none(),
            "a section keyword is not a page control -> None"
        );
    }

    #[test]
    fn test_extract_control_name_from_paren_and_fallback() {
        // Quoted name inside the paren is unquoted: field("Cust Name"; ...).
        let (tree, src) = parse_tree(
            "page 50100 \"P\"\n{\n    layout { area(Content) { field(\"Cust Name\"; Rec.X) { } } }\n}",
        );
        // Target the field's paren explicitly via the "field" keyword so the
        // DFS order doesn't matter.
        let field_paren = find_node_kind_text(tree.root_node(), "metadata_keyword", "field", &src)
            .and_then(|kw| kw.parent())
            .and_then(find_parenthesized_block)
            .expect("field parenthesized_block");
        assert_eq!(
            extract_control_name(field_paren, src.as_bytes()),
            "Cust Name",
            "quoted control name must be unquoted"
        );

        let area_paren = find_node_kind_text(tree.root_node(), "metadata_keyword", "area", &src)
            .and_then(|kw| kw.parent())
            .and_then(find_parenthesized_block)
            .expect("area parenthesized_block");
        assert_eq!(extract_control_name(area_paren, src.as_bytes()), "Content");
    }

    #[test]
    fn test_try_extract_inline_trigger_direct() {
        // A raw `trigger OnFoo` token sequence is parsed (in some contexts) as
        // control_keyword("trigger") + identifier. We locate a control_keyword
        // and, if its text is "trigger", verify the inline extraction.
        // `field(...; Code[20])` produces a control_keyword for the type, so we
        // need a context that yields control_keyword == "trigger". A trailing
        // `trigger Name;` inside a field body is parsed as an ERROR subtree
        // containing kw_trigger, not a clean control_keyword, so instead we
        // assert the negative branch (non-"trigger" control_keyword -> None)
        // and the positive branch via a synthesised-shape parse below.
        let (tree, src) =
            parse_tree("table 50100 \"T\"\n{\n    fields { field(1; \"No.\"; Code[20]) { } }\n}");
        if let Some(ck) = find_node_of_kind(tree.root_node(), "control_keyword") {
            if !ck
                .utf8_text(src.as_bytes())
                .unwrap_or("")
                .eq_ignore_ascii_case("trigger")
            {
                assert!(
                    try_extract_inline_trigger(ck, src.as_bytes()).is_none(),
                    "a non-trigger control_keyword must yield None"
                );
            }
        }
    }

    #[test]
    fn test_try_extract_inline_trigger_via_extract_section_body() {
        // Reproduce the real end-to-end path: a `trigger OnFoo()` whose grammar
        // shape (control_keyword + identifier) is recognised by
        // extract_section_body_children -> try_extract_inline_trigger.
        // We drive it directly through the public entry to keep it robust to
        // grammar shape: a page action body with a nested trigger.
        let symbols = parse_symbols(
            "page 50100 \"P\"\n{\n    actions\n    {\n        area(Processing)\n        {\n            action(Post)\n            {\n                trigger OnAction()\n                begin\n                end;\n            }\n        }\n    }\n}",
        );
        let all = collect_names_rec(&symbols);
        assert!(
            all.iter().any(|n| n == "OnAction"),
            "OnAction trigger must surface. Got: {:?}",
            all
        );
    }
}
