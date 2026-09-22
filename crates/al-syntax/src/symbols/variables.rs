//! `var` sections: the variable names and types declared in them.

use super::labels::collect_label_symbols_from_text;
use crate::ts_range_to_syntax as ts_range_to_lsp;
use crate::types::{
    SyntaxDocumentSymbol as DocumentSymbol, SyntaxRange, SyntaxSymbolKind as SymbolKind,
};
use tree_sitter::Node;

pub(super) fn extract_var_section_children(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<DocumentSymbol>,
) {
    collect_var_symbols_recursive(node, source, symbols);
    collect_label_symbols_from_text(node, source, symbols);
}

pub(super) fn collect_var_symbols_recursive(
    root: Node,
    source: &[u8],
    symbols: &mut Vec<DocumentSymbol>,
) {
    // Iterative DFS using an explicit stack — avoids stack overflow on deeply nested AL.
    // Start by pushing the children of root (mirroring the original behaviour of iterating
    // root's children and recursing only into unknown-kind nodes).
    let mut stack: Vec<Node> = {
        let mut cursor = root.walk();
        root.children(&mut cursor).collect()
    };
    // Reverse so that popping yields left-to-right order
    stack.reverse();

    while let Some(child) = stack.pop() {
        match child.kind() {
            "regular_variable_declaration" => {
                let detail = extract_node_text(child.child_by_field_name("type"), source);
                let range = ts_range_to_lsp(&child.range(), source);
                for (name, selection_range) in extract_regular_variable_names(child, source) {
                    symbols.push(DocumentSymbol {
                        name,
                        detail: detail.clone(),
                        kind: SymbolKind::Variable,
                        range,
                        selection_range,
                        children: None,
                    });
                }
                // Do not descend into regular_variable_declaration children
            }
            "label_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Some(name) = clean_node_text(name_node, source) {
                        let detail = extract_node_text(child.child_by_field_name("type"), source);
                        symbols.push(DocumentSymbol {
                            name,
                            detail,
                            kind: SymbolKind::Variable,
                            range: ts_range_to_lsp(&child.range(), source),
                            selection_range: ts_range_to_lsp(&name_node.range(), source),
                            children: None,
                        });
                    }
                }
                // Do not descend into label_declaration children
            }
            _ => {
                let mut cursor = child.walk();
                let grandchildren: Vec<Node> = child.children(&mut cursor).collect();
                for gc in grandchildren.into_iter().rev() {
                    stack.push(gc);
                }
            }
        }
    }
}

fn extract_regular_variable_names(node: Node, source: &[u8]) -> Vec<(String, SyntaxRange)> {
    let Some(sep_start) = node.child_by_field_name("sep").map(|sep| sep.start_byte()) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    collect_variable_name_nodes(node, sep_start, source, &mut names);

    if names.is_empty() {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Some(name) = clean_node_text(name_node, source) {
                names.push((name, ts_range_to_lsp(&name_node.range(), source)));
            }
        }
    }

    names
}

fn collect_variable_name_nodes(
    node: Node,
    sep_start: usize,
    source: &[u8],
    names: &mut Vec<(String, SyntaxRange)>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.start_byte() >= sep_start {
            continue;
        }

        if current.child_count() == 0 && is_variable_name_node(current.kind()) {
            if let Some(name) = clean_node_text(current, source) {
                names.push((name, ts_range_to_lsp(&current.range(), source)));
            }
            continue;
        }

        // Push children in reverse order so left-to-right children are
        // popped (and thus processed) in their original left-to-right order.
        let mut cursor = current.walk();
        let children: Vec<_> = current.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
}

fn is_variable_name_node(kind: &str) -> bool {
    // Reserved words in identifier position retain their `kw_*` grammar kind.
    matches!(
        kind,
        "identifier"
            | "quoted_identifier"
            | "keyword"
            | "object_keyword"
            | "metadata_keyword"
            | "property_keyword"
    ) || kind.starts_with("kw_")
}

fn extract_node_text(node: Option<Node>, source: &[u8]) -> Option<String> {
    crate::node_text_clean(node?, source)
}

fn clean_node_text(node: Node, source: &[u8]) -> Option<String> {
    crate::node_text_clean(node, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::extract_document_symbols;
    use crate::symbols::test_support::*;
    use crate::types::SyntaxSymbolKind as SymbolKind;
    use crate::AlParser;

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
            .filter(|child| child.kind == SymbolKind::Variable)
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

    #[test]
    fn test_is_variable_name_node_accepts_kw_prefix() {
        assert!(is_variable_name_node("identifier"));
        assert!(is_variable_name_node("quoted_identifier"));
        assert!(is_variable_name_node("kw_record"));
        assert!(is_variable_name_node("kw_anything_at_all"));
        assert!(!is_variable_name_node("semicolon"));
        assert!(!is_variable_name_node(";"));
    }

    #[test]
    fn test_multiple_vars_share_type_each_get_own_symbol() {
        // `Alpha, Beta, Gamma: Integer;` must yield three Variable symbols, each
        // carrying the shared type as detail (multi-name var extraction path).
        let src = r#"codeunit 50100 Test
{
    var
        Alpha, Beta, Gamma: Integer;
}"#;
        let symbols = parse_symbols(src);
        let children = symbols[0].children.as_ref().expect("var children");
        let vars: Vec<&DocumentSymbol> = children
            .iter()
            .filter(|c| c.kind == SymbolKind::Variable)
            .collect();
        assert_eq!(
            vars.len(),
            3,
            "one symbol per declared name, got {:?}",
            vars
        );
        for v in &vars {
            assert_eq!(v.detail.as_deref(), Some("Integer"));
        }
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"Alpha"));
        assert!(names.contains(&"Beta"));
        assert!(names.contains(&"Gamma"));
    }
}
