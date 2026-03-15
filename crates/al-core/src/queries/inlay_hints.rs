//! Inlay hints query.
//!
//! Provides parameter name hints for function calls. Resolves overloads
//! using type-aware scoring when multiple signatures exist.

use al_syntax::AlParser;
use tower_lsp::lsp_types::{self, DocumentSymbol, InlayHint, InlayHintKind, InlayHintLabel, Position, Range, SymbolKind};
use url::Url;

use crate::workspace::Workspace;

/// Get inlay hints for a range within a document.
pub fn inlay_hints(workspace: &Workspace, uri: &Url, range: lsp_types::Range) -> Option<Vec<InlayHint>> {
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut hints = Vec::new();
    let doc_symbols = al_syntax::extract_document_symbols(&tree, &text);

    collect_inlay_hints(root, source, &text, &tree, workspace, &doc_symbols, &range, &mut hints);

    if hints.is_empty() { None } else { Some(hints) }
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
    if node_end < range.start.line || node_start > range.end.line { return; }

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
                    workspace, doc_symbols, &func_name, receiver_name.as_deref(),
                    text, tree, position, &arg_types,
                );
                if !param_names.is_empty() {
                    add_parameter_hints(node, source, &param_names, hints);
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_inlay_hints(child, source, text, tree, workspace, doc_symbols, range, hints);
    }
}

#[derive(Debug, Clone)]
struct InferredType { base: String, subtype: Option<String> }

struct OverloadCandidate { names: Vec<String>, types: Vec<String> }

fn infer_argument_type(
    node: tree_sitter::Node<'_>, source: &[u8], text: &str,
    tree: &tree_sitter::Tree, position: Position,
) -> Option<InferredType> {
    // Non-UTF8 node text means invalid expression — skip
    let expr = node.utf8_text(source).unwrap_or("");
    let expr = expr.trim();

    if let Some(idx) = expr.find("::") {
        let base = expr[..idx].trim().trim_matches('"');
        if !base.is_empty() {
            return Some(InferredType { base: base.to_string(), subtype: None });
        }
    }
    if expr.starts_with('\'') {
        return Some(InferredType { base: "Text".to_string(), subtype: None });
    }
    if !expr.is_empty() && expr.bytes().next().is_some_and(|b| b.is_ascii_digit() || b == b'-') {
        let numeric_part = expr.trim_start_matches('-');
        if numeric_part.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            let base = if expr.contains('.') { "Decimal" } else { "Integer" };
            return Some(InferredType { base: base.to_string(), subtype: None });
        }
    }
    if expr.eq_ignore_ascii_case("true") || expr.eq_ignore_ascii_case("false") {
        return Some(InferredType { base: "Boolean".to_string(), subtype: None });
    }
    let var_name = expr.trim_matches('"');
    let resolver = al_syntax::TypeResolver::new(tree, text);
    if let Some(decl) = resolver.resolve_type(var_name, position) {
        return Some(InferredType { base: decl.type_name, subtype: decl.type_subtype });
    }
    None
}

fn infer_argument_types(
    arg_list: tree_sitter::Node<'_>, source: &[u8], text: &str,
    tree: &tree_sitter::Tree, position: Position,
) -> Vec<Option<InferredType>> {
    let expr_parent = arg_list.children(&mut arg_list.walk())
        .find(|c| c.kind() == "expression_list")
        .unwrap_or(arg_list);
    let mut cursor = expr_parent.walk();
    let mut types = Vec::new();
    for child in expr_parent.children(&mut cursor) {
        let kind = child.kind();
        if !child.is_named() || kind == "comma" || kind == "(" || kind == ")" || kind == "semicolon" { continue; }
        types.push(infer_argument_type(child, source, text, tree, position));
    }
    types
}

fn parse_type_string(type_str: &str) -> (&str, Option<&str>) {
    let trimmed = type_str.trim();
    if let Some(quote_start) = trimmed.find("\\\"") {
        let base = trimmed[..quote_start].trim();
        let rest = &trimmed[quote_start + 2..];
        if let Some(quote_end) = rest.find("\\\"") {
            return (base, Some(&rest[..quote_end]));
        }
    }
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
    if candidate.types.len() == arg_count { score += 1000; }
    else if candidate.types.len() > arg_count { score += 100; }
    for (i, arg_type) in arg_types.iter().enumerate() {
        if let Some(param_type_str) = candidate.types.get(i) {
            if let Some(inferred) = arg_type {
                let (param_base, param_subtype) = parse_type_string(param_type_str);
                if param_base.eq_ignore_ascii_case(&inferred.base) {
                    score += 50;
                    if let (Some(p_sub), Some(a_sub)) = (param_subtype, inferred.subtype.as_deref()) {
                        if p_sub.eq_ignore_ascii_case(a_sub) { score += 25; }
                    }
                }
            }
        }
    }
    score
}

fn select_best_overload(candidates: &[OverloadCandidate], arg_types: &[Option<InferredType>]) -> Option<Vec<String>> {
    candidates.iter().max_by_key(|c| score_overload(c, arg_types)).map(|c| c.names.clone())
}

fn extract_call_info(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "member_call_suffix" | "scope_call_suffix" => {
            let member = node.child_by_field_name("member")?;
            let method_name = member.utf8_text(source).unwrap_or("").trim_matches('"').to_string();
            let receiver = extract_receiver_before(node, source);
            Some((method_name, receiver))
        }
        "call_suffix" => {
            if let Some(prev) = node.prev_sibling() {
                let name = prev.utf8_text(source).unwrap_or("").trim_matches('"').to_string();
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
        "primary_expression" => Some(prev.utf8_text(source).unwrap_or("").trim_matches('"').to_string()),
        "member_suffix" | "member_call_suffix" => {
            let member = prev.child_by_field_name("member")?;
            Some(member.utf8_text(source).unwrap_or("").trim_matches('"').to_string())
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
    let mut candidates: Vec<OverloadCandidate> = Vec::new();
    for sym in doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && (child.kind == SymbolKind::FUNCTION || child.kind == SymbolKind::EVENT)
                {
                    if let Some(detail) = &child.detail {
                        let params = parse_parameters_from_detail(detail);
                        if !params.is_empty() {
                            candidates.push(OverloadCandidate {
                                names: params.iter().map(|(n, _)| n.clone()).collect(),
                                types: params.iter().map(|(_, t)| t.clone()).collect(),
                            });
                        }
                    }
                }
            }
        }
    }
    if !candidates.is_empty() {
        if let Some(best) = select_best_overload(&candidates, arg_types) { return best; }
    }

    // 2. Receiver type resolution
    if let Some(recv) = receiver_name {
        if let Some(names) = lookup_via_receiver(workspace, func_name, recv, text, tree, position, arg_types) {
            return names;
        }
    }

    // 3. Package symbols
    let symbols = workspace.symbols.search(func_name, 5);
    let candidates: Vec<OverloadCandidate> = symbols.iter()
        .flat_map(|e| e.methods.iter())
        .filter(|m| m.name.eq_ignore_ascii_case(func_name))
        .map(|m| OverloadCandidate {
            names: m.parameters.iter().map(|p| p.name.clone()).collect(),
            types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
        }).collect();
    if let Some(best) = select_best_overload(&candidates, arg_types) { return best; }

    // 4. Builtins
    let builtins = workspace.builtins.read().unwrap().clone();
    let candidates: Vec<OverloadCandidate> = builtins.iter()
        .flat_map(|bt| bt.methods.iter())
        .filter(|m| m.name.eq_ignore_ascii_case(func_name))
        .map(|m| OverloadCandidate {
            names: m.parameters.iter().map(|p| p.name.clone()).collect(),
            types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
        }).collect();
    if let Some(best) = select_best_overload(&candidates, arg_types) { return best; }

    // 5. Embedded builtins
    if let Some(names) = lookup_embedded_builtin(func_name) { return names; }

    Vec::new()
}

fn lookup_via_receiver(
    workspace: &Workspace, func_name: &str, receiver_name: &str,
    text: &str, tree: &tree_sitter::Tree, position: Position,
    arg_types: &[Option<InferredType>],
) -> Option<Vec<String>> {
    let resolver = al_syntax::TypeResolver::new(tree, text);
    let decl = resolver.resolve_type(receiver_name, position)?;

    // Builtins filtered by receiver type
    let builtins = workspace.builtins.read().unwrap().clone();
    let candidates: Vec<OverloadCandidate> = builtins.iter()
        .filter(|bt| bt.name.eq_ignore_ascii_case(&decl.type_name)
            || decl.type_subtype.as_deref().is_some_and(|s| bt.name.eq_ignore_ascii_case(s)))
        .flat_map(|bt| bt.methods.iter())
        .filter(|m| m.name.eq_ignore_ascii_case(func_name))
        .map(|m| OverloadCandidate {
            names: m.parameters.iter().map(|p| p.name.clone()).collect(),
            types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
        }).collect();
    if let Some(best) = select_best_overload(&candidates, arg_types) { return Some(best); }

    // Package symbols by resolved subtype
    if let Some(subtype) = &decl.type_subtype {
        let candidates: Vec<OverloadCandidate> = workspace.symbols.get_by_name(subtype).iter()
            .flat_map(|e| e.methods.iter())
            .filter(|m| m.name.eq_ignore_ascii_case(func_name))
            .map(|m| OverloadCandidate {
                names: m.parameters.iter().map(|p| p.name.clone()).collect(),
                types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
            }).collect();
        if let Some(best) = select_best_overload(&candidates, arg_types) { return Some(best); }
    }

    // Workspace objects by resolved subtype
    if let Some(subtype) = &decl.type_subtype {
        let obj_key = subtype.to_lowercase();
        if let Some(file_path) = workspace.file_index.objects.get(&obj_key) {
            let file_path = file_path.value().clone();
            if let Some(file_text) = workspace.file_index.files.get(&file_path) {
                let content = file_text.value();
                let result = AlParser::parse_quick(content);
                let target_symbols = al_syntax::extract_document_symbols(&result.tree, content);
                let mut candidates: Vec<OverloadCandidate> = Vec::new();
                for sym in &target_symbols {
                    if let Some(children) = &sym.children {
                        for child in children {
                            if child.name.eq_ignore_ascii_case(func_name)
                                && (child.kind == SymbolKind::FUNCTION || child.kind == SymbolKind::EVENT)
                            {
                                if let Some(detail) = &child.detail {
                                    let params = parse_parameters_from_detail(detail);
                                    if !params.is_empty() {
                                        candidates.push(OverloadCandidate {
                                            names: params.iter().map(|(n, _)| n.clone()).collect(),
                                            types: params.iter().map(|(_, t)| t.clone()).collect(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(best) = select_best_overload(&candidates, arg_types) { return Some(best); }
            }
        }
    }

    lookup_embedded_builtin(func_name)
}

fn lookup_embedded_builtin(func_name: &str) -> Option<Vec<String>> {
    let names: &[&str] = match func_name.to_lowercase().as_str() {
        "message" | "error" => &["Value"],
        "confirm" => &["Question"],
        "strmenu" => &["OptionString"],
        "format" => &["Value"],
        "strlen" | "maxstrlen" | "get" | "findset" | "findfirst" | "findlast"
        | "getposition" | "count" | "isempty" | "reset" | "setrecfilter" => return Some(vec![]),
        "copystr" => &["Position", "Length"],
        "selectstr" => &["Number", "CommaString"],
        "strpos" => &["SubString"],
        "contains" | "startswith" | "endswith" => &["Value"],
        "setrange" => &["FieldNo", "FromValue", "ToValue"],
        "setfilter" => &["FieldNo", "String"],
        "insert" | "modify" | "delete" => &["RunTrigger"],
        "fieldno" => &["FieldName"],
        "setposition" => &["Position"],
        "read" | "write" => &["Value"],
        "run" | "setrecord" | "getrecord" => &["Record"],
        _ => return None,
    };
    Some(names.iter().map(|s| s.to_string()).collect())
}

fn parse_parameters_from_detail(detail: &str) -> Vec<(String, String)> {
    let trimmed = detail.trim();
    let start = match trimmed.find('(') { Some(i) => i + 1, None => return Vec::new() };
    let mut depth = 1;
    let mut end = start;
    for (i, ch) in trimmed[start..].char_indices() {
        match ch { '(' => depth += 1, ')' => { depth -= 1; if depth == 0 { end = start + i; break; } } _ => {} }
    }
    let params_str = &trimmed[start..end];
    if params_str.trim().is_empty() { return Vec::new(); }
    params_str.split(';').filter_map(|param| {
        let param = param.trim();
        if param.is_empty() { return None; }
        let param = param.strip_prefix("var ").unwrap_or(param).trim();
        if let Some(colon_pos) = param.find(':') {
            let name = param[..colon_pos].trim().trim_matches('"');
            let type_name = param[colon_pos + 1..].trim();
            if !name.is_empty() { return Some((name.to_string(), type_name.to_string())); }
        }
        let name = param.trim().trim_matches('"');
        if !name.is_empty() { Some((name.to_string(), String::new())) } else { None }
    }).collect()
}

fn add_parameter_hints(
    arg_list: tree_sitter::Node<'_>,
    _source: &[u8],
    param_names: &[String],
    hints: &mut Vec<InlayHint>,
) {
    let expr_parent = arg_list.children(&mut arg_list.walk())
        .find(|c| c.kind() == "expression_list")
        .unwrap_or(arg_list);
    let mut cursor = expr_parent.walk();
    let mut arg_idx = 0;
    for child in expr_parent.children(&mut cursor) {
        let kind = child.kind();
        if !child.is_named() || kind == "comma" || kind == "(" || kind == ")" || kind == "semicolon" { continue; }
        if arg_idx >= param_names.len() { break; }
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
