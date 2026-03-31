//! Inlay hints query.
//!
//! Provides parameter name hints for function calls. Resolves overloads
//! using type-aware scoring when multiple signatures exist.
//!
//! Also provides return type hints for procedure declarations when
//! `al.inlayhints.returnTypes` is enabled.

use tower_lsp::lsp_types::{
    self, DocumentSymbol, InlayHint, InlayHintKind, InlayHintLabel, Position, Range,
};
use url::Url;

use crate::workspace::Workspace;

/// Get inlay hints for a range within a document.
pub fn inlay_hints(
    workspace: &Workspace,
    uri: &Url,
    range: lsp_types::Range,
) -> Option<Vec<InlayHint>> {
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut hints = Vec::new();

    let (param_hints, return_hints) = match workspace.config.try_read() {
        Ok(config) => (
            config.inlay_hints.parameter_names,
            config.inlay_hints.return_types,
        ),
        Err(_) => (true, false), // defaults if lock is held
    };

    if param_hints {
        let doc_symbols = al_syntax::extract_document_symbols(&tree, &text);
        collect_inlay_hints(
            root,
            source,
            &text,
            &tree,
            workspace,
            &doc_symbols,
            &range,
            &mut hints,
        );
    }

    if return_hints {
        collect_return_type_hints(root, source, &range, &mut hints);
    }

    if hints.is_empty() {
        None
    } else {
        Some(hints)
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_inlay_hints(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    text: &str,
    tree: &tree_sitter::Tree,
    workspace: &Workspace,
    doc_symbols: &[DocumentSymbol],
    range: &Range,
    hints: &mut Vec<InlayHint>,
) {
    let node_start = node.start_position().row as u32;
    let node_end = node.end_position().row as u32;
    if node_end < range.start.line || node_start > range.end.line {
        return;
    }

    if node.kind() == "argument_list" || node.kind() == "call_arguments" {
        if let Some(parent) = node.parent() {
            let call_info = extract_call_info(parent, source);
            if let Some((func_name, receiver_name)) = call_info {
                let position = Position {
                    line: node.start_position().row as u32,
                    character: node.start_position().column as u32,
                };
                let arg_types = infer_argument_types(node, source, text, tree, position);
                let param_names = lookup_parameter_names(
                    workspace,
                    doc_symbols,
                    &func_name,
                    receiver_name.as_deref(),
                    text,
                    tree,
                    position,
                    &arg_types,
                );
                if !param_names.is_empty() {
                    add_parameter_hints(node, source, &param_names, hints);
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_inlay_hints(
            child,
            source,
            text,
            tree,
            workspace,
            doc_symbols,
            range,
            hints,
        );
    }
}

#[derive(Debug, Clone)]
struct InferredType {
    base: String,
    subtype: Option<String>,
}

struct OverloadCandidate {
    names: Vec<String>,
    types: Vec<String>,
}

fn infer_argument_type(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    text: &str,
    tree: &tree_sitter::Tree,
    position: Position,
) -> Option<InferredType> {
    // Non-UTF8 node text means invalid expression — skip
    let expr = node.utf8_text(source).unwrap_or("");
    let expr = expr.trim();

    if let Some(idx) = expr.find("::") {
        let base = expr[..idx].trim().trim_matches('"');
        if !base.is_empty() {
            return Some(InferredType {
                base: base.to_string(),
                subtype: None,
            });
        }
    }
    if expr.starts_with('\'') {
        return Some(InferredType {
            base: "Text".to_string(),
            subtype: None,
        });
    }
    if !expr.is_empty()
        && expr
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_digit() || b == b'-')
    {
        let numeric_part = expr.trim_start_matches('-');
        if numeric_part
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
        {
            let base = if expr.contains('.') {
                "Decimal"
            } else {
                "Integer"
            };
            return Some(InferredType {
                base: base.to_string(),
                subtype: None,
            });
        }
    }
    if expr.eq_ignore_ascii_case("true") || expr.eq_ignore_ascii_case("false") {
        return Some(InferredType {
            base: "Boolean".to_string(),
            subtype: None,
        });
    }
    let var_name = expr.trim_matches('"');
    let resolver = al_syntax::TypeResolver::new(tree, text);
    if let Some(decl) = resolver.resolve_type(var_name, position) {
        return Some(InferredType {
            base: decl.type_name,
            subtype: decl.type_subtype,
        });
    }
    None
}

fn infer_argument_types(
    arg_list: tree_sitter::Node<'_>,
    source: &[u8],
    text: &str,
    tree: &tree_sitter::Tree,
    position: Position,
) -> Vec<Option<InferredType>> {
    let expr_parent = arg_list
        .children(&mut arg_list.walk())
        .find(|c| c.kind() == "expression_list")
        .unwrap_or(arg_list);
    let mut cursor = expr_parent.walk();
    let mut types = Vec::new();
    for child in expr_parent.children(&mut cursor) {
        let kind = child.kind();
        if !child.is_named() || kind == "comma" || kind == "(" || kind == ")" || kind == "semicolon"
        {
            continue;
        }
        types.push(infer_argument_type(child, source, text, tree, position));
    }
    types
}

fn parse_type_string(type_str: &str) -> (&str, Option<&str>) {
    let trimmed = type_str.trim();
    if let Some(quote_start) = trimmed.find('"') {
        let base = trimmed[..quote_start].trim();
        let rest = &trimmed[quote_start + 1..];
        if let Some(quote_end) = rest.find('"') {
            return (base, Some(&rest[..quote_end]));
        }
    }
    (trimmed.split_whitespace().next().unwrap_or(trimmed), None)
}

fn score_overload(candidate: &OverloadCandidate, arg_types: &[Option<InferredType>]) -> u32 {
    let arg_count = arg_types.len();
    let mut score = 0u32;
    if candidate.types.len() == arg_count {
        score += 1000;
    } else if candidate.types.len() > arg_count {
        score += 100;
    }
    for (i, arg_type) in arg_types.iter().enumerate() {
        if let Some(param_type_str) = candidate.types.get(i) {
            if let Some(inferred) = arg_type {
                let (param_base, param_subtype) = parse_type_string(param_type_str);
                if param_base.eq_ignore_ascii_case(&inferred.base) {
                    score += 50;
                    if let (Some(p_sub), Some(a_sub)) = (param_subtype, inferred.subtype.as_deref())
                    {
                        if p_sub.eq_ignore_ascii_case(a_sub) {
                            score += 25;
                        }
                    }
                }
            }
        }
    }
    score
}

fn select_best_overload(
    candidates: &[OverloadCandidate],
    arg_types: &[Option<InferredType>],
) -> Option<Vec<String>> {
    candidates
        .iter()
        .max_by_key(|c| score_overload(c, arg_types))
        .map(|c| c.names.clone())
}

fn extract_call_info(
    node: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<(String, Option<String>)> {
    match node.kind() {
        "member_call_suffix" | "scope_call_suffix" => {
            let member = node.child_by_field_name("member")?;
            let method_name = member
                .utf8_text(source)
                .unwrap_or("")
                .trim_matches('"')
                .to_string();
            let receiver = extract_receiver_before(node, source);
            Some((method_name, receiver))
        }
        "call_suffix" => {
            if let Some(prev) = node.prev_sibling() {
                let name = prev
                    .utf8_text(source)
                    .unwrap_or("")
                    .trim_matches('"')
                    .to_string();
                return Some((name, None));
            }
            None
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                let kind = child.kind();
                if kind == "identifier" || kind == "quoted_identifier" || kind == "name" {
                    let t = child.utf8_text(source).unwrap_or("");
                    return Some((t.trim_matches('"').to_string(), None));
                }
            }
            None
        }
    }
}

fn extract_receiver_before(suffix_node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let prev = suffix_node.prev_sibling()?;
    match prev.kind() {
        "primary_expression" => Some(
            prev.utf8_text(source)
                .unwrap_or("")
                .trim_matches('"')
                .to_string(),
        ),
        "member_suffix" | "member_call_suffix" => {
            let member = prev.child_by_field_name("member")?;
            Some(
                member
                    .utf8_text(source)
                    .unwrap_or("")
                    .trim_matches('"')
                    .to_string(),
            )
        }
        _ => None,
    }
}

#[allow(deprecated)]
#[allow(clippy::too_many_arguments)]
fn lookup_parameter_names(
    workspace: &Workspace,
    doc_symbols: &[DocumentSymbol],
    func_name: &str,
    receiver_name: Option<&str>,
    text: &str,
    tree: &tree_sitter::Tree,
    position: Position,
    arg_types: &[Option<InferredType>],
) -> Vec<String> {
    // 1. Local procedures
    let candidates = overload_candidates_from_symbols(doc_symbols, func_name);
    if !candidates.is_empty() {
        if let Some(best) = select_best_overload(&candidates, arg_types) {
            return best;
        }
    }

    // 2. Receiver type resolution
    if let Some(recv) = receiver_name {
        if let Some(names) =
            lookup_via_receiver(workspace, func_name, recv, text, tree, position, arg_types)
        {
            return names;
        }
    }

    // 3. Package symbols
    let symbols = workspace.symbols.get_by_name(func_name);
    let candidates: Vec<OverloadCandidate> = symbols
        .iter()
        .flat_map(|e| e.methods.iter())
        .filter(|m| m.name.eq_ignore_ascii_case(func_name))
        .map(|m| OverloadCandidate {
            names: m.parameters.iter().map(|p| p.name.clone()).collect(),
            types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
        })
        .collect();
    if let Some(best) = select_best_overload(&candidates, arg_types) {
        return best;
    }

    // 4. Builtins
    let builtins = workspace.builtins.read().unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    let candidates: Vec<OverloadCandidate> = builtins
        .iter()
        .flat_map(|bt| bt.methods.iter())
        .filter(|m| m.name.eq_ignore_ascii_case(func_name))
        .map(|m| OverloadCandidate {
            names: m.parameters.iter().map(|p| p.name.clone()).collect(),
            types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
        })
        .collect();
    drop(builtins); // release read lock before returning
    if let Some(best) = select_best_overload(&candidates, arg_types) {
        return best;
    }

    // 5. Embedded builtins
    if let Some(names) = lookup_embedded_builtin(func_name) {
        return names;
    }

    Vec::new()
}

fn lookup_via_receiver(
    workspace: &Workspace,
    func_name: &str,
    receiver_name: &str,
    text: &str,
    tree: &tree_sitter::Tree,
    position: Position,
    arg_types: &[Option<InferredType>],
) -> Option<Vec<String>> {
    let resolver = al_syntax::TypeResolver::new(tree, text);
    let decl = resolver.resolve_type(receiver_name, position)?;

    // Builtins filtered by receiver type — use semantic_cache for O(1) type lookup
    let cache = workspace
        .semantic_cache
        .read()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    let type_names: Vec<&str> = {
        let mut names = vec![decl.type_name.as_str()];
        if let Some(sub) = decl.type_subtype.as_deref() {
            names.push(sub);
        }
        names
    };
    let candidates: Vec<OverloadCandidate> = type_names
        .iter()
        .filter_map(|tn| cache.get_type(tn))
        .flat_map(|bt| bt.methods.iter())
        .filter(|m| m.name.eq_ignore_ascii_case(func_name))
        .map(|m| OverloadCandidate {
            names: m.parameters.iter().map(|p| p.name.clone()).collect(),
            types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
        })
        .collect();
    drop(cache); // release read lock before continuing
    if let Some(best) = select_best_overload(&candidates, arg_types) {
        return Some(best);
    }

    // Package symbols by resolved subtype
    if let Some(subtype) = &decl.type_subtype {
        let candidates: Vec<OverloadCandidate> = workspace
            .symbols
            .get_by_name(subtype)
            .iter()
            .flat_map(|e| e.methods.iter())
            .filter(|m| m.name.eq_ignore_ascii_case(func_name))
            .map(|m| OverloadCandidate {
                names: m.parameters.iter().map(|p| p.name.clone()).collect(),
                types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
            })
            .collect();
        if let Some(best) = select_best_overload(&candidates, arg_types) {
            return Some(best);
        }
    }

    // Workspace objects by resolved subtype
    if let Some(subtype) = &decl.type_subtype {
        let obj_key = subtype.to_lowercase();
        if let Some(file_path) = workspace.file_index.objects.get(&obj_key) {
            let file_path = file_path.value().clone();
            // Use cached parse tree — avoids re-parsing on every inlay-hint request.
            if let Some((content, file_tree)) = workspace.file_index.get_cached_parse(&file_path) {
                let target_symbols = al_syntax::extract_document_symbols(&file_tree, &content);
                let candidates = overload_candidates_from_symbols(&target_symbols, func_name);
                if let Some(best) = select_best_overload(&candidates, arg_types) {
                    return Some(best);
                }
            }
        }
    }

    lookup_embedded_builtin(func_name)
}

fn lookup_embedded_builtin(func_name: &str) -> Option<Vec<String>> {
    let func = al_syntax::language_data::builtin_function_by_name(func_name)?;
    Some(func.parameters.iter().map(|p| p.name.clone()).collect())
}

use super::parse_detail_params;

/// Collect `OverloadCandidate` entries from a slice of `DocumentSymbol` for the given
/// function name. Shared by `lookup_parameter_names` (local file) and
/// `lookup_via_receiver` (resolved-type file) to avoid duplicating the nested loop.
#[allow(deprecated)]
fn overload_candidates_from_symbols(
    symbols: &[tower_lsp::lsp_types::DocumentSymbol],
    func_name: &str,
) -> Vec<OverloadCandidate> {
    let mut candidates = Vec::new();
    for sym in symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && super::is_procedure_symbol(child.kind.into())
                {
                    if let Some(detail) = &child.detail {
                        let params = parse_detail_params(detail);
                        if !params.is_empty() {
                            candidates.push(OverloadCandidate {
                                names: params.iter().map(|(_, n, _)| n.clone()).collect(),
                                types: params.iter().map(|(_, _, t)| t.clone()).collect(),
                            });
                        }
                    }
                }
            }
        }
    }
    candidates
}

/// Walk the tree and emit return type hints for procedures/triggers that declare
/// a return type. The hint appears immediately after the closing `)` of the
/// parameter list and shows `: <ReturnType>`.
fn collect_return_type_hints(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    range: &Range,
    hints: &mut Vec<InlayHint>,
) {
    let node_start = node.start_position().row as u32;
    let node_end = node.end_position().row as u32;
    if node_end < range.start.line || node_start > range.end.line {
        return;
    }

    let kind = node.kind();
    if kind == "procedure_declaration"
        || kind == "trigger_declaration"
        || kind == "event_procedure_declaration"
    {
        if let Some(rt_node) = node.child_by_field_name("return_type") {
            if let Ok(rt_text) = rt_node.utf8_text(source) {
                let rt_text = rt_text.trim();
                if !rt_text.is_empty() {
                    // Place the hint at the end of the parameter list (closing paren).
                    // Fall back to the name-node end position if no parameter node exists.
                    let hint_pos = node
                        .child_by_field_name("parameters")
                        .map(|p| Position {
                            line: p.end_position().row as u32,
                            character: p.end_position().column as u32,
                        })
                        .or_else(|| {
                            node.child_by_field_name("name").map(|n| Position {
                                line: n.end_position().row as u32,
                                character: n.end_position().column as u32,
                            })
                        });

                    if let Some(pos) = hint_pos {
                        if pos.line >= range.start.line && pos.line <= range.end.line {
                            hints.push(InlayHint {
                                position: pos,
                                label: InlayHintLabel::String(format!(": {rt_text}")),
                                kind: Some(InlayHintKind::TYPE),
                                text_edits: None,
                                tooltip: None,
                                padding_left: Some(true),
                                padding_right: None,
                                data: None,
                            });
                        }
                    }
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_return_type_hints(child, source, range, hints);
    }
}

fn add_parameter_hints(
    arg_list: tree_sitter::Node<'_>,
    _source: &[u8],
    param_names: &[String],
    hints: &mut Vec<InlayHint>,
) {
    let expr_parent = arg_list
        .children(&mut arg_list.walk())
        .find(|c| c.kind() == "expression_list")
        .unwrap_or(arg_list);
    let mut cursor = expr_parent.walk();
    let mut arg_idx = 0;
    for child in expr_parent.children(&mut cursor) {
        let kind = child.kind();
        if !child.is_named() || kind == "comma" || kind == "(" || kind == ")" || kind == "semicolon"
        {
            continue;
        }
        if arg_idx >= param_names.len() {
            break;
        }
        hints.push(InlayHint {
            position: Position {
                line: child.start_position().row as u32,
                character: child.start_position().column as u32,
            },
            label: InlayHintLabel::String(format!("{}:", param_names[arg_idx])),
            kind: Some(InlayHintKind::PARAMETER),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: Some(true),
            data: None,
        });
        arg_idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_syntax::AlParser;

    fn full_range() -> Range {
        Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 999,
                character: 0,
            },
        }
    }

    fn parse(src: &str) -> (String, tree_sitter::Tree) {
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        (src.to_string(), result.tree)
    }

    #[test]
    fn return_type_hint_for_boolean_procedure() {
        let src = r#"codeunit 50100 Test
{
    procedure IsValid(Customer: Record Customer): Boolean
    begin
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let mut hints = Vec::new();
        collect_return_type_hints(tree.root_node(), source, &full_range(), &mut hints);

        assert_eq!(
            hints.len(),
            1,
            "Expected one return type hint, got: {hints:?}"
        );
        let h = &hints[0];
        assert_eq!(h.kind, Some(InlayHintKind::TYPE));
        match &h.label {
            InlayHintLabel::String(s) => assert_eq!(s, ": Boolean"),
            _ => panic!("Unexpected label type"),
        }
    }

    #[test]
    fn no_hint_for_void_procedure() {
        let src = r#"codeunit 50100 Test
{
    procedure DoWork()
    begin
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let mut hints = Vec::new();
        collect_return_type_hints(tree.root_node(), source, &full_range(), &mut hints);

        assert!(
            hints.is_empty(),
            "Void procedure should not get return type hint"
        );
    }

    #[test]
    fn return_type_hint_with_record_type() {
        let src = r#"codeunit 50100 Test
{
    procedure GetHeader(No: Code[20]): Record "Sales Header"
    begin
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let mut hints = Vec::new();
        collect_return_type_hints(tree.root_node(), source, &full_range(), &mut hints);

        assert_eq!(hints.len(), 1);
        match &hints[0].label {
            InlayHintLabel::String(s) => {
                assert!(s.contains("Record"), "Expected Record in hint: {s}")
            }
            _ => panic!("Unexpected label type"),
        }
    }

    #[test]
    fn multiple_procedures_only_returning_ones_get_hints() {
        let src = r#"codeunit 50100 Test
{
    procedure GetCount(): Integer
    begin
    end;

    procedure DoWork()
    begin
    end;

    procedure GetName(): Text[50]
    begin
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let mut hints = Vec::new();
        collect_return_type_hints(tree.root_node(), source, &full_range(), &mut hints);

        assert_eq!(
            hints.len(),
            2,
            "Expected 2 hints (Integer + Text), got: {hints:?}"
        );
    }

    #[test]
    fn range_filter_excludes_out_of_range_procedures() {
        let src = r#"codeunit 50100 Test
{
    procedure First(): Integer
    begin
    end;

    procedure Second(): Boolean
    begin
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let mut hints = Vec::new();
        // Only the first few lines — First() is at line 2, Second() is at line 7
        let narrow_range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 3,
                character: 0,
            },
        };
        collect_return_type_hints(tree.root_node(), source, &narrow_range, &mut hints);

        assert_eq!(
            hints.len(),
            1,
            "Expected only First() hint in narrow range, got: {hints:?}"
        );
    }
}
