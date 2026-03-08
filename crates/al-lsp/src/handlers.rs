//! LSP method dispatch — delegates to specific handler modules.
//!
//! This module contains handlers for:
//! - textDocument/documentSymbol
//! - textDocument/foldingRange
//! - textDocument/semanticTokens/full
//! - textDocument/signatureHelp
//! - textDocument/codeAction
//! - textDocument/inlayHint

use tower_lsp::lsp_types::*;

use crate::completions::extract_last_identifier;
use crate::server::AlServer;

// ---------------------------------------------------------------------------
// Document symbols
// ---------------------------------------------------------------------------

/// Handle textDocument/documentSymbol.
#[allow(deprecated)]
pub(crate) fn handle_document_symbol(
    server: &AlServer,
    uri: &Url,
) -> Option<DocumentSymbolResponse> {
    let text = server.documents.get_text(uri)?;

    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

    let symbols = al_syntax::extract_document_symbols(&tree, &text);

    Some(DocumentSymbolResponse::Nested(symbols))
}

// ---------------------------------------------------------------------------
// Folding ranges
// ---------------------------------------------------------------------------

/// Handle textDocument/foldingRange.
pub(crate) fn handle_folding_range(server: &AlServer, uri: &Url) -> Option<Vec<FoldingRange>> {
    let text = server.documents.get_text(uri)?;

    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

    let ranges = al_syntax::extract_folding_ranges(&tree, &text);
    Some(ranges)
}

// ---------------------------------------------------------------------------
// Semantic tokens
// ---------------------------------------------------------------------------

/// Handle textDocument/semanticTokens/full.
pub(crate) fn handle_semantic_tokens(
    server: &AlServer,
    uri: &Url,
) -> Option<SemanticTokensResult> {
    let text = server.documents.get_text(uri)?;

    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

    let tokens = al_syntax::extract_semantic_tokens(&tree, &text);

    let lsp_tokens: Vec<SemanticToken> = tokens
        .iter()
        .map(|t| SemanticToken {
            delta_line: t.delta_line,
            delta_start: t.delta_start,
            length: t.length,
            token_type: t.token_type,
            token_modifiers_bitset: t.token_modifiers,
        })
        .collect();

    Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data: lsp_tokens,
    }))
}

// ---------------------------------------------------------------------------
// Signature help
// ---------------------------------------------------------------------------

/// Handle textDocument/signatureHelp.
pub(crate) fn handle_signature_help(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<SignatureHelp> {
    let text = server.documents.get_text(uri)?;

    let line_idx = position.line as usize;
    let col = position.character as usize;
    let line = text.lines().nth(line_idx)?;

    // Find the function name and active parameter from the text before the cursor
    let prefix = if col <= line.len() {
        &line[..col]
    } else {
        line
    };

    // Walk backwards to find the opening paren and count commas
    let (func_name, active_param) = find_call_context(prefix)?;

    // Look up the procedure in the current file
    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

    // Search document symbols for the function
    let doc_symbols = al_syntax::extract_document_symbols(&tree, &text);
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && (child.kind == SymbolKind::FUNCTION || child.kind == SymbolKind::EVENT)
                {
                    let detail = child.detail.as_deref().unwrap_or("()");
                    return Some(SignatureHelp {
                        signatures: vec![SignatureInformation {
                            label: format!("{}{}",  child.name, detail),
                            documentation: None,
                            parameters: None,
                            active_parameter: Some(active_param),
                        }],
                        active_signature: Some(0),
                        active_parameter: Some(active_param),
                    });
                }
            }
        }
    }

    // Look up in package symbols
    let symbols = server.symbols.search(func_name, 5);
    for entry in &symbols {
        for method in &entry.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                let params: Vec<ParameterInformation> = method
                    .parameters
                    .iter()
                    .map(|p| {
                        let var_prefix = if p.is_var { "var " } else { "" };
                        ParameterInformation {
                            label: ParameterLabel::Simple(format!(
                                "{}{}: {}",
                                var_prefix, p.name, p.type_name
                            )),
                            documentation: None,
                        }
                    })
                    .collect();

                let params_str: Vec<String> = method
                    .parameters
                    .iter()
                    .map(|p| {
                        let var_prefix = if p.is_var { "var " } else { "" };
                        format!("{}{}: {}", var_prefix, p.name, p.type_name)
                    })
                    .collect();
                let return_str = method
                    .return_type
                    .as_ref()
                    .map(|r| format!(": {}", r))
                    .unwrap_or_default();

                return Some(SignatureHelp {
                    signatures: vec![SignatureInformation {
                        label: format!(
                            "{}({}){}",
                            method.name,
                            params_str.join("; "),
                            return_str
                        ),
                        documentation: None,
                        parameters: Some(params),
                        active_parameter: Some(active_param),
                    }],
                    active_signature: Some(0),
                    active_parameter: Some(active_param),
                });
            }
        }
    }

    // Look up in built-in types
    let builtins = server.builtins.blocking_read();
    for bt in builtins.iter() {
        for method in &bt.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                let params: Vec<ParameterInformation> = method
                    .parameters
                    .iter()
                    .map(|p| {
                        let var_prefix = if p.is_var { "var " } else { "" };
                        ParameterInformation {
                            label: ParameterLabel::Simple(format!(
                                "{}{}: {}",
                                var_prefix, p.name, p.type_name
                            )),
                            documentation: None,
                        }
                    })
                    .collect();

                let params_str: Vec<String> = method
                    .parameters
                    .iter()
                    .map(|p| {
                        let var_prefix = if p.is_var { "var " } else { "" };
                        format!("{}{}: {}", var_prefix, p.name, p.type_name)
                    })
                    .collect();
                let return_str = method
                    .return_type
                    .as_ref()
                    .map(|r| format!(": {}", r))
                    .unwrap_or_default();

                let doc = if method.documentation.is_empty() {
                    None
                } else {
                    Some(Documentation::String(method.documentation.clone()))
                };

                return Some(SignatureHelp {
                    signatures: vec![SignatureInformation {
                        label: format!(
                            "{}.{}({}){}",
                            bt.name,
                            method.name,
                            params_str.join("; "),
                            return_str
                        ),
                        documentation: doc,
                        parameters: Some(params),
                        active_parameter: Some(active_param),
                    }],
                    active_signature: Some(0),
                    active_parameter: Some(active_param),
                });
            }
        }
    }

    None
}

/// Find the function name and active parameter index from text before cursor.
/// Returns (function_name, active_parameter_index).
fn find_call_context(prefix: &str) -> Option<(&str, u32)> {
    let bytes = prefix.as_bytes();
    let mut paren_depth = 0i32;
    let mut comma_count = 0u32;

    // Walk backwards from end
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' => paren_depth += 1,
            b'(' => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                } else {
                    // Found the matching open paren
                    let before_paren = prefix[..i].trim_end();
                    let func_name = extract_trailing_identifier(before_paren)?;
                    return Some((func_name, comma_count));
                }
            }
            b',' if paren_depth == 0 => {
                comma_count += 1;
            }
            _ => {}
        }
    }

    None
}

/// Extract the trailing identifier from a string.
fn extract_trailing_identifier(s: &str) -> Option<&str> {
    let result = extract_last_identifier(s);
    if result.is_empty() { None } else { Some(result) }
}

// ---------------------------------------------------------------------------
// Code actions
// ---------------------------------------------------------------------------

/// Handle textDocument/codeAction.
pub(crate) fn handle_code_action(
    server: &AlServer,
    uri: &Url,
    _range: Range,
    diagnostics: &[Diagnostic],
) -> Option<Vec<CodeActionOrCommand>> {
    let text = server.documents.get_text(uri)?;
    let mut actions = Vec::new();

    for diag in diagnostics {
        // Quick fix for missing case else (AL-L010)
        if diag
            .code
            .as_ref()
            .map_or(false, |c| matches!(c, NumberOrString::String(s) if s == "AL-L010"))
        {
            let insert_pos = Position {
                line: diag.range.end.line,
                character: 0,
            };

            let edit = TextEdit {
                range: Range {
                    start: insert_pos,
                    end: insert_pos,
                },
                new_text: "            else\n                ; // default case\n".to_string(),
            };

            let mut changes = std::collections::HashMap::new();
            changes.insert(uri.clone(), vec![edit]);

            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "Add missing 'else' branch".to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diag.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }

        // Quick fix for empty begin..end (AL-L001) — suggest adding a comment
        if diag
            .code
            .as_ref()
            .map_or(false, |c| matches!(c, NumberOrString::String(s) if s == "AL-L001"))
        {
            let insert_pos = Position {
                line: diag.range.start.line + 1,
                character: 0,
            };

            let edit = TextEdit {
                range: Range {
                    start: insert_pos,
                    end: insert_pos,
                },
                new_text: "        // TODO: Implement\n".to_string(),
            };

            let mut changes = std::collections::HashMap::new();
            changes.insert(uri.clone(), vec![edit]);

            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "Add TODO comment".to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diag.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
    }

    let _ = text; // used indirectly

    if actions.is_empty() {
        None
    } else {
        Some(actions)
    }
}

// ---------------------------------------------------------------------------
// Inlay hints
// ---------------------------------------------------------------------------

/// Handle textDocument/inlayHint.
pub(crate) fn handle_inlay_hint(
    server: &AlServer,
    uri: &Url,
    range: Range,
) -> Option<Vec<InlayHint>> {
    let text = server.documents.get_text(uri)?;

    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

    let root = tree.root_node();
    let source = text.as_bytes();
    let mut hints = Vec::new();

    collect_inlay_hints(root, source, server, &range, &mut hints);

    if hints.is_empty() {
        None
    } else {
        Some(hints)
    }
}

/// Recursively collect inlay hints from the AST, limited to the requested range.
fn collect_inlay_hints(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    server: &AlServer,
    range: &Range,
    hints: &mut Vec<InlayHint>,
) {
    // Skip nodes entirely outside the requested range
    let node_start = node.start_position().row as u32;
    let node_end = node.end_position().row as u32;
    if node_end < range.start.line || node_start > range.end.line {
        return;
    }

    // Look for procedure/function calls with arguments
    if node.kind() == "argument_list" || node.kind() == "call_arguments" {
        // Try to find the function name from the parent expression
        if let Some(parent) = node.parent() {
            let func_name = extract_call_name(parent, source);
            if let Some(name) = func_name {
                // Look up parameters from index or builtins
                let param_names = lookup_parameter_names(server, &name);
                if !param_names.is_empty() {
                    add_parameter_hints(node, source, &param_names, hints);
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_inlay_hints(child, source, server, range, hints);
    }
}

/// Extract the function name from a call expression node.
fn extract_call_name(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    // Look for identifier children that represent the function name
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "identifier" || kind == "quoted_identifier" || kind == "name" {
            if let Ok(text) = child.utf8_text(source) {
                return Some(text.trim_matches('"').to_string());
            }
        }
    }
    None
}

/// Look up parameter names for a function from index or builtins.
fn lookup_parameter_names(server: &AlServer, func_name: &str) -> Vec<String> {
    // Check package symbols
    let symbols = server.symbols.search(func_name, 5);
    for entry in &symbols {
        for method in &entry.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                return method.parameters.iter().map(|p| p.name.clone()).collect();
            }
        }
    }

    // Check built-in types
    let builtins = server.builtins.blocking_read();
    for bt in builtins.iter() {
        for method in &bt.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                return method.parameters.iter().map(|p| p.name.clone()).collect();
            }
        }
    }

    Vec::new()
}

/// Add parameter name hints for arguments in a call.
fn add_parameter_hints(
    arg_list: tree_sitter::Node<'_>,
    _source: &[u8],
    param_names: &[String],
    hints: &mut Vec<InlayHint>,
) {
    let mut cursor = arg_list.walk();
    let mut param_idx = 0;

    for child in arg_list.children(&mut cursor) {
        // Skip delimiters and whitespace
        let kind = child.kind();
        if !child.is_named() || kind == "," || kind == "(" || kind == ")" || kind == "semicolon" {
            continue;
        }

        if let Some(name) = param_names.get(param_idx) {
            let start = child.start_position();
            hints.push(InlayHint {
                position: Position {
                    line: start.row as u32,
                    character: start.column as u32,
                },
                label: InlayHintLabel::String(format!("{}: ", name)),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }

        param_idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_call_context_simple() {
        let result = find_call_context("Message(");
        assert_eq!(result, Some(("Message", 0)));
    }

    #[test]
    fn test_find_call_context_with_comma() {
        let result = find_call_context("Message('hello', ");
        assert_eq!(result, Some(("Message", 1)));
    }

    #[test]
    fn test_find_call_context_nested() {
        let result = find_call_context("Outer(Inner(a, b), ");
        assert_eq!(result, Some(("Outer", 1)));
    }

    #[test]
    fn test_find_call_context_no_call() {
        let result = find_call_context("x := 42");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_trailing_identifier() {
        assert_eq!(extract_trailing_identifier("Message"), Some("Message"));
        assert_eq!(extract_trailing_identifier("x.DoWork"), Some("DoWork"));
        assert_eq!(extract_trailing_identifier(""), None);
    }

    #[test]
    fn test_extract_trailing_identifier_quoted() {
        assert_eq!(
            extract_trailing_identifier("\"My Proc\""),
            Some("My Proc")
        );
    }
}
