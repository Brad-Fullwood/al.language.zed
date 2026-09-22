//! Procedures, triggers, event subscribers, and the executable scopes
//! (`begin`, `if`, `case`, loops, `with`) nested inside them.

use super::object::extract_named_symbol;
use super::variables::extract_var_section_children;
use crate::ts_range_to_syntax as ts_range_to_lsp;
use crate::types::{SyntaxDocumentSymbol as DocumentSymbol, SyntaxSymbolKind as SymbolKind};
use tracing::debug;
use tree_sitter::Node;

pub(super) fn extract_callable_symbol(
    node: Node,
    source: &[u8],
    kind: SymbolKind,
    detail: Option<String>,
) -> Option<DocumentSymbol> {
    let mut symbol = extract_named_symbol(node, source, kind, detail)?;
    let mut children = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "var_section" | "empty_var_section" => {
                extract_var_section_children(child, source, &mut children);
            }
            "begin_end_block" => {
                children.push(extract_executable_scope(child, source));
            }
            _ => {}
        }
    }

    symbol.children = (!children.is_empty()).then_some(children);
    Some(symbol)
}

/// Keyword, the node kind of that keyword, the field holding the block's own
/// expression, and the symbol kind, for each executable scope.
///
/// The field is what turns a row of bare `if`/`case` keywords into something
/// worth reading: `if not Rec.IsEmpty()` instead of `if`. A `begin` block has no
/// expression of its own, so it keeps the keyword alone.
fn executable_scope_metadata(
    kind: &str,
) -> Option<(&'static str, &'static str, Option<&'static str>, SymbolKind)> {
    match kind {
        "begin_end_block" => Some(("begin", "kw_begin", None, SymbolKind::Struct)),
        "if_statement" => Some(("if", "kw_if", Some("condition"), SymbolKind::Operator)),
        "case_statement" => Some(("case", "kw_case", Some("value"), SymbolKind::Operator)),
        "for_statement" => Some(("for", "kw_for", Some("iterator"), SymbolKind::Operator)),
        "foreach_statement" => Some((
            "foreach",
            "kw_foreach",
            Some("iterator"),
            SymbolKind::Operator,
        )),
        "while_statement" => Some(("while", "kw_while", Some("condition"), SymbolKind::Operator)),
        "repeat_statement" => Some((
            "repeat",
            "kw_repeat",
            Some("condition"),
            SymbolKind::Operator,
        )),
        "with_statement" => Some(("with", "kw_with", Some("value"), SymbolKind::Operator)),
        _ => None,
    }
}

/// `if` plus the condition text, collapsed to one line and capped so a
/// multi-line condition cannot push a whole expression into the outline.
fn executable_scope_name(node: Node, source: &[u8], keyword: &str, field: Option<&str>) -> String {
    const MAX_LEN: usize = 60;

    let Some(text) = field
        .and_then(|field| node.child_by_field_name(field))
        .and_then(|expr| expr.utf8_text(source).ok())
    else {
        return keyword.to_string();
    };

    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return keyword.to_string();
    }
    let truncated = match collapsed.char_indices().nth(MAX_LEN) {
        Some((byte, _)) => format!("{}…", &collapsed[..byte]),
        None => collapsed,
    };
    format!("{keyword} {truncated}")
}

fn extract_executable_scope(node: Node, source: &[u8]) -> DocumentSymbol {
    let (keyword, keyword_kind, field, kind) = executable_scope_metadata(node.kind())
        .expect("extract_executable_scope must receive a supported executable scope");
    let name = executable_scope_name(node, source, keyword, field);
    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = {
        let mut cursor = node.walk();
        let selected = node
            .children(&mut cursor)
            .find(|child| child.kind() == keyword_kind)
            .map(|child| ts_range_to_lsp(&child.range(), source))
            .unwrap_or(range);
        selected
    };
    let mut children = Vec::new();
    collect_immediate_executable_scopes(node, source, &mut children);

    DocumentSymbol {
        name,
        detail: Some("executable scope".to_string()),
        kind,
        range,
        selection_range,
        children: (!children.is_empty()).then_some(children),
    }
}

/// Find the first executable scopes below `root`.
///
/// Once a scope is found, that scope owns its descendants. This preserves the
/// syntax tree's nesting instead of flattening every control statement under
/// the callable's outer `begin` block.
fn collect_immediate_executable_scopes(
    root: Node,
    source: &[u8],
    symbols: &mut Vec<DocumentSymbol>,
) {
    let mut cursor = root.walk();
    let mut stack: Vec<Node> = root.children(&mut cursor).collect();
    stack.reverse();

    while let Some(node) = stack.pop() {
        if executable_scope_metadata(node.kind()).is_some() {
            symbols.push(extract_executable_scope(node, source));
            continue;
        }

        let mut child_cursor = node.walk();
        let children: Vec<_> = node.children(&mut child_cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

pub(super) fn extract_procedure_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| crate::node_text_clean(n, source))
        .unwrap_or_else(|| "(unnamed)".to_string());
    let name = name.as_str();

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

    extract_callable_symbol(node, source, SymbolKind::Function, detail)
}

pub(super) fn extract_trigger_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    extract_callable_symbol(node, source, SymbolKind::Event, Some("trigger".to_string()))
}

pub(super) fn extract_event_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    extract_callable_symbol(node, source, SymbolKind::Event, Some("event".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::extract_document_symbols;
    use crate::symbols::test_support::*;
    use crate::types::SyntaxSymbolKind as SymbolKind;
    use crate::AlParser;

    #[test]
    fn executable_scopes_are_named_by_their_own_expression() {
        let src = r#"codeunit 50100 Test
{
    procedure Post()
    var
        Item: Record Item;
        Index: Integer;
    begin
        if not Item.IsEmpty() then
            case Item.Type of
                Item.Type::Inventory:
                    Message('a');
            end;
        for Index := 1 to 10 do
            Message('b');
        repeat
            Message('c');
        until Item.Next() = 0;
    end;
}"#;
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

        assert!(names.iter().any(|n| n == "begin"), "got {names:?}");
        assert!(
            names.iter().any(|n| n == "if not Item.IsEmpty()"),
            "got {names:?}"
        );
        assert!(names.iter().any(|n| n == "case Item.Type"), "got {names:?}");
        assert!(names.iter().any(|n| n == "for Index"), "got {names:?}");
        assert!(
            names.iter().any(|n| n == "repeat Item.Next() = 0"),
            "got {names:?}"
        );
    }

    #[test]
    fn a_multi_line_scope_expression_is_collapsed_and_capped() {
        let src = "codeunit 50100 Test\n\
                   {\n\
                   \x20   procedure Post()\n\
                   \x20   var\n\
                   \x20       Item: Record Item;\n\
                   \x20   begin\n\
                   \x20       if (Item.\"No.\" <> '') and\n\
                   \x20          (Item.Description <> '') and\n\
                   \x20          (Item.Type = Item.Type::Inventory) then\n\
                   \x20           Message('a');\n\
                   \x20   end;\n\
                   }\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);

        fn find_if(symbols: &[DocumentSymbol]) -> Option<String> {
            for symbol in symbols {
                if symbol.name.starts_with("if ") {
                    return Some(symbol.name.clone());
                }
                if let Some(found) = symbol.children.as_deref().and_then(find_if) {
                    return Some(found);
                }
            }
            None
        }
        let name = find_if(&symbols).expect("an if scope");

        assert!(!name.contains('\n'), "{name}");
        assert!(name.chars().count() <= 64, "{name}");
        assert!(name.ends_with('…'), "{name}");
    }

    #[test]
    fn test_callable_symbols_include_local_variables_and_nested_executable_scopes() {
        let src = r#"codeunit 50100 "Breadcrumbs"
{
    procedure Walk(Value: Integer)
    var
        Counter: Integer;
    begin
        if Value > 0 then begin
            while Counter < Value do begin
                Counter += 1;
            end;
        end;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        assert!(
            result.errors.is_empty(),
            "fixture must parse cleanly: {:?}",
            result.errors
        );

        let symbols = extract_document_symbols(&result.tree, src);
        let object_children = symbols[0].children.as_ref().expect("object children");
        let procedure = object_children
            .iter()
            .find(|symbol| symbol.name == "Walk")
            .expect("procedure symbol");
        let procedure_children = procedure.children.as_ref().expect("callable children");
        assert!(
            procedure_children
                .iter()
                .any(|symbol| symbol.name == "Counter" && symbol.kind == SymbolKind::Variable),
            "local variable should be nested under the procedure"
        );

        let begin = procedure_children
            .iter()
            .find(|symbol| symbol.name == "begin")
            .expect("procedure begin scope");
        // A scope is named by its keyword plus its own expression.
        let if_scope = begin
            .children
            .as_ref()
            .and_then(|children| {
                children
                    .iter()
                    .find(|symbol| symbol.name.starts_with("if "))
            })
            .expect("if scope");
        let if_begin = if_scope
            .children
            .as_ref()
            .and_then(|children| children.iter().find(|symbol| symbol.name == "begin"))
            .expect("if begin scope");
        let while_scope = if_begin
            .children
            .as_ref()
            .and_then(|children| {
                children
                    .iter()
                    .find(|symbol| symbol.name.starts_with("while "))
            })
            .expect("while scope");
        assert!(
            while_scope
                .children
                .as_ref()
                .is_some_and(|children| children.iter().any(|symbol| symbol.name == "begin")),
            "while body should retain its nested begin scope"
        );

        assert_eq!(begin.selection_range.start.line, 5);
        assert_eq!(if_scope.selection_range.start.line, 6);
        assert_eq!(while_scope.selection_range.start.line, 7);
    }

    #[test]
    fn test_procedure_detail_params_and_return_type() {
        let src = r#"codeunit 50100 Test
{
    procedure NoReturn(x: Integer)
    begin
    end;

    procedure WithReturn(a: Integer; b: Code[20]): Boolean
    begin
    end;
}"#;
        let symbols = parse_symbols(src);
        let no_return = find_sym(&symbols, "NoReturn").expect("NoReturn proc");
        assert_eq!(no_return.kind, SymbolKind::Function);
        let detail = no_return.detail.as_deref().unwrap();
        assert!(detail.contains("Integer"), "got detail {:?}", detail);
        assert!(!detail.contains(':') || detail.starts_with('('));

        let with_return = find_sym(&symbols, "WithReturn").expect("WithReturn proc");
        let detail = with_return.detail.as_deref().unwrap();
        assert!(
            detail.ends_with(": Boolean"),
            "return type should be in detail, got {:?}",
            detail
        );
    }

    #[test]
    fn test_event_procedure_symbol() {
        // [IntegrationEvent] decorated procedures parse as procedure_declaration;
        // ensure they still surface as a Function symbol with param detail.
        let src = r#"codeunit 50100 Test
{
    [IntegrationEvent(false, false)]
    procedure OnBeforeFoo(var Handled: Boolean)
    begin
    end;
}"#;
        let symbols = parse_symbols(src);
        let ev = find_sym(&symbols, "OnBeforeFoo").expect("event procedure");
        assert!(matches!(ev.kind, SymbolKind::Function | SymbolKind::Event));
        assert!(ev.detail.as_deref().unwrap().contains("Handled"));
    }

    #[test]
    fn test_event_decorated_procedure_surfaces_with_param_detail() {
        // A [BusinessEvent]-decorated procedure still surfaces as a symbol with
        // parameter detail (procedure / event extraction path).
        let src = r#"codeunit 50100 "Pub"
{
    [BusinessEvent(false)]
    procedure OnSomethingHappened(Sender: Integer)
    begin
    end;
}"#;
        let symbols = parse_symbols(src);
        let ev = find_sym(&symbols, "OnSomethingHappened").expect("event procedure");
        assert!(matches!(ev.kind, SymbolKind::Function | SymbolKind::Event));
        assert!(ev.detail.as_deref().unwrap().contains("Sender"));
    }

    #[test]
    fn test_extract_event_symbol_direct() {
        // event_declaration is not emitted by the current grammar, but the
        // helper is a thin wrapper over extract_named_symbol. Drive it via a
        // procedure_declaration node (same field layout: name/parameters) to
        // prove it produces an Event symbol with the "event" detail and reads
        // the name field correctly.
        let (tree, src) = parse_tree(
            "codeunit 50100 C\n{\n    procedure DoIt(x: Integer)\n    begin\n    end;\n}",
        );
        let proc = find_node_of_kind(tree.root_node(), "procedure_declaration")
            .expect("procedure_declaration");
        let sym = extract_event_symbol(proc, src.as_bytes()).expect("named symbol");
        assert_eq!(sym.name, "DoIt");
        assert_eq!(sym.kind, SymbolKind::Event);
        assert_eq!(sym.detail.as_deref(), Some("event"));
    }
}
