//! The object declaration itself and the members directly under its body.

use super::callable::{extract_event_symbol, extract_procedure_symbol, extract_trigger_symbol};
use super::kinds::{object_kind_display, object_kind_to_symbol_kind};
use super::section::{extract_enum_value_symbol, extract_key_symbol, extract_section_symbol};
use super::variables::{collect_var_symbols_recursive, extract_var_section_children};
use crate::ts_range_to_syntax as ts_range_to_lsp;
use crate::types::{SyntaxDocumentSymbol as DocumentSymbol, SyntaxSymbolKind as SymbolKind};
use tree_sitter::Node;

pub(super) fn extract_object_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let kind_node = node.child_by_field_name("kind")?;
    let kind_str = kind_node.kind();
    let sym_kind = object_kind_to_symbol_kind(kind_str);

    let name = crate::extract_object_name(node, source).unwrap_or_else(|| "(unnamed)".to_string());
    let name_node_range = {
        let mut found_range = None;
        let mut obj_cursor = node.walk();
        for c in node.children(&mut obj_cursor) {
            if matches!(
                c.kind(),
                "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
            ) && crate::node_text_clean(c, source).is_some()
            {
                found_range = Some(c.range());
                break;
            }
        }
        found_range
    };

    let id_text = node
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let kind_display = object_kind_display(kind_str);
    let detail = if !id_text.is_empty() {
        Some(format!("{} {}", kind_display, id_text))
    } else {
        Some(kind_display)
    };

    let range = ts_range_to_lsp(&node.range(), source);

    let selection_range = name_node_range
        .map(|r| ts_range_to_lsp(&r, source))
        .unwrap_or(ts_range_to_lsp(&kind_node.range(), source));

    let mut children = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        extract_body_children(body, source, &mut children);
    }

    Some(DocumentSymbol {
        name,
        detail,
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

pub(super) fn extract_namespace_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = crate::node_name_or(node, source, "(unknown)");

    let keyword = node
        .child_by_field_name("keyword")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("using");

    let range = ts_range_to_lsp(&node.range(), source);

    Some(DocumentSymbol {
        name,
        detail: Some(keyword.to_string()),
        kind: SymbolKind::Namespace,
        range,
        selection_range: range,
        children: None,
    })
}

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
            // `key_section` (the `keys { }` block) is routed through
            // extract_section_symbol like any other section: its `keyword`
            // field (`kw_keys`, text "keys") maps to SymbolKind::Key and its
            // body's key_declarations become child key symbols.
            "object_section" | "key_section" => {
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

pub(super) fn extract_named_symbol(
    node: Node,
    source: &[u8],
    kind: SymbolKind,
    detail: Option<String>,
) -> Option<DocumentSymbol> {
    let name = crate::node_name_or(node, source, "(unnamed)");

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range(), source))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail,
        kind,
        range,
        selection_range,
        children: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::extract_document_symbols;
    use crate::symbols::test_support::*;
    use crate::types::SyntaxSymbolKind as SymbolKind;
    use crate::AlParser;

    /// The outline shows the identifier, not its escaped spelling.
    #[test]
    fn symbol_names_containing_a_doubled_quote_are_unescaped() {
        let src = "table 50100 \"My \"\"Big\"\" Table\"\n\
                   {\n\
                   \x20   fields\n\
                   \x20   {\n\
                   \x20       field(1; \"No. \"\"X\"\" Series\"; Code[20]) { }\n\
                   \x20   }\n\
                   \n\
                   \x20   procedure \"Do \"\"It\"\" Now\"()\n\
                   \x20   begin\n\
                   \x20   end;\n\
                   }\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);

        fn all_names(symbols: &[DocumentSymbol], out: &mut Vec<String>) {
            for symbol in symbols {
                out.push(symbol.name.clone());
                if let Some(children) = &symbol.children {
                    all_names(children, out);
                }
            }
        }
        let mut names = Vec::new();
        all_names(&symbols, &mut names);

        assert_eq!(symbols[0].name, r#"My "Big" Table"#);
        assert!(
            names.iter().any(|n| n == r#"No. "X" Series"#),
            "got {names:?}"
        );
        assert!(names.iter().any(|n| n == r#"Do "It" Now"#), "got {names:?}");
    }

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
        assert_eq!(obj.kind, SymbolKind::Class);
        let children = obj.children.as_ref().expect("Should have children");
        assert!(
            children.len() >= 2,
            "Should have at least 2 procedures, got {}",
            children.len()
        );
    }

    #[test]
    fn test_namespace_and_using_declarations() {
        let src = r#"namespace MyApp.Sales;
using System.Utilities;

codeunit 50100 Test { }"#;
        let symbols = parse_symbols(src);

        let ns = find_sym(&symbols, "MyApp.Sales").expect("namespace symbol");
        assert_eq!(ns.kind, SymbolKind::Namespace);
        assert_eq!(ns.detail.as_deref(), Some("namespace"));
        // selection_range mirrors range for namespaces (no separate name node range)
        assert_eq!(ns.selection_range, ns.range);
        assert!(ns.children.is_none());

        let using = find_sym(&symbols, "System.Utilities").expect("using symbol");
        assert_eq!(using.kind, SymbolKind::Namespace);
        assert_eq!(using.detail.as_deref(), Some("using"));
    }

    #[test]
    fn test_object_detail_includes_id() {
        let src = r#"codeunit 50100 "My Codeunit" { }"#;
        let symbols = parse_symbols(src);
        let obj = &symbols[0];
        assert_eq!(obj.detail.as_deref(), Some("codeunit 50100"));
        // The selection range must point at the name, not the whole object.
        assert!(obj.selection_range.start.character >= obj.range.start.character);
    }

    #[test]
    fn test_object_without_id_detail_is_keyword_only() {
        // An interface has no numeric ID — detail should be just the keyword.
        let src = r#"interface "My Interface"
{
    procedure Foo()
}"#;
        let symbols = parse_symbols(src);
        let obj = &symbols[0];
        assert_eq!(obj.name, "My Interface");
        assert_eq!(obj.kind, SymbolKind::Interface);
        assert_eq!(obj.detail.as_deref(), Some("interface"));
    }

    #[test]
    fn test_empty_source_yields_no_symbols() {
        let symbols = parse_symbols("");
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_multiple_top_level_objects_in_one_file() {
        let src = r#"codeunit 50100 First { }

table 50101 Second { fields { } }

enum 50102 Third { value(0; A) { } }"#;
        let symbols = parse_symbols(src);
        assert_eq!(symbols.len(), 3, "three top-level objects");
        assert_eq!(symbols[0].name, "First");
        assert_eq!(symbols[1].name, "Second");
        assert_eq!(symbols[2].name, "Third");
        assert_eq!(symbols[2].kind, SymbolKind::Enum);
    }

    #[test]
    fn test_extract_named_symbol_unnamed_fallback() {
        // A node with no `name` field falls back to "(unnamed)" and uses the
        // node range as the selection range. Object declarations now carry a
        // populated `name:` field, so the fallback is exercised with an
        // `object_body`, which has no name field.
        let (tree, src) = parse_tree("codeunit 50100 \"C\" { }");
        let body = find_node_of_kind(tree.root_node(), "object_body").expect("object_body");
        let sym = extract_named_symbol(body, src.as_bytes(), SymbolKind::Function, None)
            .expect("always Some");
        assert_eq!(sym.name, "(unnamed)");
        assert_eq!(sym.selection_range, sym.range);
    }
}
