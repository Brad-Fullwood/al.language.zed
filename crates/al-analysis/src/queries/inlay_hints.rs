//! Inlay hints query.
//!
//! Provides parameter name hints for function calls. Resolves overloads
//! using type-aware scoring when multiple signatures exist.
//!
//! Also provides return type hints for procedure declarations when
//! `al.inlayhints.returnTypes` is enabled.

use url::Url;

use crate::queries::{AlInlayHint, AlInlayHintKind, AlInlayHintLabel, Position, Range};
use al_workspace::Workspace;

/// Returns transport-agnostic `AlInlayHint` values; al-lsp converts at the boundary.
#[must_use]
pub fn inlay_hints(workspace: &Workspace, uri: &Url, range: Range) -> Option<Vec<AlInlayHint>> {
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
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
        // Prefer the cached symbols already populated by the file index to
        // avoid re-extracting on every keystroke. Fall back to a fresh
        // extraction only when the cache is empty (e.g., a virtual document
        // not stored on disk).
        let file_path = uri.to_file_path().ok();
        let doc_symbols: Vec<super::AlDocumentSymbol> = file_path
            .as_ref()
            .and_then(|p| workspace.file_index.get_cached_symbols(p))
            .map(|syms| syms.into_iter().map(Into::into).collect())
            .unwrap_or_else(|| {
                al_syntax::extract_document_symbols(&tree, &text)
                    .into_iter()
                    .map(Into::into)
                    .collect()
            });
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

// Eight arguments is past the clippy threshold but each one is genuinely
// independent — tree-sitter root / raw source bytes / rope text / tree
// handle / workspace / pre-computed doc symbols / requested range /
// output buffer — and bundling them into a Context struct would only
// move the cognitive load, not reduce it. We accept the lint here.
#[allow(clippy::too_many_arguments)]
fn collect_inlay_hints(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    text: &str,
    tree: &tree_sitter::Tree,
    workspace: &Workspace,
    doc_symbols: &[super::AlDocumentSymbol],
    range: &Range,
    hints: &mut Vec<AlInlayHint>,
) {
    // TypeResolver is built once and shared across all argument nodes; a fresh
    // resolver per argument would mean N full-AST scans per inlay-hints request.
    let resolver = al_syntax::TypeResolver::new(tree, text);

    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let node_start = node.start_position().row as u32;
        // tree-sitter end_position().row is exclusive; use `<=` not `<` or a
        // node ending exactly one row before the range is wrongly visited.
        let node_end = node.end_position().row as u32;
        if node_end <= range.start.line || node_start > range.end.line {
            continue;
        }

        if node.kind() == "argument_list" || node.kind() == "call_arguments" {
            if let Some(parent) = node.parent() {
                let call_info = extract_call_info(parent, source);
                if let Some((func_name, receiver_name)) = call_info {
                    let position = Position {
                        line: node.start_position().row as u32,
                        character: al_syntax::byte_col_to_utf16_col(
                            source_line(source, node.start_position().row),
                            node.start_position().column,
                        ),
                    };
                    let arg_types = infer_argument_types(node, source, &resolver, position);
                    let param_names = lookup_parameter_names(
                        workspace,
                        doc_symbols,
                        &func_name,
                        receiver_name.as_deref(),
                        text,
                        tree,
                        &resolver,
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
        stack.extend(node.children(&mut cursor));
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
    resolver: &al_syntax::TypeResolver<'_>,
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
    if let Some(decl) = resolver.resolve_type(var_name, position.into()) {
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
    resolver: &al_syntax::TypeResolver<'_>,
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
        types.push(infer_argument_type(child, source, resolver, position));
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
            // Invalid UTF-8 (or an empty name after trimming quotes) is not a
            // usable function name — bail out instead of running the whole
            // lookup pipeline with "".
            let method_name = member.utf8_text(source).ok()?.trim_matches('"');
            if method_name.is_empty() {
                return None;
            }
            let receiver = extract_receiver_before(node, source);
            Some((method_name.to_string(), receiver))
        }
        "call_suffix" => {
            let prev = node.prev_sibling()?;
            let name = prev.utf8_text(source).ok()?.trim_matches('"');
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), None))
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                let kind = child.kind();
                if kind == "identifier" || kind == "quoted_identifier" || kind == "name" {
                    let t = child.utf8_text(source).ok()?.trim_matches('"');
                    if t.is_empty() {
                        return None;
                    }
                    return Some((t.to_string(), None));
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

// Two allows: (1) `lsp_types::*` deprecation around inlay-hint label parts
// in older tower-lsp versions — we can't avoid the API; (2) eight unrelated
// inputs (workspace, source/tree/cursor context, resolver state) that don't
// gain clarity from being bundled.
#[allow(deprecated)]
#[allow(clippy::too_many_arguments)]
fn lookup_parameter_names(
    workspace: &Workspace,
    doc_symbols: &[super::AlDocumentSymbol],
    func_name: &str,
    receiver_name: Option<&str>,
    text: &str,
    tree: &tree_sitter::Tree,
    resolver: &al_syntax::TypeResolver<'_>,
    position: Position,
    arg_types: &[Option<InferredType>],
) -> Vec<String> {
    let candidates = overload_candidates_from_symbols(doc_symbols, func_name);
    if !candidates.is_empty() {
        if let Some(best) = select_best_overload(&candidates, arg_types) {
            return best;
        }
    }

    if let Some(recv) = receiver_name {
        if let Some(names) = lookup_via_receiver(
            workspace, func_name, recv, text, tree, resolver, position, arg_types,
        ) {
            return names;
        }
    }

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
    drop(builtins);
    if let Some(best) = select_best_overload(&candidates, arg_types) {
        return best;
    }

    if let Some(names) = lookup_embedded_builtin(func_name) {
        return names;
    }

    Vec::new()
}

// Member-call resolution needs workspace + func + receiver + the tree-walk
// state. Bundling into a struct adds an indirection layer without removing
// any of the inputs.
#[allow(clippy::too_many_arguments)]
fn lookup_via_receiver(
    workspace: &Workspace,
    func_name: &str,
    receiver_name: &str,
    _text: &str,
    _tree: &tree_sitter::Tree,
    resolver: &al_syntax::TypeResolver<'_>,
    position: Position,
    arg_types: &[Option<InferredType>],
) -> Option<Vec<String>> {
    let decl = resolver.resolve_type(receiver_name, position.into())?;

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
    drop(cache);
    if let Some(best) = select_best_overload(&candidates, arg_types) {
        return Some(best);
    }

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

    if let Some(subtype) = &decl.type_subtype {
        let obj_key = subtype.to_lowercase();
        if let Some(file_path) = workspace.file_index.objects.get(&obj_key) {
            let file_path = file_path.value().clone();
            if let Some(target_symbols) = workspace.file_index.get_cached_symbols(&file_path) {
                let target_symbols: Vec<super::AlDocumentSymbol> =
                    target_symbols.into_iter().map(Into::into).collect();
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

/// Collect `OverloadCandidate` entries from a slice of `AlDocumentSymbol` for the given
/// function name. Shared by `lookup_parameter_names` (local file) and
/// `lookup_via_receiver` (resolved-type file) to avoid duplicating the nested loop.
fn overload_candidates_from_symbols(
    symbols: &[super::AlDocumentSymbol],
    func_name: &str,
) -> Vec<OverloadCandidate> {
    let mut candidates = Vec::new();
    for sym in symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && super::is_procedure_symbol(child.kind)
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
    root: tree_sitter::Node<'_>,
    source: &[u8],
    range: &Range,
    hints: &mut Vec<AlInlayHint>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let node_start = node.start_position().row as u32;
        // end_position().row is exclusive (see note in the argument-hints
        // walker above): use `<=` so a node ending exactly on the row before
        // the range is correctly skipped.
        let node_end = node.end_position().row as u32;
        if node_end <= range.start.line || node_start > range.end.line {
            continue;
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
                                character: al_syntax::byte_col_to_utf16_col(
                                    source_line(source, p.end_position().row),
                                    p.end_position().column,
                                ),
                            })
                            .or_else(|| {
                                node.child_by_field_name("name").map(|n| Position {
                                    line: n.end_position().row as u32,
                                    character: al_syntax::byte_col_to_utf16_col(
                                        source_line(source, n.end_position().row),
                                        n.end_position().column,
                                    ),
                                })
                            });

                        if let Some(pos) = hint_pos {
                            if pos.line >= range.start.line && pos.line <= range.end.line {
                                hints.push(AlInlayHint {
                                    position: pos,
                                    label: AlInlayHintLabel::String(format!(": {rt_text}")),
                                    kind: Some(AlInlayHintKind::Type),
                                    padding_left: Some(true),
                                    padding_right: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn add_parameter_hints(
    arg_list: tree_sitter::Node<'_>,
    source: &[u8],
    param_names: &[String],
    hints: &mut Vec<AlInlayHint>,
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
        let row = child.start_position().row;
        let line_text = source_line(source, row);
        // tree-sitter column is a UTF-8 byte offset; LSP Position uses UTF-16 code units.
        let character = al_syntax::byte_col_to_utf16_col(line_text, child.start_position().column);
        hints.push(AlInlayHint {
            position: Position {
                line: row as u32,
                character,
            },
            label: AlInlayHintLabel::String(format!("{}:", param_names[arg_idx])),
            kind: Some(AlInlayHintKind::Parameter),
            padding_left: None,
            padding_right: Some(true),
        });
        arg_idx += 1;
    }
}

/// Decode `row` (0-indexed) of `source` as UTF-8, or `""` on bad UTF-8 / OOB.
fn source_line(source: &[u8], row: usize) -> &str {
    source
        .split(|&b| b == b'\n')
        .nth(row)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
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
        assert_eq!(h.kind, Some(AlInlayHintKind::Type));
        let AlInlayHintLabel::String(s) = &h.label;
        assert_eq!(s, ": Boolean");
    }

    #[test]
    fn return_type_walker_skips_node_ending_on_row_before_range() {
        // The procedure spans rows 2..=4 (end_position().row == 5, exclusive).
        // Requesting a range starting at line 5 must skip the procedure subtree
        // entirely: tree-sitter's end row is exclusive, so a `<` boundary check
        // would wrongly descend into a node that occupies only rows up to 4.
        let src =
            "codeunit 50100 Test\n{\n    procedure IsValid(): Boolean\n    begin\n    end;\n}";
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let range = Range {
            start: Position {
                line: 5,
                character: 0,
            },
            end: Position {
                line: 5,
                character: 1,
            },
        };
        let mut hints = Vec::new();
        collect_return_type_hints(tree.root_node(), source, &range, &mut hints);
        assert!(
            hints.is_empty(),
            "procedure ending before the range must yield no hints, got: {hints:?}"
        );
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
        let AlInlayHintLabel::String(s) = &hints[0].label;
        assert!(s.contains("Record"), "Expected Record in hint: {s}");
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

    /// Walk `tree` and return the first node whose kind is `argument_list` or
    /// `call_arguments`, so tests can drive `extract_call_info` /
    /// `add_parameter_hints` against a real call site.
    fn first_arg_list<'t>(root: tree_sitter::Node<'t>) -> Option<tree_sitter::Node<'t>> {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "argument_list" || node.kind() == "call_arguments" {
                return Some(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        None
    }

    fn doc_symbols(src: &str, tree: &tree_sitter::Tree) -> Vec<crate::queries::AlDocumentSymbol> {
        al_syntax::extract_document_symbols(tree, src)
            .into_iter()
            .map(Into::into)
            .collect()
    }

    #[test]
    fn parameter_hint_local_procedure() {
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        Add(1, 2);
    end;

    procedure Add(First: Integer; Second: Integer): Integer
    begin
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let ws = Workspace::new();
        let symbols = doc_symbols(src, &tree);
        let mut hints = Vec::new();
        collect_inlay_hints(
            tree.root_node(),
            source,
            &text,
            &tree,
            &ws,
            &symbols,
            &full_range(),
            &mut hints,
        );

        let labels: Vec<String> = hints
            .iter()
            .filter(|h| h.kind == Some(AlInlayHintKind::Parameter))
            .map(|h| {
                let AlInlayHintLabel::String(s) = &h.label;
                s.clone()
            })
            .collect();
        assert_eq!(
            labels,
            vec!["First:".to_string(), "Second:".to_string()],
            "Expected First:/Second: parameter hints, got: {hints:?}"
        );
    }

    #[test]
    fn parameter_hint_overload_selection_prefers_matching_arity() {
        // Two overloads: one with a single param, one with two. A two-arg call
        // must select the two-param overload via arity scoring.
        let one = OverloadCandidate {
            names: vec!["Only".into()],
            types: vec!["Integer".into()],
        };
        let two = OverloadCandidate {
            names: vec!["First".into(), "Second".into()],
            types: vec!["Integer".into(), "Integer".into()],
        };
        let candidates = vec![one, two];
        let arg_types = vec![
            Some(InferredType {
                base: "Integer".into(),
                subtype: None,
            }),
            Some(InferredType {
                base: "Integer".into(),
                subtype: None,
            }),
        ];
        let best = select_best_overload(&candidates, &arg_types).expect("an overload");
        assert_eq!(best, vec!["First".to_string(), "Second".to_string()]);
    }

    #[test]
    fn parameter_hint_overload_selection_prefers_type_match() {
        // Same arity, differing types — the candidate whose parameter type
        // matches the inferred argument type wins on score.
        let text_sig = OverloadCandidate {
            names: vec!["Msg".into()],
            types: vec!["Text".into()],
        };
        let int_sig = OverloadCandidate {
            names: vec!["Count".into()],
            types: vec!["Integer".into()],
        };
        let candidates = vec![text_sig, int_sig];
        let arg_types = vec![Some(InferredType {
            base: "Integer".into(),
            subtype: None,
        })];
        let best = select_best_overload(&candidates, &arg_types).expect("an overload");
        assert_eq!(best, vec!["Count".to_string()]);
    }

    #[test]
    fn argument_type_inference_literals() {
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        Foo(42, 3.14, true, 'hello');
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let resolver = al_syntax::TypeResolver::new(&tree, &text);
        let arg_list = first_arg_list(tree.root_node()).expect("an argument list");
        let pos = Position {
            line: 0,
            character: 0,
        };
        let types = infer_argument_types(arg_list, source, &resolver, pos);
        let bases: Vec<Option<String>> = types
            .iter()
            .map(|t| t.as_ref().map(|i| i.base.clone()))
            .collect();
        assert_eq!(
            bases,
            vec![
                Some("Integer".to_string()),
                Some("Decimal".to_string()),
                Some("Boolean".to_string()),
                Some("Text".to_string()),
            ],
            "Literal inference mismatch: {types:?}"
        );
    }

    #[test]
    fn extract_call_info_plain_call() {
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        DoThing(1);
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let arg_list = first_arg_list(tree.root_node()).expect("an argument list");
        let parent = arg_list.parent().expect("a parent");
        let info = extract_call_info(parent, source).expect("call info");
        assert_eq!(info.0, "DoThing");
        assert_eq!(info.1, None);
    }

    #[test]
    fn extract_call_info_member_call_has_receiver() {
        let src = r#"codeunit 50100 Test
{
    procedure Caller(Rec: Record Customer)
    begin
        Rec.SetRange(1);
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let arg_list = first_arg_list(tree.root_node()).expect("an argument list");
        let parent = arg_list.parent().expect("a parent");
        let info = extract_call_info(parent, source).expect("call info");
        assert_eq!(info.0, "SetRange");
        assert_eq!(info.1.as_deref(), Some("Rec"));
    }

    #[test]
    fn extract_call_info_empty_quoted_name_returns_none() {
        // A call through an empty quoted identifier ("") trims to the empty
        // string. extract_call_info must return None rather than Some(("",..))
        // so the lookup pipeline is never run with a blank function name.
        let src = "codeunit 50100 Test\n{\n    procedure Caller()\n    begin\n        \"\"(1);\n    end;\n}";
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        if let Some(arg_list) = first_arg_list(tree.root_node()) {
            if let Some(parent) = arg_list.parent() {
                let info = extract_call_info(parent, source);
                assert!(
                    info.as_ref().map(|(n, _)| n.is_empty()) != Some(true),
                    "extract_call_info must not return an empty function name: {info:?}"
                );
            }
        }
    }

    #[test]
    fn parameter_hint_utf16_conversion() {
        // A non-ASCII identifier before the call site shifts the byte column
        // away from the UTF-16 column; the emitted hint position must use
        // UTF-16 code units, not raw bytes.
        let src = "codeunit 50100 Test\n{\n    procedure Caller()\n    var\n        Ünïcödé: Integer;\n    begin\n        Foo(Ünïcödé);\n    end;\n}";
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let arg_list = first_arg_list(tree.root_node()).expect("an argument list");
        let mut hints = Vec::new();
        add_parameter_hints(arg_list, source, &["Value".to_string()], &mut hints);
        assert_eq!(hints.len(), 1, "Expected one parameter hint: {hints:?}");
        let h = &hints[0];
        // The argument "Ünïcödé" starts at byte column 12 ("        Foo(" = 8
        // spaces + "Foo("). All ASCII before it, so UTF-16 == byte here, but
        // the conversion must not crash on the multibyte arg itself.
        assert_eq!(h.position.line, 6);
        assert_eq!(h.position.character, 12);
        let AlInlayHintLabel::String(s) = &h.label;
        assert_eq!(s, "Value:");
    }

    #[test]
    fn add_parameter_hints_stops_at_param_count() {
        // More arguments than known parameter names: only emit hints for the
        // params we know, never index out of bounds.
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        Foo(1, 2, 3);
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let arg_list = first_arg_list(tree.root_node()).expect("an argument list");
        let mut hints = Vec::new();
        add_parameter_hints(arg_list, source, &["Only".to_string()], &mut hints);
        assert_eq!(
            hints.len(),
            1,
            "Should emit only one hint for one known param: {hints:?}"
        );
    }

    #[test]
    fn parse_type_string_quoted_subtype() {
        // `Record "Sales Header"` → base "Record", subtype "Sales Header".
        let (base, sub) = parse_type_string(r#"Record "Sales Header""#);
        assert_eq!(base, "Record");
        assert_eq!(sub, Some("Sales Header"));
    }

    #[test]
    fn parse_type_string_unquoted_takes_first_word() {
        // No quote → first whitespace-delimited token, no subtype.
        let (base, sub) = parse_type_string("Integer");
        assert_eq!(base, "Integer");
        assert_eq!(sub, None);

        let (base2, sub2) = parse_type_string("  Boolean  ");
        assert_eq!(base2, "Boolean");
        assert_eq!(sub2, None);
    }

    #[test]
    fn parse_type_string_unterminated_quote_no_subtype() {
        // An opening quote with no closing quote must not panic and yields no
        // subtype; base is the text before the quote, trimmed.
        let (base, sub) = parse_type_string(r#"Record "Unterminated"#);
        assert_eq!(base, "Record");
        assert_eq!(sub, None);
    }

    #[test]
    fn score_overload_exact_arity_beats_excess_arity() {
        // Exact arity match (+1000) must outscore an over-arity candidate (+100).
        let exact = OverloadCandidate {
            names: vec!["A".into()],
            types: vec!["Integer".into()],
        };
        let excess = OverloadCandidate {
            names: vec!["A".into(), "B".into()],
            types: vec!["Integer".into(), "Integer".into()],
        };
        let arg_types = vec![Some(InferredType {
            base: "Integer".into(),
            subtype: None,
        })];
        assert!(
            score_overload(&exact, &arg_types) > score_overload(&excess, &arg_types),
            "exact-arity overload must score higher than an over-arity one"
        );
    }

    #[test]
    fn score_overload_subtype_match_adds_bonus() {
        // Same base type, but only one candidate also matches the subtype; it
        // must score 25 higher (the subtype bonus).
        let with_sub = OverloadCandidate {
            names: vec!["Rec".into()],
            types: vec![r#"Record "Customer""#.into()],
        };
        let without_sub = OverloadCandidate {
            names: vec!["Rec".into()],
            types: vec![r#"Record "Vendor""#.into()],
        };
        let arg_types = vec![Some(InferredType {
            base: "Record".into(),
            subtype: Some("Customer".into()),
        })];
        assert_eq!(
            score_overload(&with_sub, &arg_types) - score_overload(&without_sub, &arg_types),
            25,
            "matching subtype must add exactly the 25-point bonus"
        );
    }

    #[test]
    fn score_overload_fewer_params_than_args_no_arity_bonus() {
        // Candidate with fewer params than args gets neither the exact (+1000)
        // nor the over-arity (+100) bonus, and no per-arg type bonus past its
        // param count.
        let candidate = OverloadCandidate {
            names: vec!["Only".into()],
            types: vec!["Integer".into()],
        };
        let arg_types = vec![
            Some(InferredType {
                base: "Integer".into(),
                subtype: None,
            }),
            Some(InferredType {
                base: "Integer".into(),
                subtype: None,
            }),
        ];
        // Only the first arg's base matches (+50); no arity bonus.
        assert_eq!(score_overload(&candidate, &arg_types), 50);
    }

    #[test]
    fn overload_candidates_from_symbols_extracts_procedure_params() {
        // A codeunit with a parameterized procedure must yield one candidate
        // whose names/types come from the procedure's detail string.
        let src = r#"codeunit 50100 Test
{
    procedure Add(First: Integer; Second: Integer): Integer
    begin
    end;
}"#;
        let (text, tree) = parse(src);
        let symbols = doc_symbols(&text, &tree);
        let candidates = overload_candidates_from_symbols(&symbols, "Add");
        assert_eq!(candidates.len(), 1, "expected one Add overload");
        assert_eq!(candidates[0].names, vec!["First", "Second"]);
        assert_eq!(candidates[0].types, vec!["Integer", "Integer"]);
    }

    #[test]
    fn overload_candidates_from_symbols_unknown_name_is_empty() {
        let src = r#"codeunit 50100 Test
{
    procedure Add(First: Integer): Integer
    begin
    end;
}"#;
        let (text, tree) = parse(src);
        let symbols = doc_symbols(&text, &tree);
        let candidates = overload_candidates_from_symbols(&symbols, "DoesNotExist");
        assert!(
            candidates.is_empty(),
            "no procedure named DoesNotExist exists, expected no candidates"
        );
    }

    /// Build a single-argument call and return its first inferred argument type.
    fn infer_single(src: &str) -> Option<InferredType> {
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let resolver = al_syntax::TypeResolver::new(&tree, &text);
        let arg_list = first_arg_list(tree.root_node()).expect("an argument list");
        let pos = Position {
            line: 0,
            character: 0,
        };
        infer_argument_types(arg_list, source, &resolver, pos)
            .into_iter()
            .next()
            .flatten()
    }

    #[test]
    fn infer_argument_type_enum_scope_uses_left_of_double_colon() {
        // `MyEnum::Value` infers base "MyEnum" from the text left of `::`.
        let inferred = infer_single(
            "codeunit 50100 Test\n{\n    procedure Caller()\n    begin\n        Foo(MyEnum::Value);\n    end;\n}",
        )
        .expect("enum scope should infer a type");
        assert_eq!(inferred.base, "MyEnum");
        assert_eq!(inferred.subtype, None);
    }

    #[test]
    fn infer_argument_type_negative_integer() {
        // A leading '-' must still be treated as a numeric literal (Integer).
        let inferred = infer_single(
            "codeunit 50100 Test\n{\n    procedure Caller()\n    begin\n        Foo(-7);\n    end;\n}",
        )
        .expect("negative number should infer a type");
        assert_eq!(inferred.base, "Integer");
    }

    #[test]
    fn infer_argument_type_negative_decimal() {
        let inferred = infer_single(
            "codeunit 50100 Test\n{\n    procedure Caller()\n    begin\n        Foo(-2.5);\n    end;\n}",
        )
        .expect("negative decimal should infer a type");
        assert_eq!(inferred.base, "Decimal");
    }

    #[test]
    fn extract_call_info_chained_member_receiver() {
        // `Rec.Field.SetRange(1)` — the receiver before SetRange's call suffix
        // is a member_suffix; extract_receiver_before must pull the member name.
        let src = r#"codeunit 50100 Test
{
    procedure Caller(Rec: Record Customer)
    begin
        Rec.Name.SetRange(1);
    end;
}"#;
        let (text, tree) = parse(src);
        let source = text.as_bytes();
        let arg_list = first_arg_list(tree.root_node()).expect("an argument list");
        let parent = arg_list.parent().expect("a parent");
        let info = extract_call_info(parent, source).expect("call info");
        assert_eq!(info.0, "SetRange");
        assert_eq!(info.1.as_deref(), Some("Name"));
    }

    #[test]
    fn source_line_returns_requested_row() {
        let source = b"line0\nline1\nline2";
        assert_eq!(source_line(source, 0), "line0");
        assert_eq!(source_line(source, 1), "line1");
        assert_eq!(source_line(source, 2), "line2");
    }

    #[test]
    fn source_line_out_of_bounds_is_empty() {
        let source = b"only-one-line";
        assert_eq!(
            source_line(source, 5),
            "",
            "out-of-range row must yield empty string, not panic"
        );
    }

    #[test]
    fn source_line_invalid_utf8_is_empty() {
        // Row 1 contains an invalid UTF-8 byte (0xFF); must degrade to "".
        let source: &[u8] = b"ok\n\xff\xfe";
        assert_eq!(source_line(source, 0), "ok");
        assert_eq!(
            source_line(source, 1),
            "",
            "invalid UTF-8 row must yield empty string"
        );
    }

    #[test]
    fn lookup_embedded_builtin_matches_language_data() {
        // Don't hardcode a builtin name — discover one from LanguageData at
        // runtime, then assert lookup_embedded_builtin returns exactly that
        // function's parameter names. Unknown names must return None.
        let first_builtin = al_syntax::language_data::builtin_functions()
            .iter()
            .find(|f| !f.parameters.is_empty());

        if let Some(func) = first_builtin {
            let expected: Vec<String> = func.parameters.iter().map(|p| p.name.clone()).collect();
            let got = lookup_embedded_builtin(&func.name)
                .expect("a known builtin must resolve to its parameter names");
            assert_eq!(got, expected);
        }

        assert!(
            lookup_embedded_builtin("ThisIsDefinitelyNotABuiltinFunction").is_none(),
            "an unknown function name must not resolve to an embedded builtin"
        );
    }

    #[test]
    fn inlay_hints_none_for_unopened_document() {
        // No document in the store → get_or_parse returns None → no hints.
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent.al").unwrap();
        assert!(
            inlay_hints(&ws, &uri, full_range()).is_none(),
            "an unopened document must produce no inlay hints"
        );
    }

    #[test]
    fn inlay_hints_emits_parameter_hints_for_open_document() {
        // Default config has parameter_names = true. Opening a file with a
        // local call should yield parameter hints end-to-end.
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        Add(1, 2);
    end;

    procedure Add(First: Integer; Second: Integer): Integer
    begin
    end;
}"#;
        let ws = Workspace::new();
        let uri = Url::parse("file:///tmp/inlay_param_test.al").unwrap();
        ws.documents.open(uri.clone(), src.to_string());

        let hints = inlay_hints(&ws, &uri, full_range()).expect("some hints");
        let param_labels: Vec<String> = hints
            .iter()
            .filter(|h| h.kind == Some(AlInlayHintKind::Parameter))
            .map(|h| {
                let AlInlayHintLabel::String(s) = &h.label;
                s.clone()
            })
            .collect();
        assert_eq!(
            param_labels,
            vec!["First:".to_string(), "Second:".to_string()],
            "expected First:/Second: parameter hints end-to-end, got: {hints:?}"
        );
    }

    #[test]
    fn inlay_hints_respects_disabled_parameter_names() {
        // With parameter_names disabled and return_types disabled (default),
        // a file that only has call sites yields no hints at all.
        let src = r#"codeunit 50100 Test
{
    procedure Caller()
    begin
        Add(1, 2);
    end;

    procedure Add(First: Integer; Second: Integer): Integer
    begin
    end;
}"#;
        let ws = Workspace::new();
        {
            let mut cfg = ws.config.try_write().unwrap();
            cfg.inlay_hints.parameter_names = false;
            cfg.inlay_hints.return_types = false;
        }
        let uri = Url::parse("file:///tmp/inlay_disabled_test.al").unwrap();
        ws.documents.open(uri.clone(), src.to_string());

        assert!(
            inlay_hints(&ws, &uri, full_range()).is_none(),
            "with both hint kinds disabled there should be no hints"
        );
    }

    #[test]
    fn inlay_hints_emits_return_type_hints_when_enabled() {
        // Enable return_types; a procedure with a return type must yield a
        // Type-kind hint via the top-level entry point.
        let src = r#"codeunit 50100 Test
{
    procedure GetCount(): Integer
    begin
    end;
}"#;
        let ws = Workspace::new();
        {
            let mut cfg = ws.config.try_write().unwrap();
            cfg.inlay_hints.parameter_names = false;
            cfg.inlay_hints.return_types = true;
        }
        let uri = Url::parse("file:///tmp/inlay_return_test.al").unwrap();
        ws.documents.open(uri.clone(), src.to_string());

        let hints = inlay_hints(&ws, &uri, full_range()).expect("a return-type hint");
        assert!(
            hints.iter().any(|h| h.kind == Some(AlInlayHintKind::Type)),
            "expected a Type-kind return hint, got: {hints:?}"
        );
    }
}
