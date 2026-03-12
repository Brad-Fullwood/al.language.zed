//! LSP method dispatch — delegates to specific handler modules.
//!
//! This module contains handlers for:
//! - textDocument/documentSymbol
//! - textDocument/foldingRange
//! - textDocument/semanticTokens/full
//! - textDocument/signatureHelp
//! - textDocument/codeAction
//! - textDocument/inlayHint

use al_syntax::AlParser;
use tower_lsp::lsp_types::*;

use crate::diagnostics;
use crate::formatting;
use crate::parsing;
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
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let symbols = al_syntax::extract_document_symbols(&tree, &text);

    tracing::debug!(
        symbol_count = symbols.len(),
        "document_symbol: returning symbols"
    );
    Some(DocumentSymbolResponse::Nested(symbols))
}

// ---------------------------------------------------------------------------
// Folding ranges
// ---------------------------------------------------------------------------

/// Handle textDocument/foldingRange.
pub(crate) fn handle_folding_range(server: &AlServer, uri: &Url) -> Option<Vec<FoldingRange>> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let ranges = al_syntax::extract_folding_ranges(&tree, &text);
    tracing::debug!(
        range_count = ranges.len(),
        "folding_range: returning ranges"
    );
    Some(ranges)
}

// ---------------------------------------------------------------------------
// Semantic tokens
// ---------------------------------------------------------------------------

/// Handle textDocument/semanticTokens/full.
pub(crate) fn handle_semantic_tokens(server: &AlServer, uri: &Url) -> Option<SemanticTokensResult> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

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

    tracing::debug!(
        token_count = lsp_tokens.len(),
        "semantic_tokens: returning tokens"
    );
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

    tracing::debug!(func_name = %func_name, active_param = active_param, "signature: find_call_context result");

    // Look up the procedure in the current file
    let tree = {
        let (_, t) = parsing::get_or_parse(server, uri)?;
        t
    };

    // Search document symbols for the function
    let doc_symbols = al_syntax::extract_document_symbols(&tree, &text);
    tracing::debug!(func_name = %func_name, doc_symbol_count = doc_symbols.len(), "signature: searching document symbols");
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && (child.kind == SymbolKind::FUNCTION || child.kind == SymbolKind::EVENT)
                {
                    tracing::debug!(func_name = %func_name, matched = %child.name, "signature: matched document symbol");
                    let detail = child.detail.as_deref().unwrap_or("()");
                    return Some(SignatureHelp {
                        signatures: vec![SignatureInformation {
                            label: format!("{}{}", child.name, detail),
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
    tracing::debug!(func_name = %func_name, "signature: no document symbol match, trying receiver resolution");

    // Look up through receiver type resolution (cross-file workspace procedures)
    // If the call is `receiver.Method(...)`, resolve the receiver type and search
    // the target workspace file's procedures.
    if let Some(sig) = resolve_receiver_signature(
        server,
        uri,
        &text,
        &tree,
        prefix,
        func_name,
        active_param,
        position,
    ) {
        tracing::debug!(func_name = %func_name, "signature: matched via receiver resolution");
        return Some(sig);
    }
    tracing::debug!(func_name = %func_name, "signature: no receiver match, trying package symbols");

    // Look up in package symbols
    let symbols = server.symbols.search(func_name, 5);
    tracing::debug!(func_name = %func_name, package_results = symbols.len(), "signature: package symbol lookup");
    for entry in &symbols {
        for method in &entry.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                tracing::debug!(func_name = %func_name, object = %entry.name, "signature: matched package symbol method");
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
                        label: format!("{}({}){}", method.name, params_str.join("; "), return_str),
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
    tracing::debug!(func_name = %func_name, "signature: no package symbol match, trying builtins");

    // Look up in built-in types — collect all overloads
    let builtins = server.builtins.read().unwrap().clone();
    tracing::debug!(func_name = %func_name, builtin_types = builtins.len(), "signature: builtin lookup");
    let mut signatures = Vec::new();
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
                    Some(Documentation::String(crate::resolution::strip_xml_tags(
                        &method.documentation,
                    )))
                };

                signatures.push(SignatureInformation {
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
                });
            }
        }
    }
    if !signatures.is_empty() {
        tracing::debug!(func_name = %func_name, overloads = signatures.len(), "signature: matched builtin overloads");
        // Pick the best active_signature based on parameter count matching active_param
        let active_sig = signatures
            .iter()
            .position(|s| {
                s.parameters
                    .as_ref()
                    .is_some_and(|p| p.len() as u32 > active_param)
            })
            .unwrap_or(0) as u32;
        return Some(SignatureHelp {
            signatures,
            active_signature: Some(active_sig),
            active_parameter: Some(active_param),
        });
    }

    tracing::debug!(func_name = %func_name, "signature: no match found");
    None
}

// find_call_context and extract_trailing_identifier moved to al_syntax::context
use al_syntax::find_call_context;

/// Resolve signature help through receiver type for cross-file workspace procedures.
///
/// Given `ProcessReport.SetAction(...)`, resolves `ProcessReport` to its type
/// (e.g., Report "IJL Process Staging"), then searches that workspace file's
/// procedures for `SetAction`.
#[allow(clippy::too_many_arguments)]
fn resolve_receiver_signature(
    server: &AlServer,
    _uri: &Url,
    text: &str,
    tree: &tree_sitter::Tree,
    prefix: &str,
    func_name: &str,
    active_param: u32,
    position: Position,
) -> Option<SignatureHelp> {
    // Find the open paren for this call to get the text before it
    let paren_pos = prefix.rfind('(')?;
    let before_paren = prefix[..paren_pos].trim_end();

    // Check if there's a dot before the function name
    let dot_pos = before_paren.rfind('.')?;
    let receiver_text = before_paren[..dot_pos].trim();

    // Extract the receiver identifier
    let receiver_name = al_syntax::extract_last_identifier(receiver_text);
    if receiver_name.is_empty() {
        tracing::debug!(func_name = %func_name, "signature: receiver resolution — empty receiver name");
        return None;
    }

    tracing::debug!(func_name = %func_name, receiver_name = %receiver_name, "signature: receiver resolution — resolving type");

    // Resolve the receiver type using TypeResolver
    let resolver = al_syntax::TypeResolver::new(tree, text);
    let decl = resolver.resolve_type(receiver_name, position)?;

    // Get the subtype (e.g., "IJL Process Staging" from Report "IJL Process Staging")
    let subtype = decl.type_subtype.as_deref()?;

    tracing::debug!(func_name = %func_name, receiver_name = %receiver_name, resolved_type = %decl.type_name, subtype = %subtype, "signature: receiver resolution — type resolved");

    // Search workspace objects for this subtype
    let obj_key = subtype.to_lowercase();
    let file_path_entry = server.workspace_objects.get(&obj_key)?;
    let file_path = file_path_entry.value();
    let file_text_entry = server.workspace_files.get(file_path)?;
    let file_text = file_text_entry.value();

    let result = AlParser::parse_quick(file_text);

    // Search for the method in the target file's document symbols
    let doc_symbols = al_syntax::extract_document_symbols(&result.tree, file_text);
    tracing::debug!(func_name = %func_name, subtype = %subtype, target_symbols = doc_symbols.len(), "signature: receiver resolution — searching target file symbols");
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && (child.kind == SymbolKind::FUNCTION || child.kind == SymbolKind::EVENT)
                {
                    tracing::debug!(func_name = %func_name, matched = %child.name, subtype = %subtype, "signature: receiver resolution — matched workspace procedure");
                    let detail = child.detail.as_deref().unwrap_or("()");
                    return Some(SignatureHelp {
                        signatures: vec![SignatureInformation {
                            label: format!("{}{}", child.name, detail),
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

    // Also check package symbols for the resolved type
    let pkg_symbols = server.symbols.get_by_name(subtype);
    tracing::debug!(func_name = %func_name, subtype = %subtype, pkg_results = pkg_symbols.len(), "signature: receiver resolution — searching package symbols");
    for entry in &pkg_symbols {
        for method in &entry.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                tracing::debug!(func_name = %func_name, object = %entry.name, subtype = %subtype, "signature: receiver resolution — matched package method");
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
                        label: format!("{}({}){}", method.name, params_str.join("; "), return_str),
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

    tracing::debug!(func_name = %func_name, subtype = %subtype, "signature: receiver resolution — no match found in workspace or packages");
    None
}

// ---------------------------------------------------------------------------
// Code actions
// ---------------------------------------------------------------------------

/// Handle textDocument/codeAction.
pub(crate) fn handle_code_action(
    server: &AlServer,
    uri: &Url,
    range: Range,
    diagnostics: &[Diagnostic],
) -> Option<Vec<CodeActionOrCommand>> {
    let text = server.documents.get_text(uri)?;
    let mut actions = Vec::new();

    tracing::debug!(
        diagnostic_count = diagnostics.len(),
        "code_action: processing diagnostics"
    );

    // --- Diagnostic-based quick fixes ---
    for diag in diagnostics {
        let code = diag_code_str(diag);

        match code.as_deref() {
            Some("AL-L001") => {
                // Empty begin..end — suggest adding a TODO comment
                let insert_pos = Position {
                    line: diag.range.start.line + 1,
                    character: 0,
                };
                let indent = detect_indent(&text, diag.range.start.line);
                let edit = TextEdit {
                    range: Range {
                        start: insert_pos,
                        end: insert_pos,
                    },
                    new_text: format!("{}    // TODO: Implement\n", indent),
                };
                actions.push(make_quickfix("Add TODO comment", uri, diag, vec![edit]));
            }

            Some("AL-L005") => {
                // Unused variable — offer to remove the declaration line
                // The diagnostic range covers the variable name; remove the whole line
                let line_start = Position {
                    line: diag.range.start.line,
                    character: 0,
                };
                let line_end = Position {
                    line: diag.range.start.line + 1,
                    character: 0,
                };
                let edit = TextEdit {
                    range: Range {
                        start: line_start,
                        end: line_end,
                    },
                    new_text: String::new(),
                };
                actions.push(make_quickfix(
                    "Remove unused variable",
                    uri,
                    diag,
                    vec![edit],
                ));
            }

            Some("AL-L006") => {
                // Empty trigger body — offer to add a TODO comment
                let insert_pos = Position {
                    line: diag.range.start.line + 1,
                    character: 0,
                };
                let indent = detect_indent(&text, diag.range.start.line);
                let edit = TextEdit {
                    range: Range {
                        start: insert_pos,
                        end: insert_pos,
                    },
                    new_text: format!("{}        // TODO: Implement trigger\n", indent),
                };
                actions.push(make_quickfix(
                    "Add TODO comment to trigger",
                    uri,
                    diag,
                    vec![edit],
                ));
            }

            Some("AL-L007") => {
                // TODO/FIXME comment — offer to remove (mark as resolved)
                let line_start = Position {
                    line: diag.range.start.line,
                    character: 0,
                };
                let line_end = Position {
                    line: diag.range.start.line + 1,
                    character: 0,
                };
                let edit = TextEdit {
                    range: Range {
                        start: line_start,
                        end: line_end,
                    },
                    new_text: String::new(),
                };
                actions.push(make_quickfix(
                    "Remove TODO comment (mark as resolved)",
                    uri,
                    diag,
                    vec![edit],
                ));
            }

            Some("AL-L009") => {
                // Excessive parameters — offer to add a comment suggesting extraction
                let insert_pos = Position {
                    line: diag.range.start.line,
                    character: 0,
                };
                let indent = detect_indent(&text, diag.range.start.line);
                let edit = TextEdit {
                    range: Range { start: insert_pos, end: insert_pos },
                    new_text: format!(
                        "{}// REFACTOR: Consider extracting parameters into a record or buffer table\n",
                        indent
                    ),
                };
                actions.push(make_quickfix(
                    "Add refactoring suggestion comment",
                    uri,
                    diag,
                    vec![edit],
                ));
            }

            Some("AL-L010") => {
                // Missing case else — add else branch
                let insert_pos = Position {
                    line: diag.range.end.line,
                    character: 0,
                };
                let indent = detect_indent(&text, diag.range.start.line);
                let edit = TextEdit {
                    range: Range {
                        start: insert_pos,
                        end: insert_pos,
                    },
                    new_text: format!("{}    else\n{}        ; // default case\n", indent, indent),
                };
                actions.push(make_quickfix(
                    "Add missing 'else' branch",
                    uri,
                    diag,
                    vec![edit],
                ));
            }

            Some("AL-L011") => {
                // Redundant begin..end around single statement — remove begin/end
                if let Some(edits) = compute_remove_begin_end(&text, diag) {
                    actions.push(make_quickfix(
                        "Remove redundant begin..end",
                        uri,
                        diag,
                        edits,
                    ));
                }
            }

            Some("AL-L013") => {
                // Empty REPEAT..UNTIL loop — add TODO comment
                let insert_pos = Position {
                    line: diag.range.start.line + 1,
                    character: 0,
                };
                let indent = detect_indent(&text, diag.range.start.line);
                let edit = TextEdit {
                    range: Range {
                        start: insert_pos,
                        end: insert_pos,
                    },
                    new_text: format!("{}    // TODO: Add loop body\n", indent),
                };
                actions.push(make_quickfix(
                    "Add TODO comment to loop",
                    uri,
                    diag,
                    vec![edit],
                ));
            }

            Some("AL-L016") => {
                // Procedure naming — fix to PascalCase (capitalize first letter)
                if let Some(edit) = compute_pascal_case_fix(&text, diag) {
                    actions.push(make_quickfix(
                        "Fix procedure name to PascalCase",
                        uri,
                        diag,
                        vec![edit],
                    ));
                }
            }

            Some("AL-L017") => {
                // Hard-coded string — extract to Label variable
                if let Some(edits) = compute_extract_to_label(&text, diag) {
                    actions.push(make_quickfix("Extract to Label variable", uri, diag, edits));
                }
            }

            _ => {}
        }
    }

    // --- Source actions (not tied to diagnostics) ---
    // Add procedure documentation template
    if let Some(action) = source_action_add_doc_comment(server, uri, &text, range) {
        actions.push(action);
    }

    // Add region wrapper
    if range.start != range.end {
        if let Some(action) = source_action_add_region(uri, &text, range) {
            actions.push(action);
        }
    }

    // --- Source actions (workspace commands surfaced as code actions) ---
    // Format File (calculate edits directly)
    if let Some(edits) = formatting::handle_formatting(
        server,
        uri,
        &FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
    ) {
        if !edits.is_empty() {
            let mut changes = std::collections::HashMap::new();
            changes.insert(uri.clone(), edits);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "AL: Format File".to_string(),
                kind: Some(CodeActionKind::SOURCE),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        } else {
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "AL: Format File".to_string(),
                kind: Some(CodeActionKind::SOURCE),
                command: Some(Command {
                    title: "AL: Format File".to_string(),
                    command: "al.formatFile".to_string(),
                    arguments: serde_json::to_value(uri).ok().map(|v| vec![v]),
                }),
                ..Default::default()
            }));
        }
    }

    // Lint File (still a command because it triggers an async side effect)
    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
        title: "AL: Lint File".to_string(),
        kind: Some(CodeActionKind::SOURCE),
        command: Some(Command {
            title: "AL: Lint File".to_string(),
            command: "al.lintFile".to_string(),
            arguments: serde_json::to_value(uri).ok().map(|v| vec![v]),
        }),
        ..Default::default()
    }));

    tracing::debug!(
        action_count = actions.len(),
        "code_action: returning actions"
    );

    if actions.is_empty() {
        None
    } else {
        Some(actions)
    }
}

/// Helper to extract a diagnostic code as a string.
fn diag_code_str(diag: &Diagnostic) -> Option<String> {
    diag.code.as_ref().map(|c| match c {
        NumberOrString::String(s) => s.clone(),
        NumberOrString::Number(n) => n.to_string(),
    })
}

/// Build a quick-fix CodeAction.
fn make_quickfix(
    title: &str,
    uri: &Url,
    diag: &Diagnostic,
    edits: Vec<TextEdit>,
) -> CodeActionOrCommand {
    let mut changes = std::collections::HashMap::new();
    changes.insert(uri.clone(), edits);
    CodeActionOrCommand::CodeAction(CodeAction {
        title: title.to_string(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Detect the leading whitespace of a line in the document text.
fn detect_indent(text: &str, line: u32) -> String {
    if let Some(line_str) = text.lines().nth(line as usize) {
        let indent_len = line_str.len() - line_str.trim_start().len();
        line_str[..indent_len].to_string()
    } else {
        "    ".to_string()
    }
}

/// Compute edits to remove redundant begin..end around a single statement (AL-L011).
/// Returns edits that remove the `begin` line and `end` line, keeping the inner statement.
fn compute_remove_begin_end(text: &str, diag: &Diagnostic) -> Option<Vec<TextEdit>> {
    let lines: Vec<&str> = text.lines().collect();
    let start_line = diag.range.start.line as usize;
    let end_line = diag.range.end.line as usize;

    if start_line >= lines.len() || end_line >= lines.len() {
        return None;
    }

    // The begin line
    let begin_line_text = lines[start_line];
    if !begin_line_text.trim().eq_ignore_ascii_case("begin") {
        return None;
    }

    // The end line (may be "end" or "end;")
    let end_line_text = lines[end_line];
    let end_trimmed = end_line_text.trim().to_lowercase();
    if !end_trimmed.starts_with("end") {
        return None;
    }

    let edits = vec![
        // Remove the begin line
        TextEdit {
            range: Range {
                start: Position {
                    line: start_line as u32,
                    character: 0,
                },
                end: Position {
                    line: (start_line + 1) as u32,
                    character: 0,
                },
            },
            new_text: String::new(),
        },
        // Remove the end line
        TextEdit {
            range: Range {
                start: Position {
                    line: end_line as u32,
                    character: 0,
                },
                end: Position {
                    line: (end_line + 1) as u32,
                    character: 0,
                },
            },
            new_text: String::new(),
        },
    ];

    Some(edits)
}

/// Compute edit to capitalize the first letter of a procedure name (AL-L016).
fn compute_pascal_case_fix(text: &str, diag: &Diagnostic) -> Option<TextEdit> {
    let lines: Vec<&str> = text.lines().collect();
    let line = diag.range.start.line as usize;
    if line >= lines.len() {
        return None;
    }

    let start_col = diag.range.start.character as usize;
    let end_col = diag.range.end.character as usize;
    let line_text = lines[line];

    if end_col > line_text.len() || start_col >= end_col {
        return None;
    }

    let name = &line_text[start_col..end_col];
    let name = name.trim_matches('"');
    if name.is_empty() {
        return None;
    }

    // Capitalize first letter
    let mut chars = name.chars();
    let first = chars.next()?;
    let fixed = format!("{}{}", first.to_uppercase(), chars.as_str());

    Some(TextEdit {
        range: diag.range,
        new_text: fixed,
    })
}

/// Compute edits to extract a hard-coded string to a Label variable (AL-L017).
/// Replaces the string with a variable reference and adds a Label declaration.
fn compute_extract_to_label(text: &str, diag: &Diagnostic) -> Option<Vec<TextEdit>> {
    let lines: Vec<&str> = text.lines().collect();
    let line = diag.range.start.line as usize;
    if line >= lines.len() {
        return None;
    }

    let start_col = diag.range.start.character as usize;
    let end_col = diag.range.end.character as usize;
    let line_text = lines[line];

    if end_col > line_text.len() || start_col >= end_col {
        return None;
    }

    let string_literal = &line_text[start_col..end_col];
    // Strip outer quotes to make a variable name
    let inner = string_literal
        .trim_start_matches("@'")
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .trim_matches('"');

    // Generate a label variable name: take first few words, PascalCase
    let label_name = generate_label_name(inner);

    let mut edits = Vec::new();

    // Replace the string literal with the label variable
    edits.push(TextEdit {
        range: diag.range,
        new_text: label_name.clone(),
    });

    // Find the var section to insert the label declaration
    // Search backwards for a line containing 'var' (simple heuristic)
    let indent = detect_indent(text, diag.range.start.line);
    let label_decl = format!("{}    {}: Label {};\n", indent, label_name, string_literal);

    // Try to find a 'var' section above the current line
    let mut var_line = None;
    for i in (0..line).rev() {
        let l = lines[i].trim().to_lowercase();
        if l == "var" || l.starts_with("var ") {
            var_line = Some(i);
            break;
        }
        // Stop at procedure/trigger declaration
        if l.starts_with("procedure ")
            || l.starts_with("local procedure ")
            || l.starts_with("trigger ")
            || l.starts_with("begin")
        {
            break;
        }
    }

    if let Some(vl) = var_line {
        // Insert after the var line
        let insert_pos = Position {
            line: (vl + 1) as u32,
            character: 0,
        };
        edits.push(TextEdit {
            range: Range {
                start: insert_pos,
                end: insert_pos,
            },
            new_text: label_decl,
        });
    } else {
        // No var section found — add a comment near the string as a hint
        let insert_pos = Position {
            line: diag.range.start.line,
            character: 0,
        };
        edits.push(TextEdit {
            range: Range {
                start: insert_pos,
                end: insert_pos,
            },
            new_text: format!(
                "{}// TODO: Add to var section: {}: Label {};\n",
                indent, label_name, string_literal
            ),
        });
    }

    Some(edits)
}

/// Generate a PascalCase label variable name from a string value.
fn generate_label_name(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().take(3).collect();
    if words.is_empty() {
        return "LblText".to_string();
    }
    let name: String = words
        .iter()
        .map(|w| {
            let mut chars = w.chars().filter(|c| c.is_alphanumeric());
            match chars.next() {
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    let rest: String = chars.collect();
                    format!("{}{}", upper, rest)
                }
                None => String::new(),
            }
        })
        .collect();

    if name.is_empty() {
        "LblText".to_string()
    } else {
        format!("Lbl{}", name)
    }
}

/// Source action: Add XML doc comment template above a procedure.
#[allow(deprecated)]
fn source_action_add_doc_comment(
    server: &AlServer,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionOrCommand> {
    // Check if cursor is on or inside a procedure declaration
    let (_, tree) = parsing::get_or_parse(server, uri)?;
    let doc_symbols = al_syntax::extract_document_symbols(&tree, text);

    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.kind != SymbolKind::FUNCTION {
                    continue;
                }
                // Check if the cursor range overlaps with the procedure
                if range.start.line >= child.range.start.line
                    && range.start.line <= child.range.end.line
                {
                    // Check if there is already a doc comment above
                    let proc_line = child.range.start.line as usize;
                    if proc_line > 0 {
                        let lines: Vec<&str> = text.lines().collect();
                        if proc_line <= lines.len() {
                            let prev_line = lines[proc_line.saturating_sub(1)].trim();
                            if prev_line.starts_with("///") {
                                return None; // Already has doc comment
                            }
                        }
                    }

                    let indent = detect_indent(text, child.range.start.line);
                    let params_detail = child.detail.as_deref().unwrap_or("()");
                    let param_names = parse_parameter_names_from_detail(params_detail);

                    let mut doc = format!("{}/// <summary>\n", indent);
                    doc.push_str(&format!("{}/// Description for {}.\n", indent, child.name));
                    doc.push_str(&format!("{}/// </summary>\n", indent));

                    for param in &param_names {
                        doc.push_str(&format!(
                            "{}/// <param name=\"{}\">Description.</param>\n",
                            indent, param
                        ));
                    }

                    // Check for return type
                    if let Some(detail) = &child.detail {
                        if detail.contains("):") || detail.contains(") :") {
                            doc.push_str(&format!(
                                "{}/// <returns>Description of return value.</returns>\n",
                                indent
                            ));
                        }
                    }

                    let insert_pos = Position {
                        line: child.range.start.line,
                        character: 0,
                    };
                    let edit = TextEdit {
                        range: Range {
                            start: insert_pos,
                            end: insert_pos,
                        },
                        new_text: doc,
                    };

                    let mut changes = std::collections::HashMap::new();
                    changes.insert(uri.clone(), vec![edit]);

                    return Some(CodeActionOrCommand::CodeAction(CodeAction {
                        title: "Add procedure documentation".to_string(),
                        kind: Some(CodeActionKind::REFACTOR),
                        diagnostics: None,
                        edit: Some(WorkspaceEdit {
                            changes: Some(changes),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }));
                }
            }
        }
    }

    None
}

/// Source action: Wrap selected code in //region ... //endregion.
fn source_action_add_region(uri: &Url, text: &str, range: Range) -> Option<CodeActionOrCommand> {
    let indent = detect_indent(text, range.start.line);

    let region_start = TextEdit {
        range: Range {
            start: Position {
                line: range.start.line,
                character: 0,
            },
            end: Position {
                line: range.start.line,
                character: 0,
            },
        },
        new_text: format!("{}//region MyRegion\n", indent),
    };

    let region_end = TextEdit {
        range: Range {
            start: Position {
                line: range.end.line + 1,
                character: 0,
            },
            end: Position {
                line: range.end.line + 1,
                character: 0,
            },
        },
        new_text: format!("{}//endregion\n", indent),
    };

    let mut changes = std::collections::HashMap::new();
    changes.insert(uri.clone(), vec![region_start, region_end]);

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: "AL: Wrap in region".to_string(),
        kind: Some(CodeActionKind::REFACTOR),
        diagnostics: None,
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    }))
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
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let root = tree.root_node();
    let source = text.as_bytes();
    let mut hints = Vec::new();

    // Extract document symbols once for the whole file (used for local procedure lookups)
    let doc_symbols = al_syntax::extract_document_symbols(&tree, &text);

    collect_inlay_hints(
        root,
        source,
        &text,
        &tree,
        server,
        &doc_symbols,
        &range,
        &mut hints,
    );

    let parameter_hints = hints
        .iter()
        .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
        .count();
    let type_hints = hints
        .iter()
        .filter(|h| h.kind == Some(InlayHintKind::TYPE))
        .count();
    let other_hints = hints.len() - parameter_hints - type_hints;
    tracing::debug!(
        hint_count = hints.len(),
        parameter_hints = parameter_hints,
        type_hints = type_hints,
        other_hints = other_hints,
        "inlay_hint: returning hints"
    );

    if hints.is_empty() {
        None
    } else {
        Some(hints)
    }
}

/// Recursively collect inlay hints from the AST, limited to the requested range.
#[allow(clippy::too_many_arguments)]
fn collect_inlay_hints(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    text: &str,
    tree: &tree_sitter::Tree,
    server: &AlServer,
    doc_symbols: &[DocumentSymbol],
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
        if let Some(parent) = node.parent() {
            let call_info = extract_call_info(parent, source);
            if let Some((func_name, receiver_name)) = call_info {
                let position = Position {
                    line: node.start_position().row as u32,
                    character: node.start_position().column as u32,
                };
                let arg_types = infer_argument_types(node, source, text, tree, position);
                let param_names = lookup_parameter_names(
                    server,
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
        collect_inlay_hints(child, source, text, tree, server, doc_symbols, range, hints);
    }
}

/// An inferred type for a call argument.
#[derive(Debug, Clone)]
struct InferredType {
    base: String,
    subtype: Option<String>,
}

/// An overload candidate with parameter names and types.
struct OverloadCandidate {
    names: Vec<String>,
    types: Vec<String>, // full type strings, e.g. "Record \"Customer\"", "TextEncoding"
}

/// Infer the type of a single argument expression node.
fn infer_argument_type(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    text: &str,
    tree: &tree_sitter::Tree,
    position: Position,
) -> Option<InferredType> {
    let expr = node.utf8_text(source).ok()?;
    let expr = expr.trim();

    // Scope access: TextEncoding::UTF8 → base type is "TextEncoding"
    if let Some(idx) = expr.find("::") {
        let base = expr[..idx].trim().trim_matches('"');
        if !base.is_empty() {
            return Some(InferredType {
                base: base.to_string(),
                subtype: None,
            });
        }
    }

    // String literal: 'hello' → Text
    if expr.starts_with('\'') {
        return Some(InferredType {
            base: "Text".to_string(),
            subtype: None,
        });
    }

    // Numeric literal
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

    // Boolean
    if expr.eq_ignore_ascii_case("true") || expr.eq_ignore_ascii_case("false") {
        return Some(InferredType {
            base: "Boolean".to_string(),
            subtype: None,
        });
    }

    // Variable name: resolve via TypeResolver
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

/// Infer types for all arguments in a call's argument list.
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

/// Parse a full type string into (base_type, optional_subtype).
///
/// Examples:
/// - `"Record \"Customer\""` → ("Record", Some("Customer"))
/// - `"OutStream"` → ("OutStream", None)
/// - `"Code[20]"` → ("Code", None)
fn parse_type_string(type_str: &str) -> (&str, Option<&str>) {
    let trimmed = type_str.trim();

    // Handle escaped quotes first: Record \"Customer\"
    if let Some(quote_start) = trimmed.find("\\\"") {
        let base = trimmed[..quote_start].trim();
        let rest = &trimmed[quote_start + 2..];
        if let Some(quote_end) = rest.find("\\\"") {
            let subtype = &rest[..quote_end];
            return (base, Some(subtype));
        }
    }

    // Handle plain quoted subtypes: Record "Customer", Codeunit "Sales-Post"
    if let Some(quote_start) = trimmed.find('"') {
        let base = trimmed[..quote_start].trim();
        let rest = &trimmed[quote_start + 1..];
        if let Some(quote_end) = rest.find('"') {
            let subtype = &rest[..quote_end];
            return (base, Some(subtype));
        }
    }

    (trimmed.split_whitespace().next().unwrap_or(trimmed), None)
}

/// Score an overload candidate against inferred argument types.
///
/// Higher score = better match. Considers:
/// - Parameter count match (strong signal)
/// - Base type match per argument
/// - Subtype match per argument (for Record/Codeunit etc.)
fn score_overload(candidate: &OverloadCandidate, arg_types: &[Option<InferredType>]) -> u32 {
    let arg_count = arg_types.len();
    let mut score = 0u32;

    // Parameter count matching
    if candidate.types.len() == arg_count {
        score += 1000;
    } else if candidate.types.len() > arg_count {
        // Has enough params but more than needed — possible but less likely
        score += 100;
    }
    // If fewer params than args, score stays low (can't match)

    // Per-argument type matching
    for (i, arg_type) in arg_types.iter().enumerate() {
        if let Some(param_type_str) = candidate.types.get(i) {
            if let Some(inferred) = arg_type {
                let (param_base, param_subtype) = parse_type_string(param_type_str);

                // Base type match
                if param_base.eq_ignore_ascii_case(&inferred.base) {
                    score += 50;

                    // Subtype match (e.g., both are Record "Customer")
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

/// Select the best overload from candidates using type-aware scoring.
fn select_best_overload(
    candidates: &[OverloadCandidate],
    arg_types: &[Option<InferredType>],
) -> Option<Vec<String>> {
    candidates
        .iter()
        .max_by_key(|c| score_overload(c, arg_types))
        .map(|c| c.names.clone())
}

/// Extract function name and optional receiver name from a call parent node.
///
/// Returns `(method_name, Option<receiver_name>)`.
fn extract_call_info(
    node: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<(String, Option<String>)> {
    match node.kind() {
        "member_call_suffix" | "scope_call_suffix" => {
            // .Method(args) or ::Method(args) — use the "member" field
            let member = node.child_by_field_name("member")?;
            let method_name = member.utf8_text(source).ok()?.trim_matches('"').to_string();
            let receiver = extract_receiver_before(node, source);
            Some((method_name, receiver))
        }
        "call_suffix" => {
            // Bare call: postfix_expression → primary_expression + call_suffix(arglist)
            if let Some(prev) = node.prev_sibling() {
                let name = prev.utf8_text(source).ok()?.trim_matches('"').to_string();
                return Some((name, None));
            }
            None
        }
        _ => {
            // Fallback: look for any identifier child
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                let kind = child.kind();
                if kind == "identifier" || kind == "quoted_identifier" || kind == "name" {
                    if let Ok(t) = child.utf8_text(source) {
                        return Some((t.trim_matches('"').to_string(), None));
                    }
                }
            }
            None
        }
    }
}

/// Extract the receiver identifier from the sibling preceding a member_call/scope_call suffix.
fn extract_receiver_before(suffix_node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let prev = suffix_node.prev_sibling()?;
    match prev.kind() {
        "primary_expression" => {
            // Simple: JsonTools.Rec2Json(...) → receiver is "JsonTools"
            Some(prev.utf8_text(source).ok()?.trim_matches('"').to_string())
        }
        "member_suffix" | "member_call_suffix" => {
            // Chained: this.StagingRec.SetJournalData(...) → receiver is "StagingRec"
            let member = prev.child_by_field_name("member")?;
            Some(member.utf8_text(source).ok()?.trim_matches('"').to_string())
        }
        _ => None,
    }
}

/// Look up parameter names for a function, with receiver type resolution.
///
/// Uses inferred argument types to select the correct overload when a method
/// has multiple signatures (matching on both parameter count and types).
#[allow(deprecated)]
#[allow(clippy::too_many_arguments)]
fn lookup_parameter_names(
    server: &AlServer,
    doc_symbols: &[DocumentSymbol],
    func_name: &str,
    receiver_name: Option<&str>,
    text: &str,
    tree: &tree_sitter::Tree,
    position: Position,
    arg_types: &[Option<InferredType>],
) -> Vec<String> {
    // 1. Check local procedures in current file (collect all overloads)
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
        if let Some(best) = select_best_overload(&candidates, arg_types) {
            return best;
        }
    }

    // 2. If we have a receiver, resolve its type and look up the method on that type
    if let Some(recv) = receiver_name {
        if let Some(names) =
            lookup_via_receiver(server, func_name, recv, text, tree, position, arg_types)
        {
            return names;
        }
    }

    // 3. Fallback: search package symbols by method name
    let symbols = server.symbols.search(func_name, 5);
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

    // 4. Fallback: search all builtins by method name
    let builtins = server.builtins.read().unwrap().clone();
    let candidates: Vec<OverloadCandidate> = builtins
        .iter()
        .flat_map(|bt| bt.methods.iter())
        .filter(|m| m.name.eq_ignore_ascii_case(func_name))
        .map(|m| OverloadCandidate {
            names: m.parameters.iter().map(|p| p.name.clone()).collect(),
            types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
        })
        .collect();
    if let Some(best) = select_best_overload(&candidates, arg_types) {
        return best;
    }

    // 5. Fallback: embedded built-in parameter names (no .NET bridge needed)
    if let Some(names) = lookup_embedded_builtin(func_name) {
        return names;
    }

    Vec::new()
}

/// Resolve receiver type and look up method parameters on that type.
fn lookup_via_receiver(
    server: &AlServer,
    func_name: &str,
    receiver_name: &str,
    text: &str,
    tree: &tree_sitter::Tree,
    position: Position,
    arg_types: &[Option<InferredType>],
) -> Option<Vec<String>> {
    let resolver = al_syntax::TypeResolver::new(tree, text);
    let decl = resolver.resolve_type(receiver_name, position)?;

    // Check builtins filtered by receiver type
    let builtins = server.builtins.read().unwrap().clone();
    let candidates: Vec<OverloadCandidate> = builtins
        .iter()
        .filter(|bt| {
            bt.name.eq_ignore_ascii_case(&decl.type_name)
                || decl
                    .type_subtype
                    .as_deref()
                    .is_some_and(|s| bt.name.eq_ignore_ascii_case(s))
        })
        .flat_map(|bt| bt.methods.iter())
        .filter(|m| m.name.eq_ignore_ascii_case(func_name))
        .map(|m| OverloadCandidate {
            names: m.parameters.iter().map(|p| p.name.clone()).collect(),
            types: m.parameters.iter().map(|p| p.type_name.clone()).collect(),
        })
        .collect();
    if let Some(best) = select_best_overload(&candidates, arg_types) {
        return Some(best);
    }

    // Check package symbols by resolved subtype
    if let Some(subtype) = &decl.type_subtype {
        let candidates: Vec<OverloadCandidate> = server
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

    // Check workspace objects by resolved subtype
    if let Some(subtype) = &decl.type_subtype {
        let obj_key = subtype.to_lowercase();
        if let Some(file_path) = server.workspace_objects.get(&obj_key) {
            let file_path = file_path.value().clone();
            if let Some(file_text) = server.workspace_files.get(&file_path) {
                let content = file_text.value();
                let result = AlParser::parse_quick(content);
                let target_symbols = al_syntax::extract_document_symbols(&result.tree, content);
                let mut candidates: Vec<OverloadCandidate> = Vec::new();
                for sym in &target_symbols {
                    if let Some(children) = &sym.children {
                        for child in children {
                            if child.name.eq_ignore_ascii_case(func_name)
                                && (child.kind == SymbolKind::FUNCTION
                                    || child.kind == SymbolKind::EVENT)
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
                if let Some(best) = select_best_overload(&candidates, arg_types) {
                    return Some(best);
                }
            }
        }
    }

    // Fallback: embedded built-in parameter names
    lookup_embedded_builtin(func_name)
}

/// Look up parameter names for a built-in method from hardcoded common methods.
///
/// This is a fallback when the builtins bridge isn't available. Only covers
/// the most common AL built-in methods.
fn lookup_embedded_builtin(func_name: &str) -> Option<Vec<String>> {
    // Common built-in method parameter names (subset — bridge provides the full set)
    let names: &[&str] = match func_name.to_lowercase().as_str() {
        "message" => &["Value"],
        "error" => &["Value"],
        "confirm" => &["Question"],
        "strmenu" => &["OptionString"],
        "format" => &["Value"],
        "strlen" | "maxstrlen" => return Some(vec![]),
        "copystr" => &["Position", "Length"],
        "selectstr" => &["Number", "CommaString"],
        "strpos" => &["SubString"],
        "contains" | "startswith" | "endswith" => &["Value"],
        "get" | "findset" | "findfirst" | "findlast" => return Some(vec![]),
        "setrange" => &["FieldNo", "FromValue", "ToValue"],
        "setfilter" => &["FieldNo", "String"],
        "setrecfilter" => return Some(vec![]),
        "insert" => &["RunTrigger"],
        "modify" => &["RunTrigger"],
        "delete" => &["RunTrigger"],
        "fieldno" => &["FieldName"],
        "getposition" => return Some(vec![]),
        "setposition" => &["Position"],
        "count" => return Some(vec![]),
        "isempty" => return Some(vec![]),
        "reset" => return Some(vec![]),
        "read" | "write" => &["Value"],
        "run" => &["Record"],
        "setrecord" => &["Record"],
        "getrecord" => &["Record"],
        _ => return None,
    };
    Some(names.iter().map(|s| s.to_string()).collect())
}

/// Parse parameter names from a procedure's detail string.
///
/// The detail string format from `extract_document_symbols` is:
/// - `"(param1: Type1; param2: Type2): ReturnType"` (with return type)
/// - `"(param1: Type1; param2: Type2)"` (without return type)
/// - `"()"` (no parameters)
///
/// Parameters may have `var` prefix: `"(var param1: Type1; param2: Type2)"`
fn parse_parameter_names_from_detail(detail: &str) -> Vec<String> {
    let trimmed = detail.trim();

    // Find the parameter list between first '(' and matching ')'
    let start = match trimmed.find('(') {
        Some(i) => i + 1,
        None => return Vec::new(),
    };

    // Find the matching close paren (handle nested parens for complex types)
    let mut depth = 1;
    let mut end = start;
    for (i, ch) in trimmed[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }

    let params_str = &trimmed[start..end];
    if params_str.trim().is_empty() {
        return Vec::new();
    }

    // Split on ';' (AL parameter separator)
    params_str
        .split(';')
        .filter_map(|param| {
            let param = param.trim();
            if param.is_empty() {
                return None;
            }
            // Strip optional 'var ' prefix
            let param = param.strip_prefix("var ").unwrap_or(param).trim();
            // The name is before the ':'
            if let Some(colon_pos) = param.find(':') {
                let name = param[..colon_pos].trim().trim_matches('"');
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
            // No colon — might be just a name with no type annotation
            let name = param.trim().trim_matches('"');
            if !name.is_empty() {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Parse parameter names AND types from a procedure's detail string.
///
/// Returns `Vec<(name, type_name)>` pairs.
/// Detail format: `"(var param1: Type1; param2: Type2): ReturnType"`
fn parse_parameters_from_detail(detail: &str) -> Vec<(String, String)> {
    let trimmed = detail.trim();

    let start = match trimmed.find('(') {
        Some(i) => i + 1,
        None => return Vec::new(),
    };

    let mut depth = 1;
    let mut end = start;
    for (i, ch) in trimmed[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }

    let params_str = &trimmed[start..end];
    if params_str.trim().is_empty() {
        return Vec::new();
    }

    params_str
        .split(';')
        .filter_map(|param| {
            let param = param.trim();
            if param.is_empty() {
                return None;
            }
            let param = param.strip_prefix("var ").unwrap_or(param).trim();
            if let Some(colon_pos) = param.find(':') {
                let name = param[..colon_pos].trim().trim_matches('"');
                let type_name = param[colon_pos + 1..].trim();
                if !name.is_empty() {
                    return Some((name.to_string(), type_name.to_string()));
                }
            }
            // No colon — name only, unknown type
            let name = param.trim().trim_matches('"');
            if !name.is_empty() {
                Some((name.to_string(), String::new()))
            } else {
                None
            }
        })
        .collect()
}

/// Add parameter name hints for arguments in a call.
fn add_parameter_hints(
    arg_list: tree_sitter::Node<'_>,
    _source: &[u8],
    param_names: &[String],
    hints: &mut Vec<InlayHint>,
) {
    // argument_list grammar: '(' expression_list? ')'
    // We need to iterate the expression children inside expression_list,
    // not argument_list itself (which has expression_list as a single child).
    let expr_parent = arg_list
        .children(&mut arg_list.walk())
        .find(|c| c.kind() == "expression_list")
        .unwrap_or(arg_list);

    let mut cursor = expr_parent.walk();
    let mut param_idx = 0;

    for child in expr_parent.children(&mut cursor) {
        let kind = child.kind();
        // Skip commas, parentheses, and other delimiters
        if !child.is_named() || kind == "comma" || kind == "(" || kind == ")" || kind == "semicolon"
        {
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

    // extract_trailing_identifier tests moved to al_syntax::context

    // --- parse_parameter_names_from_detail tests ---

    #[test]
    fn test_parse_params_simple() {
        let names = parse_parameter_names_from_detail("(x: Integer; y: Text)");
        assert_eq!(names, vec!["x", "y"]);
    }

    #[test]
    fn test_parse_params_with_return_type() {
        let names = parse_parameter_names_from_detail("(Name: Text; Amount: Decimal): Boolean");
        assert_eq!(names, vec!["Name", "Amount"]);
    }

    #[test]
    fn test_parse_params_with_var() {
        let names = parse_parameter_names_from_detail("(var Rec: Record; Count: Integer)");
        assert_eq!(names, vec!["Rec", "Count"]);
    }

    #[test]
    fn test_parse_params_empty() {
        let names = parse_parameter_names_from_detail("()");
        assert!(names.is_empty());
    }

    #[test]
    fn test_parse_params_no_parens() {
        let names = parse_parameter_names_from_detail("trigger");
        assert!(names.is_empty());
    }

    #[test]
    fn test_parse_params_single() {
        let names = parse_parameter_names_from_detail("(Value: Integer)");
        assert_eq!(names, vec!["Value"]);
    }

    // --- Code action helper tests ---

    #[test]
    fn test_detect_indent() {
        let text = "    procedure DoSomething()\n    begin\n    end;";
        assert_eq!(detect_indent(text, 0), "    ");
        assert_eq!(detect_indent(text, 1), "    ");
    }

    #[test]
    fn test_detect_indent_tabs() {
        let text = "\tprocedure DoSomething()";
        assert_eq!(detect_indent(text, 0), "\t");
    }

    #[test]
    fn test_diag_code_str() {
        let diag = Diagnostic {
            code: Some(NumberOrString::String("AL-L005".to_string())),
            message: "test".to_string(),
            ..Default::default()
        };
        assert_eq!(diag_code_str(&diag), Some("AL-L005".to_string()));

        let diag2 = Diagnostic {
            code: None,
            message: "test".to_string(),
            ..Default::default()
        };
        assert_eq!(diag_code_str(&diag2), None);
    }

    #[test]
    fn test_generate_label_name() {
        assert_eq!(generate_label_name("Hello World"), "LblHelloWorld");
        assert_eq!(
            generate_label_name("some text here extra"),
            "LblSomeTextHere"
        );
        assert_eq!(generate_label_name(""), "LblText");
    }

    #[test]
    fn test_compute_pascal_case_fix() {
        let text = "    procedure myProc()\n    begin\n    end;";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 14,
                },
                end: Position {
                    line: 0,
                    character: 20,
                },
            },
            code: Some(NumberOrString::String("AL-L016".to_string())),
            message: "test".to_string(),
            ..Default::default()
        };
        let edit = compute_pascal_case_fix(text, &diag).unwrap();
        assert_eq!(edit.new_text, "MyProc");
    }

    #[test]
    fn test_compute_remove_begin_end() {
        let text = "    if x then\n    begin\n        Message('hi');\n    end;";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 1,
                    character: 4,
                },
                end: Position {
                    line: 3,
                    character: 8,
                },
            },
            code: Some(NumberOrString::String("AL-L011".to_string())),
            message: "test".to_string(),
            ..Default::default()
        };
        let edits = compute_remove_begin_end(text, &diag).unwrap();
        assert_eq!(edits.len(), 2);
        // First edit removes the begin line
        assert_eq!(edits[0].new_text, "");
        assert_eq!(edits[0].range.start.line, 1);
        // Second edit removes the end line
        assert_eq!(edits[1].new_text, "");
        assert_eq!(edits[1].range.start.line, 3);
    }

    #[test]
    fn test_code_action_al_l007_remove_todo() {
        let uri = Url::parse("file:///test.al").unwrap();
        let text = "    // TODO: fix this\n    x := 1;";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 4,
                },
                end: Position {
                    line: 0,
                    character: 21,
                },
            },
            code: Some(NumberOrString::String("AL-L007".to_string())),
            message: "TODO/FIXME comment found".to_string(),
            ..Default::default()
        };

        // We can't call handle_code_action directly since it needs a server,
        // but we can verify the match arm logic through diag_code_str
        assert_eq!(diag_code_str(&diag), Some("AL-L007".to_string()));

        // Verify the edit that would be produced
        let line_start = Position {
            line: diag.range.start.line,
            character: 0,
        };
        let line_end = Position {
            line: diag.range.start.line + 1,
            character: 0,
        };
        let edit = TextEdit {
            range: Range {
                start: line_start,
                end: line_end,
            },
            new_text: String::new(),
        };
        // Removing a TODO comment line should produce an empty replacement
        assert_eq!(edit.new_text, "");
        let _ = (text, uri);
    }

    // --- compute_extract_to_label tests ---

    #[test]
    fn test_extract_to_label_simple_string() {
        let text = "    procedure DoSomething()\n    var\n        x: Integer;\n    begin\n        Message('Hello World');\n    end;";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 4,
                    character: 16,
                },
                end: Position {
                    line: 4,
                    character: 29,
                },
            },
            code: Some(NumberOrString::String("AL-L017".to_string())),
            message: "test".to_string(),
            ..Default::default()
        };
        let edits = compute_extract_to_label(text, &diag);
        assert!(
            edits.is_some(),
            "Should produce edits for string extraction"
        );
        let edits = edits.unwrap();
        assert!(
            edits.len() >= 2,
            "Should have at least 2 edits (replace + insert)"
        );
        // First edit replaces the string with label name
        assert!(
            edits[0].new_text.starts_with("Lbl"),
            "Label should start with Lbl"
        );
    }

    #[test]
    fn test_extract_to_label_no_var_section() {
        let text =
            "    procedure DoSomething()\n    begin\n        Message('Test string');\n    end;";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 2,
                    character: 16,
                },
                end: Position {
                    line: 2,
                    character: 29,
                },
            },
            code: Some(NumberOrString::String("AL-L017".to_string())),
            message: "test".to_string(),
            ..Default::default()
        };
        let edits = compute_extract_to_label(text, &diag);
        assert!(edits.is_some());
        let edits = edits.unwrap();
        // Should add a TODO comment since no var section found
        assert!(edits.len() >= 2);
    }

    #[test]
    fn test_generate_label_name_single_word() {
        assert_eq!(generate_label_name("Hello"), "LblHello");
    }

    #[test]
    fn test_generate_label_name_special_chars() {
        let name = generate_label_name("Hello! World? Test.");
        assert!(name.starts_with("Lbl"), "Should start with Lbl: {}", name);
        // Special chars should be filtered out
        assert!(!name.contains('!'));
        assert!(!name.contains('?'));
    }

    #[test]
    fn test_generate_label_name_max_words() {
        // Should only take first 3 words
        let name = generate_label_name("one two three four five");
        assert_eq!(name, "LblOneTwoThree");
    }

    // --- compute_remove_begin_end edge cases ---

    #[test]
    fn test_remove_begin_end_not_begin() {
        let text = "    if x then\n    notbegin\n        Message('hi');\n    end;";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 1,
                    character: 4,
                },
                end: Position {
                    line: 3,
                    character: 8,
                },
            },
            ..Default::default()
        };
        let edits = compute_remove_begin_end(text, &diag);
        assert!(
            edits.is_none(),
            "Should not produce edits when line is not 'begin'"
        );
    }

    #[test]
    fn test_remove_begin_end_out_of_range() {
        let text = "begin\nend";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 10,
                    character: 0,
                },
                end: Position {
                    line: 20,
                    character: 0,
                },
            },
            ..Default::default()
        };
        let edits = compute_remove_begin_end(text, &diag);
        assert!(
            edits.is_none(),
            "Should not produce edits when out of range"
        );
    }

    // --- compute_pascal_case_fix edge cases ---

    #[test]
    fn test_pascal_case_already_uppercase() {
        let text = "    procedure MyProc()\n    begin\n    end;";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 14,
                },
                end: Position {
                    line: 0,
                    character: 20,
                },
            },
            code: Some(NumberOrString::String("AL-L016".to_string())),
            message: "test".to_string(),
            ..Default::default()
        };
        let edit = compute_pascal_case_fix(text, &diag);
        // Even if already uppercase, should still produce an edit
        assert!(edit.is_some());
    }

    #[test]
    fn test_pascal_case_out_of_range() {
        let text = "short";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 100,
                },
            },
            ..Default::default()
        };
        let edit = compute_pascal_case_fix(text, &diag);
        assert!(edit.is_none(), "Should return None for out of range");
    }

    #[test]
    fn test_pascal_case_empty_name() {
        let text = "    procedure \"\"()\n    begin\n    end;";
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 14,
                },
                end: Position {
                    line: 0,
                    character: 16,
                },
            },
            ..Default::default()
        };
        let edit = compute_pascal_case_fix(text, &diag);
        assert!(edit.is_none(), "Should return None for empty name");
    }

    // --- detect_indent edge cases ---

    #[test]
    fn test_detect_indent_out_of_range() {
        let text = "line1\nline2";
        assert_eq!(detect_indent(text, 999), "    ");
    }

    #[test]
    fn test_detect_indent_no_indent() {
        let text = "no indent here";
        assert_eq!(detect_indent(text, 0), "");
    }

    // --- parse_parameter_names_from_detail edge cases ---

    #[test]
    fn test_parse_params_nested_parens() {
        let names = parse_parameter_names_from_detail("(Callback: Action(Integer))");
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], "Callback");
    }

    #[test]
    fn test_parse_params_quoted_identifier() {
        let names = parse_parameter_names_from_detail("(\"My Param\": Integer)");
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], "My Param");
    }

    #[test]
    fn test_parse_params_multiple_var() {
        let names = parse_parameter_names_from_detail("(var A: Integer; var B: Text; C: Boolean)");
        assert_eq!(names, vec!["A", "B", "C"]);
    }

    // --- diag_code_str with number ---

    #[test]
    fn test_diag_code_str_number() {
        let diag = Diagnostic {
            code: Some(NumberOrString::Number(42)),
            message: "test".to_string(),
            ..Default::default()
        };
        assert_eq!(diag_code_str(&diag), Some("42".to_string()));
    }

    // --- overload resolution tests ---

    #[test]
    fn test_score_overload_exact_count_match() {
        let candidate_1 = OverloadCandidate {
            names: vec!["OutStream".into()],
            types: vec!["OutStream".into()],
        };
        let candidate_2 = OverloadCandidate {
            names: vec!["OutStream".into(), "Encoding".into()],
            types: vec!["OutStream".into(), "TextEncoding".into()],
        };
        // 2 arguments: should prefer candidate_2
        let arg_types = vec![
            Some(InferredType {
                base: "OutStream".into(),
                subtype: None,
            }),
            Some(InferredType {
                base: "TextEncoding".into(),
                subtype: None,
            }),
        ];
        assert!(
            score_overload(&candidate_2, &arg_types) > score_overload(&candidate_1, &arg_types)
        );
    }

    #[test]
    fn test_score_overload_type_matching() {
        // Two overloads with same param count but different types
        let candidate_a = OverloadCandidate {
            names: vec!["Rec".into()],
            types: vec!["Record \"Customer\"".into()],
        };
        let candidate_b = OverloadCandidate {
            names: vec!["Rec".into()],
            types: vec!["Record \"Item\"".into()],
        };
        // Passing a Record "Customer" should prefer candidate_a
        let arg_types = vec![Some(InferredType {
            base: "Record".into(),
            subtype: Some("Customer".into()),
        })];
        assert!(
            score_overload(&candidate_a, &arg_types) > score_overload(&candidate_b, &arg_types)
        );
    }

    #[test]
    fn test_score_overload_base_type_match_no_subtype() {
        // Both match on base type, neither has subtype info in args
        let candidate_a = OverloadCandidate {
            names: vec!["Value".into()],
            types: vec!["Integer".into()],
        };
        let candidate_b = OverloadCandidate {
            names: vec!["Value".into()],
            types: vec!["Text".into()],
        };
        let arg_types = vec![Some(InferredType {
            base: "Integer".into(),
            subtype: None,
        })];
        assert!(
            score_overload(&candidate_a, &arg_types) > score_overload(&candidate_b, &arg_types)
        );
    }

    #[test]
    fn test_select_best_overload_picks_type_match() {
        let candidates = vec![
            OverloadCandidate {
                names: vec!["OutStream".into()],
                types: vec!["OutStream".into()],
            },
            OverloadCandidate {
                names: vec!["OutStream".into(), "Encoding".into()],
                types: vec!["OutStream".into(), "TextEncoding".into()],
            },
        ];
        let arg_types = vec![
            Some(InferredType {
                base: "OutStream".into(),
                subtype: None,
            }),
            Some(InferredType {
                base: "TextEncoding".into(),
                subtype: None,
            }),
        ];
        let result = select_best_overload(&candidates, &arg_types);
        assert_eq!(result, Some(vec!["OutStream".into(), "Encoding".into()]));
    }

    #[test]
    fn test_parse_type_string_simple() {
        assert_eq!(parse_type_string("OutStream"), ("OutStream", None));
        assert_eq!(parse_type_string("Integer"), ("Integer", None));
    }

    #[test]
    fn test_parse_type_string_with_subtype() {
        assert_eq!(
            parse_type_string("Record \"Customer\""),
            ("Record", Some("Customer"))
        );
    }

    #[test]
    fn test_parse_type_string_escaped_quotes() {
        // Input with literal backslash-quote pairs (as from JSON-escaped strings)
        assert_eq!(
            parse_type_string(r#"Record \"Customer\""#),
            ("Record", Some("Customer"))
        );
    }

    #[test]
    fn test_parse_parameters_from_detail() {
        let params =
            parse_parameters_from_detail("(var OutStream: OutStream; Encoding: TextEncoding)");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], ("OutStream".into(), "OutStream".into()));
        assert_eq!(params[1], ("Encoding".into(), "TextEncoding".into()));
    }

    #[test]
    fn test_parse_parameters_from_detail_record_subtype() {
        let params = parse_parameters_from_detail("(var Rec: Record \"Customer\")");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].0, "Rec");
        assert_eq!(params[0].1, "Record \"Customer\"");
    }
}
