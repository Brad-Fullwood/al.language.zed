//! Code actions query.
//!
//! Code actions are tightly coupled with LSP diagnostic types and formatting.
//! The core logic for quick-fix edits lives here; al-lsp wraps these with
//! the full LSP CodeAction/Diagnostic types.

use url::Url;

use super::{Range, TextEdit, WorkspaceEdit};
use crate::workspace::Workspace;

/// A code action (quick fix, refactoring, etc.).
#[derive(Debug, Clone)]
pub struct CodeActionEntry {
    pub title: String,
    pub kind: CodeActionKind,
    pub edit: Option<WorkspaceEdit>,
    pub is_preferred: bool,
}

/// Code action kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeActionKind {
    QuickFix,
    Refactor,
    Source,
}

/// Diagnostic info passed to code action providers.
#[derive(Debug, Clone)]
pub struct DiagnosticInfo {
    pub range: Range,
    pub message: String,
    pub code: Option<String>,
}

/// Get code actions for a range in a document.
///
/// This handles diagnostic-independent source actions (doc comment, region).
/// Diagnostic-based quick fixes are handled by `quick_fix_for_diagnostic`.
pub fn source_actions(
    workspace: &Workspace,
    uri: &Url,
    range: Range,
) -> Vec<CodeActionEntry> {
    let Some(text) = workspace.documents.get_text(uri) else {
        return Vec::new();
    };
    let lsp_range: tower_lsp::lsp_types::Range = range.into();
    let mut actions = Vec::new();

    // Add procedure documentation template
    if let Some(action) = source_action_add_doc_comment(workspace, uri, &text, lsp_range) {
        actions.push(action);
    }

    // Add region wrapper
    if range.start != range.end {
        if let Some(action) = source_action_add_region(uri, &text, lsp_range) {
            actions.push(action);
        }
    }

    // Add using statement for unresolved types in known namespaces
    actions.extend(source_action_add_using(workspace, uri, &text, range));

    // Convert if-else chain to case statement
    if let Some(action) = source_action_if_to_case(workspace, uri, &text, range) {
        actions.push(action);
    }

    // Eliminate with statement (AA0205)
    if let Some(action) = source_action_eliminate_with(workspace, uri, &text, range) {
        actions.push(action);
    }

    // Make method local (T1208)
    if let Some(action) = source_action_make_local(workspace, uri, &text, range) {
        actions.push(action);
    }

    actions
}

/// Generate namespace quick-fix actions for a diagnostic (e.g. AL0185 "Type not found").
///
/// Returns one code action per candidate namespace that contains a symbol matching the
/// unresolved type name extracted from the diagnostic message.  Returns an empty `Vec` when the
/// diagnostic is not an unresolved-type error or no matching namespaced symbols are found.
pub fn namespace_quick_fix_for_diagnostic(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    diag: &DiagnosticInfo,
) -> Vec<CodeActionEntry> {
    // Only handle AL0185 or diagnostics whose message indicates an unresolved type
    let is_al0185 = diag.code.as_deref() == Some("AL0185");
    let msg_matches = diag.message.contains("could not be found")
        || diag.message.contains("does not exist in the current context");

    if !is_al0185 && !msg_matches {
        return Vec::new();
    }

    let type_name = match extract_type_name_from_diagnostic(&diag.message) {
        Some(n) => n,
        None => return Vec::new(),
    };

    let matches = workspace.symbols.get_by_name(&type_name);
    if matches.is_empty() {
        return Vec::new();
    }

    let mut candidate_namespaces: Vec<String> = matches
        .iter()
        .filter(|e| !e.namespace.is_empty())
        .map(|e| e.namespace.clone())
        .collect();
    candidate_namespaces.sort();
    candidate_namespaces.dedup();

    if candidate_namespaces.is_empty() {
        return Vec::new();
    }

    let (existing_usings, insert_line) = parse_using_directives(text);
    candidate_namespaces.retain(|ns| {
        !existing_usings.iter().any(|u| u.eq_ignore_ascii_case(ns))
    });

    let ns_count = candidate_namespaces.len();
    candidate_namespaces
        .into_iter()
        .map(|ns| {
            let new_text = format!("using {};\n", ns);
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: insert_line, character: 0 },
                    end: super::Position { line: insert_line, character: 0 },
                },
                new_text,
            };
            CodeActionEntry {
                title: format!("Add using {}", ns),
                kind: CodeActionKind::QuickFix,
                edit: Some(WorkspaceEdit { changes: vec![(uri.clone(), vec![edit])] }),
                is_preferred: ns_count == 1,
            }
        })
        .collect()
}

/// Extract the unresolved type name from an AL diagnostic message.
///
/// Handles formats produced by the Microsoft AL compiler:
/// - `"Type 'Foo' could not be found"`
/// - `"The type 'Foo' could not be found"`
/// - `"'Foo' does not exist in the current context"`
/// - `"Foo could not be found"` (no quotes)
fn extract_type_name_from_diagnostic(message: &str) -> Option<String> {
    // Try quoted name first: 'TypeName'
    if let Some(start) = message.find('\'') {
        let rest = &message[start + 1..];
        if let Some(end) = rest.find('\'') {
            let name = &rest[..end];
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    // Fallback: "TypeName could not be found" — take the first word
    if message.contains("could not be found") {
        let word = message.split_whitespace().next()?;
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if !word.is_empty() {
            return Some(word.to_string());
        }
    }

    None
}

/// Generate a quick-fix action for a specific diagnostic code.
pub fn quick_fix_for_diagnostic(
    uri: &Url,
    text: &str,
    diag: &DiagnosticInfo,
) -> Option<CodeActionEntry> {
    let lsp_range: tower_lsp::lsp_types::Range = diag.range.into();
    match diag.code.as_deref() {
        Some("AL-L001") => {
            let indent = detect_indent(text, lsp_range.start.line);
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: lsp_range.start.line + 1, character: 0 },
                    end: super::Position { line: lsp_range.start.line + 1, character: 0 },
                },
                new_text: format!("{}    // TODO: Implement\n", indent),
            };
            Some(make_quickfix("Add TODO comment", uri, vec![edit]))
        }
        Some("AL-L005") => {
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: lsp_range.start.line, character: 0 },
                    end: super::Position { line: lsp_range.start.line + 1, character: 0 },
                },
                new_text: String::new(),
            };
            Some(make_quickfix("Remove unused variable", uri, vec![edit]))
        }
        Some("AL-L006") => {
            let indent = detect_indent(text, lsp_range.start.line);
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: lsp_range.start.line + 1, character: 0 },
                    end: super::Position { line: lsp_range.start.line + 1, character: 0 },
                },
                new_text: format!("{}        // TODO: Implement trigger\n", indent),
            };
            Some(make_quickfix("Add TODO comment to trigger", uri, vec![edit]))
        }
        Some("AL-L007") => {
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: lsp_range.start.line, character: 0 },
                    end: super::Position { line: lsp_range.start.line + 1, character: 0 },
                },
                new_text: String::new(),
            };
            Some(make_quickfix("Remove TODO comment (mark as resolved)", uri, vec![edit]))
        }
        Some("AL-L009") => {
            let indent = detect_indent(text, lsp_range.start.line);
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: lsp_range.start.line, character: 0 },
                    end: super::Position { line: lsp_range.start.line, character: 0 },
                },
                new_text: format!("{}// REFACTOR: Consider extracting parameters into a record or buffer table\n", indent),
            };
            Some(make_quickfix("Add refactoring suggestion comment", uri, vec![edit]))
        }
        Some("AL-L010") => {
            let indent = detect_indent(text, lsp_range.start.line);
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: lsp_range.end.line, character: 0 },
                    end: super::Position { line: lsp_range.end.line, character: 0 },
                },
                new_text: format!("{}    else\n{}        ; // default case\n", indent, indent),
            };
            Some(make_quickfix("Add missing 'else' branch", uri, vec![edit]))
        }
        Some("AL-L013") => {
            let indent = detect_indent(text, lsp_range.start.line);
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: lsp_range.start.line + 1, character: 0 },
                    end: super::Position { line: lsp_range.start.line + 1, character: 0 },
                },
                new_text: format!("{}    // TODO: Add loop body\n", indent),
            };
            Some(make_quickfix("Add TODO comment to loop", uri, vec![edit]))
        }
        Some("AL-L016") => {
            compute_pascal_case_fix(text, lsp_range).map(|edit| {
                make_quickfix("Fix procedure name to PascalCase", uri, vec![edit])
            })
        }
        _ => None,
    }
}

fn make_quickfix(title: &str, uri: &Url, edits: Vec<TextEdit>) -> CodeActionEntry {
    CodeActionEntry {
        title: title.to_string(),
        kind: CodeActionKind::QuickFix,
        edit: Some(WorkspaceEdit { changes: vec![(uri.clone(), edits)] }),
        is_preferred: false,
    }
}

fn detect_indent(text: &str, line: u32) -> String {
    if let Some(line_str) = text.lines().nth(line as usize) {
        let indent_len = line_str.len() - line_str.trim_start().len();
        line_str[..indent_len].to_string()
    } else {
        "    ".to_string()
    }
}

fn compute_pascal_case_fix(text: &str, range: tower_lsp::lsp_types::Range) -> Option<TextEdit> {
    let lines: Vec<&str> = text.lines().collect();
    let line = range.start.line as usize;
    if line >= lines.len() { return None; }
    let start_col = range.start.character as usize;
    let end_col = range.end.character as usize;
    let line_text = lines[line];
    if end_col > line_text.len() || start_col >= end_col { return None; }
    let name = line_text[start_col..end_col].trim_matches('"');
    if name.is_empty() { return None; }
    let mut chars = name.chars();
    let first = chars.next()?;
    let fixed = format!("{}{}", first.to_uppercase(), chars.as_str());
    Some(TextEdit { range: range.into(), new_text: fixed })
}

#[allow(deprecated)]
fn source_action_add_doc_comment(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: tower_lsp::lsp_types::Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let doc_symbols = al_syntax::extract_document_symbols(&tree, text);

    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.kind != tower_lsp::lsp_types::SymbolKind::FUNCTION { continue; }
                if range.start.line >= child.range.start.line
                    && range.start.line <= child.range.end.line
                {
                    let proc_line = child.range.start.line as usize;
                    if proc_line > 0 {
                        let lines: Vec<&str> = text.lines().collect();
                        if proc_line <= lines.len() {
                            let prev_line = lines[proc_line.saturating_sub(1)].trim();
                            if prev_line.starts_with("///") { return None; }
                        }
                    }
                    let indent = detect_indent(text, child.range.start.line);
                    let params_detail = child.detail.as_deref().unwrap_or("()");
                    let param_names = parse_parameter_names_from_detail(params_detail);
                    let mut doc = format!("{}/// <summary>\n", indent);
                    doc.push_str(&format!("{}/// Description for {}.\n", indent, child.name));
                    doc.push_str(&format!("{}/// </summary>\n", indent));
                    for param in &param_names {
                        doc.push_str(&format!("{}/// <param name=\"{}\">Description.</param>\n", indent, param));
                    }
                    if let Some(detail) = &child.detail {
                        if detail.contains("):") || detail.contains(") :") {
                            doc.push_str(&format!("{}/// <returns>Description of return value.</returns>\n", indent));
                        }
                    }
                    let edit = TextEdit {
                        range: Range {
                            start: super::Position { line: child.range.start.line, character: 0 },
                            end: super::Position { line: child.range.start.line, character: 0 },
                        },
                        new_text: doc,
                    };
                    return Some(CodeActionEntry {
                        title: "Add procedure documentation".to_string(),
                        kind: CodeActionKind::Refactor,
                        edit: Some(WorkspaceEdit { changes: vec![(uri.clone(), vec![edit])] }),
                        is_preferred: false,
                    });
                }
            }
        }
    }
    None
}

fn source_action_add_region(uri: &Url, text: &str, range: tower_lsp::lsp_types::Range) -> Option<CodeActionEntry> {
    let indent = detect_indent(text, range.start.line);
    let region_start = TextEdit {
        range: Range {
            start: super::Position { line: range.start.line, character: 0 },
            end: super::Position { line: range.start.line, character: 0 },
        },
        new_text: format!("{}//region MyRegion\n", indent),
    };
    let region_end = TextEdit {
        range: Range {
            start: super::Position { line: range.end.line + 1, character: 0 },
            end: super::Position { line: range.end.line + 1, character: 0 },
        },
        new_text: format!("{}//endregion\n", indent),
    };
    Some(CodeActionEntry {
        title: "AL: Wrap in region".to_string(),
        kind: CodeActionKind::Refactor,
        edit: Some(WorkspaceEdit { changes: vec![(uri.clone(), vec![region_start, region_end])] }),
        is_preferred: false,
    })
}

fn parse_parameter_names_from_detail(detail: &str) -> Vec<String> {
    super::parse_detail_params(detail)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect()
}

/// Offer "Add using <namespace>" actions when the cursor is on a type reference
/// whose name matches a symbol in a namespace not yet imported.
fn source_action_add_using(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Vec<CodeActionEntry> {
    // Find the word at the cursor position — look for identifiers or quoted names
    let word = extract_word_at_position(text, range);
    if word.is_empty() {
        return Vec::new();
    }

    // Look up matching symbols in the index
    let matches = workspace.symbols.get_by_name(&word);
    if matches.is_empty() {
        return Vec::new();
    }

    // Collect namespaces from matching symbols (skip empty namespaces)
    let mut candidate_namespaces: Vec<String> = matches
        .iter()
        .filter(|e| !e.namespace.is_empty())
        .map(|e| e.namespace.clone())
        .collect();
    candidate_namespaces.sort();
    candidate_namespaces.dedup();

    if candidate_namespaces.is_empty() {
        return Vec::new();
    }

    // Parse existing using directives and namespace declaration from the file
    let (existing_usings, insert_line) = parse_using_directives(text);

    // Filter out namespaces already imported
    candidate_namespaces.retain(|ns| {
        !existing_usings.iter().any(|u| u.eq_ignore_ascii_case(ns))
    });

    // Generate one code action per candidate namespace
    candidate_namespaces
        .into_iter()
        .map(|ns| {
            let new_text = format!("using {};\n", ns);
            let edit = TextEdit {
                range: Range {
                    start: super::Position { line: insert_line, character: 0 },
                    end: super::Position { line: insert_line, character: 0 },
                },
                new_text,
            };
            CodeActionEntry {
                title: format!("Add using {}", ns),
                kind: CodeActionKind::QuickFix,
                edit: Some(WorkspaceEdit { changes: vec![(uri.clone(), vec![edit])] }),
                is_preferred: false,
            }
        })
        .collect()
}

/// Extract the word at the given range position from text.
/// Handles both plain identifiers and quoted identifiers like "Customer".
///
/// LSP positions are UTF-16 code units — we convert to byte offsets before slicing.
fn extract_word_at_position(text: &str, range: Range) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = range.start.line as usize;
    if line_idx >= lines.len() {
        return String::new();
    }
    let line = lines[line_idx];

    // Convert UTF-16 column offsets to byte offsets (safe for non-ASCII identifiers).
    let start_byte = crate::resolution::utf16_col_to_byte_offset(line, range.start.character as usize);
    let end_byte = crate::resolution::utf16_col_to_byte_offset(line, range.end.character as usize);

    // If we have a real selection range, use it directly.
    if start_byte < end_byte && end_byte <= line.len() {
        return line[start_byte..end_byte].trim_matches('"').to_string();
    }

    // Otherwise expand the word around the cursor using byte-level scanning.
    // Non-ASCII bytes (>= 0x80) are not alphanumeric, so they act as word boundaries.
    if start_byte >= line.len() {
        return String::new();
    }
    let bytes = line.as_bytes();
    let cursor = start_byte;

    // If cursor is inside a quoted identifier like `"Sales Header"`, expand the full span.
    if let Some(quoted) = try_extract_quoted_identifier(bytes, cursor) {
        return quoted;
    }

    let mut word_start = cursor;
    let mut word_end = cursor;

    while word_start > 0 && (bytes[word_start - 1].is_ascii_alphanumeric() || bytes[word_start - 1] == b'_') {
        word_start -= 1;
    }
    while word_end < bytes.len() && (bytes[word_end].is_ascii_alphanumeric() || bytes[word_end] == b'_') {
        word_end += 1;
    }

    line[word_start..word_end].to_string()
}

/// If `cursor` is within a `"..."` quoted identifier span on the line, return the inner content.
/// Returns `None` if the cursor is not inside a quoted span.
fn try_extract_quoted_identifier(bytes: &[u8], cursor: usize) -> Option<String> {
    if cursor >= bytes.len() {
        return None;
    }

    // Scan left from cursor to find an opening quote.
    let open_quote = {
        let mut pos = cursor;
        loop {
            if bytes[pos] == b'"' {
                break Some(pos);
            }
            if pos == 0 {
                break None;
            }
            pos -= 1;
        }
    }?;

    // Scan right from open_quote+1 to find the closing quote.
    let close_quote = {
        let mut pos = open_quote + 1;
        while pos < bytes.len() && bytes[pos] != b'"' {
            pos += 1;
        }
        if pos < bytes.len() { Some(pos) } else { None }
    }?;

    // Cursor must be within [open_quote, close_quote] inclusive.
    if cursor < open_quote || cursor > close_quote {
        return None;
    }

    let inner = std::str::from_utf8(&bytes[open_quote + 1..close_quote]).ok()?;
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_string())
}

/// Detect an if-else chain comparing the same variable and offer to convert to case.
/// Requires 3+ branches on the same variable using `=` comparisons.
fn source_action_if_to_case(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();
    let source = text.as_bytes();

    // Find the if_statement node at the cursor position
    let point = tree_sitter::Point::new(range.start.line as usize, range.start.character as usize);
    let if_node = find_outermost_if_at_point(root, point)?;

    // Walk the if-else chain collecting (variable_text, value_text, body_text) triples
    let mut branches: Vec<(String, String, String)> = Vec::new();
    let mut else_body: Option<String> = None;
    let mut common_var: Option<String> = None;

    walk_if_chain(if_node, source, &mut branches, &mut else_body, &mut common_var);

    // Need 3+ branches and all on the same variable
    if branches.len() < 3 || common_var.is_none() {
        return None;
    }

    let var_name = common_var.unwrap();
    let indent = detect_indent(text, if_node.start_position().row as u32);
    let inner_indent = format!("{}    ", indent);
    let body_indent = format!("{}        ", indent);

    let mut case_text = format!("{}case {} of\n", indent, var_name);
    for (_, value, body) in &branches {
        let body_lines = body.trim();
        case_text.push_str(&format!("{}{}:\n", inner_indent, value));
        for bl in body_lines.lines() {
            case_text.push_str(&format!("{}{}\n", body_indent, bl.trim()));
        }
    }
    if let Some(ref eb) = else_body {
        let eb_trimmed = eb.trim();
        case_text.push_str(&format!("{}else\n", inner_indent));
        for bl in eb_trimmed.lines() {
            case_text.push_str(&format!("{}{}\n", body_indent, bl.trim()));
        }
    }
    case_text.push_str(&format!("{}end\n", indent));

    let edit = TextEdit {
        range: Range {
            start: super::Position {
                line: if_node.start_position().row as u32,
                character: 0,
            },
            end: super::Position {
                line: if_node.end_position().row as u32 + 1,
                character: 0,
            },
        },
        new_text: case_text,
    };

    Some(CodeActionEntry {
        title: format!("Convert to case statement on '{}'", var_name),
        kind: CodeActionKind::Refactor,
        edit: Some(WorkspaceEdit { changes: vec![(uri.clone(), vec![edit])] }),
        is_preferred: false,
    })
}

/// Find the outermost if_statement containing the given point.
/// Walks up the tree to find the topmost if_statement in the chain.
fn find_outermost_if_at_point(root: tree_sitter::Node, point: tree_sitter::Point) -> Option<tree_sitter::Node> {
    let mut node = root.descendant_for_point_range(point, point)?;

    // Walk up to find an if_statement
    while node.kind() != "if_statement" {
        node = node.parent()?;
    }

    // Walk up further to find the outermost if in the chain.
    // The chain structure is: if_statement -> statement(alternative) -> if_statement
    // So the parent of an inner if_statement is a `statement` node, whose parent is the outer if_statement.
    loop {
        let stmt_parent = node.parent();
        if let Some(stmt) = stmt_parent {
            if let Some(if_parent) = stmt.parent() {
                if if_parent.kind() == "if_statement" {
                    if let Some(alt) = if_parent.child_by_field_name("alternative") {
                        if alt.id() == stmt.id() {
                            node = if_parent;
                            continue;
                        }
                    }
                }
            }
        }
        break;
    }

    Some(node)
}

/// Walk an if-else chain, collecting branches.
/// Each branch is (variable_text, value_text, body_text).
/// Sets common_var to None if variables differ across branches.
fn walk_if_chain(
    node: tree_sitter::Node,
    source: &[u8],
    branches: &mut Vec<(String, String, String)>,
    else_body: &mut Option<String>,
    common_var: &mut Option<String>,
) {
    if node.kind() != "if_statement" {
        return;
    }

    // Extract condition: expect `<var> = <value>`
    if let Some(condition) = node.child_by_field_name("condition") {
        if let Some((var, val)) = extract_equality_operands(condition, source) {
            match common_var {
                Some(ref cv) if !cv.eq_ignore_ascii_case(&var) => {
                    *common_var = None;
                    return;
                }
                None if branches.is_empty() => {
                    *common_var = Some(var.clone());
                }
                _ => {}
            }

            let body = node
                .child_by_field_name("consequence")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            branches.push((var, val, body));
        } else {
            // Condition is not a simple equality — can't convert
            *common_var = None;
            return;
        }
    }

    // Follow the else branch
    if let Some(alt) = node.child_by_field_name("alternative") {
        // The alternative is a `statement` node wrapping the actual node
        let inner = if alt.kind() == "statement" && alt.child_count() == 1 {
            alt.child(0).unwrap_or(alt)
        } else {
            alt
        };
        if inner.kind() == "if_statement" {
            walk_if_chain(inner, source, branches, else_body, common_var);
        } else {
            // Terminal else body
            *else_body = alt.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
}

/// Extract (left, right) from an equality expression `left = right`.
/// Tree-sitter structure: expression -> unary_expression, binary_operator(operator "="), unary_expression
fn extract_equality_operands(node: tree_sitter::Node, source: &[u8]) -> Option<(String, String)> {
    let child_count = node.child_count();

    // Single-child wrapper: descend
    if child_count == 1 {
        let inner = node.child(0)?;
        return extract_equality_operands(inner, source);
    }

    if child_count < 3 {
        return None;
    }

    // Find the `=` operator — can be `binary_operator` wrapping `operator`, or direct `operator`
    let mut eq_idx = None;
    for i in 0..child_count {
        let child = node.child(i)?;
        let is_eq = match child.kind() {
            "binary_operator" | "operator" => {
                child.utf8_text(source).ok().map(|t| t.trim() == "=").unwrap_or(false)
            }
            _ => false,
        };
        if is_eq {
            eq_idx = Some(i);
            break;
        }
    }

    let eq_idx = eq_idx?;
    if eq_idx == 0 || eq_idx + 1 >= child_count {
        return None;
    }

    // Use byte ranges for precise text extraction
    let left_start = node.child(0)?.start_byte();
    let left_end = node.child(eq_idx)?.start_byte();
    let left = std::str::from_utf8(&source[left_start..left_end])
        .ok()?
        .trim()
        .to_string();

    let right_start = node.child(eq_idx)?.end_byte();
    let right_end = node.child(child_count - 1)?.end_byte();
    let right = std::str::from_utf8(&source[right_start..right_end])
        .ok()?
        .trim()
        .to_string();

    if left.is_empty() || right.is_empty() {
        return None;
    }

    Some((left, right))
}

/// Generate "Implement interface" code actions for codeunits with `implements` clauses.
///
/// For each interface that the codeunit declares it implements, checks which methods
/// are missing and offers to generate stub procedure declarations.
fn source_action_implement_interface(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Vec<CodeActionEntry> {
    let Some((_, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let source = text.as_bytes();

    // Find the codeunit object declaration at the cursor position
    let point = tree_sitter::Point::new(range.start.line as usize, range.start.character as usize);
    let obj_node = match find_codeunit_at_point(root, source, point) {
        Some(n) => n,
        None => return Vec::new(),
    };

    // Extract interface names from the implements clause
    let interface_names = extract_interface_names(obj_node, source);
    eprintln!("DEBUG: interface_names={:?}", interface_names);
    if interface_names.is_empty() {
        return Vec::new();
    }

    // Collect existing procedure names in this codeunit (case-insensitive)
    let existing_procs = collect_existing_procedures(obj_node, source);

    // Find the insertion point — before the closing `}` of the codeunit body
    let insert_line = find_stub_insertion_line(obj_node);

    let mut actions = Vec::new();

    for iface_name in &interface_names {
        // Look up the interface in the symbol index
        let iface_lower = iface_name.to_lowercase();
        let interfaces = workspace.symbols.get_by_kind(al_symbols::model::ObjectKind::Interface);
        let iface_entry = interfaces.iter().find(|e| e.name.to_lowercase() == iface_lower);
        let iface_entry = match iface_entry {
            Some(e) => e,
            None => continue,
        };

        // Filter to missing methods only
        let missing: Vec<_> = iface_entry
            .methods
            .iter()
            .filter(|m| !existing_procs.iter().any(|p| p.eq_ignore_ascii_case(&m.name)))
            .collect();

        if missing.is_empty() {
            continue;
        }

        // Generate stub text
        let indent = detect_indent(text, insert_line.saturating_sub(1));
        let mut stub_text = String::new();

        for method in &missing {
            stub_text.push('\n');
            // Procedure signature
            stub_text.push_str(&indent);
            stub_text.push_str("procedure ");
            stub_text.push_str(&method.name);
            stub_text.push('(');

            // Parameters
            let param_strs: Vec<String> = method
                .parameters
                .iter()
                .map(|p| {
                    if p.is_var {
                        format!("var {}: {}", p.name, p.type_name)
                    } else {
                        format!("{}: {}", p.name, p.type_name)
                    }
                })
                .collect();
            stub_text.push_str(&param_strs.join("; "));
            stub_text.push(')');

            // Return type
            if let Some(ref ret) = method.return_type {
                stub_text.push_str(": ");
                stub_text.push_str(ret);
            }

            stub_text.push('\n');

            // Body
            stub_text.push_str(&indent);
            stub_text.push_str("begin\n");
            stub_text.push_str(&indent);
            stub_text.push_str("    // TODO: Implement\n");
            stub_text.push_str(&indent);
            stub_text.push_str("end;\n");
        }

        let edit = TextEdit {
            range: Range {
                start: super::Position { line: insert_line, character: 0 },
                end: super::Position { line: insert_line, character: 0 },
            },
            new_text: stub_text,
        };

        actions.push(CodeActionEntry {
            title: format!("Implement interface '{}'", iface_name),
            kind: CodeActionKind::Refactor,
            edit: Some(WorkspaceEdit {
                changes: vec![(uri.clone(), vec![edit])],
            }),
            is_preferred: false,
        });
    }

    actions
}

/// Find the codeunit object declaration node containing `point`.
fn find_codeunit_at_point<'a>(
    root: tree_sitter::Node<'a>,
    source: &[u8],
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node<'a>> {
    for i in 0..root.child_count() {
        let child = root.child(i)?;
        if child.kind() != "object_declaration" {
            continue;
        }
        let ts_range = child.range();
        if point.row < ts_range.start_point.row || point.row > ts_range.end_point.row {
            continue;
        }
        // Check it's a codeunit
        let is_codeunit = child
            .child(0)
            .and_then(|kw| kw.utf8_text(source).ok())
            .map(|kw| kw.eq_ignore_ascii_case("codeunit"))
            .unwrap_or(false);
        if is_codeunit {
            return Some(child);
        }
    }
    None
}

/// Extract interface names from the object declaration node.
///
/// tree-sitter-al parses `implements IFoo, IBar` as:
///   keyword("implements") identifier("IFoo") , identifier("IBar")
/// as direct children of `object_declaration` (no wrapper node).
fn extract_interface_names(obj_node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut found_implements = false;

    for i in 0..obj_node.child_count() {
        let Some(child) = obj_node.child(i) else { continue };

        if child.kind() == "keyword" {
            if let Ok(t) = child.utf8_text(source) {
                if t.eq_ignore_ascii_case("implements") {
                    found_implements = true;
                    continue;
                }
            }
        }

        if found_implements {
            // Stop at the object body
            if child.kind() == "object_body" {
                break;
            }
            if child.kind() == "identifier" || child.kind() == "quoted_identifier" {
                if let Ok(t) = child.utf8_text(source) {
                    let name = t.trim().trim_matches('"');
                    if !name.is_empty() {
                        names.push(name.to_string());
                    }
                }
            }
            // Skip comma punctuation
        }
    }

    names
}

/// Collect names of existing procedure declarations in the object node.
fn collect_existing_procedures(obj_node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut procs = Vec::new();
    collect_procs_recursive(obj_node, source, &mut procs);
    procs
}

fn collect_procs_recursive(node: tree_sitter::Node, source: &[u8], procs: &mut Vec<String>) {
    if node.kind() == "procedure_declaration" || node.kind() == "event_procedure_declaration" {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source) {
                procs.push(name.trim_matches('"').to_string());
            }
        }
        return;
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            collect_procs_recursive(child, source, procs);
        }
    }
}

/// Find the line to insert stubs — the line of the closing `}` of the codeunit.
fn find_stub_insertion_line(obj_node: tree_sitter::Node) -> u32 {
    obj_node.end_position().row as u32
}

/// Eliminate a `with` statement by qualifying field references with the record variable.
/// Detects `with X do begin...end` and replaces with the body, prefixing unqualified
/// identifiers with `X.` (AA0205 compliance).
fn source_action_eliminate_with(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();
    let source = text.as_bytes();

    // Find with_statement at cursor
    let point = tree_sitter::Point::new(range.start.line as usize, range.start.character as usize);
    let with_node = find_with_at_point(root, point)?;

    // Extract the record variable name from the `value` field
    let value_node = with_node.child_by_field_name("value")?;
    let record_var = value_node.utf8_text(source).ok()?.trim().to_string();
    if record_var.is_empty() {
        return None;
    }

    // Extract the body
    let body_node = with_node.child_by_field_name("body")?;

    // Unwrap begin..end if present
    let (body_text, _is_begin_end) = extract_with_body(body_node, source);

    let indent = detect_indent(text, with_node.start_position().row as u32);

    // Qualify unqualified identifiers in the body with `record_var.`
    let qualified_body = qualify_with_references(&body_text, &record_var, &indent);

    let edit = TextEdit {
        range: Range {
            start: super::Position {
                line: with_node.start_position().row as u32,
                character: 0,
            },
            end: super::Position {
                line: with_node.end_position().row as u32 + 1,
                character: 0,
            },
        },
        new_text: qualified_body,
    };

    Some(CodeActionEntry {
        title: format!("Eliminate with {} (AA0205)", record_var),
        kind: CodeActionKind::Refactor,
        edit: Some(WorkspaceEdit { changes: vec![(uri.clone(), vec![edit])] }),
        is_preferred: false,
    })
}

/// Find a with_statement node at the given point.
fn find_with_at_point(root: tree_sitter::Node, point: tree_sitter::Point) -> Option<tree_sitter::Node> {
    let mut node = root.descendant_for_point_range(point, point)?;
    while node.kind() != "with_statement" {
        node = node.parent()?;
    }
    Some(node)
}

/// Extract the body text from a with statement body node.
/// Returns (body text, whether it was a begin..end block).
fn extract_with_body(body_node: tree_sitter::Node, source: &[u8]) -> (String, bool) {
    // Check if body is a begin..end block
    let inner = if body_node.kind() == "statement" && body_node.child_count() == 1 {
        body_node.child(0).unwrap_or(body_node)
    } else {
        body_node
    };

    if inner.kind() == "begin_end_block" {
        // Extract just the statements inside begin..end (skip begin/end keywords)
        if let Some(stmt_list) = inner.child_by_field_name("body") {
            let text = stmt_list.utf8_text(source).unwrap_or("").to_string();
            return (text, true);
        }
        // Fallback: try to get statement_list child
        let mut cursor = inner.walk();
        for child in inner.children(&mut cursor) {
            if child.kind() == "statement_list" {
                let text = child.utf8_text(source).unwrap_or("").to_string();
                return (text, true);
            }
        }
    }

    let text = body_node.utf8_text(source).unwrap_or("").to_string();
    (text, false)
}

/// Qualify field references in with-body text by prepending the record variable.
/// For each line, if it starts with an unqualified identifier assignment or method call,
/// prepend `record_var.`.
fn qualify_with_references(body: &str, record_var: &str, indent: &str) -> String {
    let mut result = String::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            result.push_str(&format!("{}{}\n", indent, trimmed));
            continue;
        }

        // Check if line starts with an assignment target or method call that should be qualified
        // Pattern: identifier or "quoted id" followed by := or ( or .
        let qualified_line = qualify_line(trimmed, record_var);
        result.push_str(&format!("{}{}\n", indent, qualified_line));
    }

    result
}

/// Qualify a single line's leading identifier with the record variable.
fn qualify_line(line: &str, record_var: &str) -> String {
    let trimmed = line.trim();

    // Skip lines that are already qualified (contain . before := or ()
    // Skip keywords: if, then, else, begin, end, for, while, repeat, etc.
    // Use whole-word matching to avoid false positives on field names like EndDate, FormatText, CaseNo.
    let lower = trimmed.to_lowercase();
    let al_keywords = [
        "if", "then", "else", "begin", "end", "for", "while", "repeat", "until",
        "case", "exit", "error", "message", "//", "end;",
    ];
    for kw in &al_keywords {
        if let Some(rest) = lower.strip_prefix(kw) {
            // Ensure this is a whole-word match: next char must be non-alphanumeric/non-underscore
            let is_word_boundary = rest.is_empty()
                || rest.starts_with(|c: char| !c.is_alphanumeric() && c != '_');
            if is_word_boundary {
                return trimmed.to_string();
            }
        }
    }

    // Check if it starts with a quoted identifier
    if let Some(after_open) = trimmed.strip_prefix('"') {
        if let Some(end_quote) = after_open.find('"') {
            let after = after_open[end_quote + 1..].trim_start();
            // If followed by := or ( it's a field/method reference to qualify
            if after.starts_with(":=") || after.starts_with('(') {
                return format!("{}.{}", record_var, trimmed);
            }
        }
        return trimmed.to_string();
    }

    // Check if it starts with an identifier followed by := or (
    let first_word_end = trimmed
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(trimmed.len());
    let first_word = &trimmed[..first_word_end];
    if first_word.is_empty() {
        return trimmed.to_string();
    }

    let after_word = trimmed[first_word_end..].trim_start();
    // Already qualified with a dot
    if after_word.starts_with('.') {
        return trimmed.to_string();
    }
    // Assignment or method call — qualify it
    if after_word.starts_with(":=") || after_word.starts_with('(') {
        return format!("{}.{}", record_var, trimmed);
    }

    trimmed.to_string()
}

/// Parse the file's namespace and using directives.
/// Returns (list of imported namespaces, line number to insert new using directives).
fn parse_using_directives(text: &str) -> (Vec<String>, u32) {
    let mut usings = Vec::new();
    let mut last_directive_line: u32 = 0;
    let mut found_any_directive = false;

    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("namespace ") || trimmed.starts_with("Namespace ") {
            last_directive_line = i as u32;
            found_any_directive = true;
        } else if trimmed.starts_with("using ") || trimmed.starts_with("Using ") {
            // Extract namespace name: "using Foo.Bar;" -> "Foo.Bar"
            // Strip trailing inline comment before the semicolon (e.g. "using Foo; // comment")
            let after_keyword = trimmed
                .strip_prefix("using ")
                .or_else(|| trimmed.strip_prefix("Using "))
                .unwrap_or("");
            // Remove inline comment (// ...) before parsing
            let without_comment = if let Some(pos) = after_keyword.find("//") {
                &after_keyword[..pos]
            } else {
                after_keyword
            };
            let ns = without_comment.trim_end_matches(';').trim();
            if !ns.is_empty() {
                usings.push(ns.to_string());
            }
            last_directive_line = i as u32;
            found_any_directive = true;
        } else if found_any_directive && !trimmed.is_empty() && !trimmed.starts_with("//") {
            // Stop scanning after we pass the header section
            break;
        }
    }

    // Insert after the last directive line
    let insert_line = if found_any_directive { last_directive_line + 1 } else { 0 };
    (usings, insert_line)
}

/// T1208: Make method local — offer to add `local` keyword when procedure has no external callers.
#[allow(dead_code)]
fn source_action_make_local(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();

    let point = tree_sitter::Point::new(range.start.line as usize, range.start.character as usize);
    let mut node = root.descendant_for_point_range(point, point)?;

    // Walk up to find procedure_declaration (skip trigger_declaration)
    loop {
        if node.kind() == "procedure_declaration" {
            break;
        }
        if node.kind() == "trigger_declaration" {
            return None;
        }
        node = node.parent()?;
    }

    // Check if already has 'local' modifier
    let proc_line = node.start_position().row;
    let line_text = text.lines().nth(proc_line)?;
    let lower = line_text.to_lowercase();
    if lower.contains("local ") {
        return None;
    }

    // Find `procedure` keyword position in the line and replace with `local procedure`
    let proc_col = lower.find("procedure")?;
    let proc_end_col = proc_col + "procedure".len();

    let edit = TextEdit {
        range: Range {
            start: super::Position { line: proc_line as u32, character: proc_col as u32 },
            end: super::Position { line: proc_line as u32, character: proc_end_col as u32 },
        },
        new_text: "local procedure".to_string(),
    };

    Some(CodeActionEntry {
        title: "Make procedure local".to_string(),
        kind: CodeActionKind::Refactor,
        edit: Some(WorkspaceEdit { changes: vec![(uri.clone(), vec![edit])] }),
        is_preferred: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use al_symbols::{ObjectKind, SymbolEntry};

    fn make_entry_with_namespace(kind: ObjectKind, id: i32, name: &str, ns: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            package: "TestPkg".to_string(),
            namespace: ns.to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    #[test]
    fn add_using_offered_for_unresolved_type_in_known_namespace() {
        let ws = Workspace::new();

        // Add a symbol "Customer" in namespace "Microsoft.Sales"
        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
        ]);

        // AL file that uses Customer but doesn't have the right `using`
        let al_code = r#"namespace MyCompany.MyApp;

codeunit 50100 "My Codeunit"
{
    var
        Cust: Record Customer;
}
"#;
        let uri = Url::parse("file:///test/MyCU.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        // Cursor on line 5: "        Cust: Record Customer;"
        // "Customer" starts at col 21, ends at col 29
        let range = Range {
            start: super::super::Position { line: 5, character: 21 },
            end: super::super::Position { line: 5, character: 29 },
        };

        let actions = source_actions(&ws, &uri, range);
        let using_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("using"))
            .collect();

        assert!(!using_actions.is_empty(), "Should offer 'Add using' action");
        assert!(using_actions[0].title.contains("Microsoft.Sales"));

        // The edit should insert `using Microsoft.Sales;` after the namespace line
        let edit = using_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        assert!(edits[0].new_text.contains("using Microsoft.Sales;"));
    }

    #[test]
    fn add_using_not_offered_when_namespace_already_imported() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
        ]);

        // File already has `using Microsoft.Sales;`
        let al_code = r#"namespace MyCompany.MyApp;
using Microsoft.Sales;

codeunit 50100 "My Codeunit"
{
    var
        Cust: Record Customer;
}
"#;
        let uri = Url::parse("file:///test/MyCU.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        // Line 6: "        Cust: Record Customer;" — Customer at col 21-29
        let range = Range {
            start: super::super::Position { line: 6, character: 21 },
            end: super::super::Position { line: 6, character: 29 },
        };

        let actions = source_actions(&ws, &uri, range);
        let using_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("using"))
            .collect();

        assert!(using_actions.is_empty(), "Should NOT offer 'Add using' when already imported");
    }

    #[test]
    fn add_using_not_offered_when_no_matching_symbol() {
        let ws = Workspace::new();

        // No symbols added — nothing in the index
        let al_code = r#"namespace MyCompany.MyApp;

codeunit 50100 "My Codeunit"
{
    var
        Cust: Record NonExistentTable;
}
"#;
        let uri = Url::parse("file:///test/MyCU.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        // "NonExistentTable" at col 21-37
        let range = Range {
            start: super::super::Position { line: 5, character: 21 },
            end: super::super::Position { line: 5, character: 37 },
        };

        let actions = source_actions(&ws, &uri, range);
        let using_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("using"))
            .collect();

        assert!(using_actions.is_empty(), "Should NOT offer 'Add using' for unknown types");
    }

    #[test]
    fn add_using_offers_multiple_namespaces() {
        let ws = Workspace::new();

        // Same type name in different namespaces
        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
            make_entry_with_namespace(ObjectKind::Table, 50100, "Customer", "MyCompany.CRM"),
        ]);

        let al_code = r#"namespace MyCompany.MyApp;

codeunit 50100 "My Codeunit"
{
    var
        Cust: Record Customer;
}
"#;
        let uri = Url::parse("file:///test/MyCU.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        // "Customer" at col 21-29
        let range = Range {
            start: super::super::Position { line: 5, character: 21 },
            end: super::super::Position { line: 5, character: 29 },
        };

        let actions = source_actions(&ws, &uri, range);
        let using_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("using"))
            .collect();

        assert_eq!(using_actions.len(), 2, "Should offer one action per namespace");
    }

    // -----------------------------------------------------------------------
    // Tests for parse_using_directives (insertion point logic)
    // -----------------------------------------------------------------------

    #[test]
    fn insertion_point_after_existing_using_directives() {
        let source = "namespace MyApp;\nusing Foo.Bar;\nusing Baz.Qux;\n\ncodeunit 50100 Test { }\n";
        let (usings, insert_line) = parse_using_directives(source);
        assert_eq!(usings, vec!["Foo.Bar", "Baz.Qux"]);
        // Should insert after line 2 (0-indexed), i.e. insert_line == 3
        assert_eq!(insert_line, 3, "should insert after last using line");
    }

    #[test]
    fn insertion_point_after_namespace_when_no_usings() {
        let source = "namespace MyApp;\n\ncodeunit 50100 Test { }\n";
        let (usings, insert_line) = parse_using_directives(source);
        assert!(usings.is_empty());
        // Namespace line is 0, so insert after it at line 1
        assert_eq!(insert_line, 1, "should insert after namespace line");
    }

    #[test]
    fn insertion_point_at_top_when_no_header() {
        let source = "codeunit 50100 Test { }\n";
        let (usings, insert_line) = parse_using_directives(source);
        assert!(usings.is_empty());
        assert_eq!(insert_line, 0, "should insert at top when no header");
    }

    // -----------------------------------------------------------------------
    // Tests for extract_type_name_from_diagnostic
    // -----------------------------------------------------------------------

    #[test]
    fn extract_type_name_from_quoted_al0185_message() {
        assert_eq!(
            extract_type_name_from_diagnostic("Type 'SalesHeader' could not be found"),
            Some("SalesHeader".to_string()),
        );
    }

    #[test]
    fn extract_type_name_from_unquoted_message() {
        assert_eq!(
            extract_type_name_from_diagnostic("MyTable could not be found"),
            Some("MyTable".to_string()),
        );
    }

    #[test]
    fn extract_type_name_from_does_not_exist_message() {
        assert_eq!(
            extract_type_name_from_diagnostic("'PostingGroup' does not exist in the current context"),
            Some("PostingGroup".to_string()),
        );
    }

    // -----------------------------------------------------------------------
    // Tests for namespace_quick_fix_for_diagnostic
    // -----------------------------------------------------------------------

    #[test]
    fn diagnostic_quick_fix_offered_for_al0185() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
        ]);

        let al_code = "namespace MyApp;\n\ncodeunit 50100 Test { var c: Record Customer; }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Type 'Customer' could not be found".to_string(),
            code: Some("AL0185".to_string()),
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].title.contains("Microsoft.Sales"));
        assert!(actions[0].is_preferred, "single match should be preferred");

        let edit = actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        assert!(edits[0].new_text.contains("using Microsoft.Sales;"));
        // Insert after namespace line (line 0), so at line 1
        assert_eq!(edits[0].range.start.line, 1);
    }

    #[test]
    fn diagnostic_quick_fix_not_preferred_when_multiple_namespaces() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
            make_entry_with_namespace(ObjectKind::Table, 50100, "Customer", "MyCompany.CRM"),
        ]);

        let al_code = "namespace MyApp;\n\ncodeunit 50100 Test { }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Type 'Customer' could not be found".to_string(),
            code: Some("AL0185".to_string()),
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert_eq!(actions.len(), 2);
        for a in &actions {
            assert!(!a.is_preferred, "should not be preferred when multiple options");
        }
    }

    #[test]
    fn diagnostic_quick_fix_skipped_when_already_imported() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
        ]);

        let al_code = "namespace MyApp;\nusing Microsoft.Sales;\n\ncodeunit 50100 Test { }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Type 'Customer' could not be found".to_string(),
            code: Some("AL0185".to_string()),
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert!(actions.is_empty(), "should not offer when namespace already imported");
    }

    #[test]
    fn diagnostic_quick_fix_ignored_for_unrelated_diagnostic() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
        ]);

        let al_code = "namespace MyApp;\n\ncodeunit 50100 Test { }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        // Unrelated diagnostic code
        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Some other compile error".to_string(),
            code: Some("AL-L001".to_string()),
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert!(actions.is_empty(), "should return nothing for unrelated diagnostics");
    }

    #[test]
    fn diagnostic_quick_fix_works_via_message_without_code() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Codeunit, 50100, "PostingSetup", "MyNs.Finance"),
        ]);

        let al_code = "namespace MyApp;\n\ncodeunit 50200 Test { }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Type 'PostingSetup' could not be found".to_string(),
            code: None, // no code — message-based detection
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].title.contains("MyNs.Finance"));
    }

    // -----------------------------------------------------------------------
    // Tests for if-to-case conversion (T1203)
    // -----------------------------------------------------------------------

    #[test]
    fn if_to_case_offered_for_3_branch_chain() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff(x: Integer)
    begin
        if x = 1 then
            Message('one')
        else if x = 2 then
            Message('two')
        else if x = 3 then
            Message('three');
    end;
}
"#;
        let uri = Url::parse("file:///test/IfCase.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        // Cursor on the first `if` line (line 4)
        let range = Range {
            start: super::super::Position { line: 4, character: 8 },
            end: super::super::Position { line: 4, character: 8 },
        };

        let actions = source_actions(&ws, &uri, range);
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(!case_actions.is_empty(), "Should offer 'Convert to case' action");

        let edit = case_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;
        assert!(new_text.contains("case x of"), "Should have 'case x of'");
        assert!(new_text.contains("1:"), "Should have branch for value 1");
        assert!(new_text.contains("2:"), "Should have branch for value 2");
        assert!(new_text.contains("3:"), "Should have branch for value 3");
        assert!(new_text.contains("end"), "Should close with 'end'");
    }

    #[test]
    fn if_to_case_not_offered_for_2_branches() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff(x: Integer)
    begin
        if x = 1 then
            Message('one')
        else if x = 2 then
            Message('two');
    end;
}
"#;
        let uri = Url::parse("file:///test/IfCase.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 4, character: 8 },
            end: super::super::Position { line: 4, character: 8 },
        };

        let actions = source_actions(&ws, &uri, range);
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(case_actions.is_empty(), "Should NOT offer conversion for only 2 branches");
    }

    #[test]
    fn if_to_case_not_offered_for_different_variables() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff(x: Integer; y: Integer)
    begin
        if x = 1 then
            Message('one')
        else if y = 2 then
            Message('two')
        else if x = 3 then
            Message('three');
    end;
}
"#;
        let uri = Url::parse("file:///test/IfCase.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 4, character: 8 },
            end: super::super::Position { line: 4, character: 8 },
        };

        let actions = source_actions(&ws, &uri, range);
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(case_actions.is_empty(), "Should NOT offer when branches compare different variables");
    }

    #[test]
    fn if_to_case_preserves_else_clause() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff(x: Integer)
    begin
        if x = 1 then
            Message('one')
        else if x = 2 then
            Message('two')
        else if x = 3 then
            Message('three')
        else
            Message('default');
    end;
}
"#;
        let uri = Url::parse("file:///test/IfCase.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 4, character: 8 },
            end: super::super::Position { line: 4, character: 8 },
        };

        let actions = source_actions(&ws, &uri, range);
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(!case_actions.is_empty(), "Should offer conversion with else");

        let edit = case_actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;
        assert!(new_text.contains("else"), "Should preserve else clause");
    }

    // -----------------------------------------------------------------------
    // Tests for with-statement elimination (T1204)
    // -----------------------------------------------------------------------

    #[test]
    fn with_elimination_offered_on_with_statement() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    var
        Cust: Record Customer;
    begin
        with Cust do begin
            Name := 'Test';
            "No." := '10000';
        end;
    end;
}
"#;
        let uri = Url::parse("file:///test/With.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        // Cursor on the `with` line (line 6)
        let range = Range {
            start: super::super::Position { line: 6, character: 8 },
            end: super::super::Position { line: 6, character: 8 },
        };

        let actions = source_actions(&ws, &uri, range);
        let with_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("with"))
            .collect();

        assert!(!with_actions.is_empty(), "Should offer 'Eliminate with' action");

        let edit = with_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;
        // Should qualify field references with Cust.
        assert!(new_text.contains("Cust.Name"), "Should qualify Name with Cust");
        assert!(new_text.contains("Cust.\"No.\""), "Should qualify \"No.\" with Cust");
        // Should NOT have with...do wrapper
        assert!(!new_text.contains("with Cust do"), "Should remove with wrapper");
    }

    #[test]
    fn qualify_line_does_not_skip_keyword_prefixed_identifiers() {
        // Field names that start with AL keywords must still be qualified.
        // e.g. EndDate, FormatText, CaseNo must not be silently dropped by
        // the whole-word keyword guard.
        assert!(
            qualify_line("EndDate := Today;", "Rec").starts_with("Rec."),
            "EndDate starts with 'end' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("FormatText := 'X';", "Rec").starts_with("Rec."),
            "FormatText starts with 'for' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("CaseNo := 1;", "Rec").starts_with("Rec."),
            "CaseNo starts with 'case' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("IfFlag := true;", "Rec").starts_with("Rec."),
            "IfFlag starts with 'if' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("MessageText := '';", "Rec").starts_with("Rec."),
            "MessageText starts with 'message' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("ExitCode := 0;", "Rec").starts_with("Rec."),
            "ExitCode starts with 'exit' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("ErrorText := '';", "Rec").starts_with("Rec."),
            "ErrorText starts with 'error' but is not a keyword — must be qualified"
        );
    }

    #[test]
    fn qualify_line_skips_al_keywords_exactly() {
        // Exact keyword lines must not be qualified.
        assert_eq!(qualify_line("end;", "Rec"), "end;");
        assert_eq!(qualify_line("begin", "Rec"), "begin");
        assert_eq!(qualify_line("end", "Rec"), "end");
        assert!(qualify_line("if x = 1 then", "Rec").starts_with("if"));
        assert!(qualify_line("for i := 1 to 10 do", "Rec").starts_with("for"));
        assert!(qualify_line("while x > 0 do", "Rec").starts_with("while"));
        assert!(qualify_line("case x of", "Rec").starts_with("case"));
    }

    #[test]
    fn qualify_line_skips_already_qualified_references() {
        // Rec.Field := X should not become Rec2.Rec.Field := X
        assert_eq!(qualify_line("Rec.Name := 'X';", "Rec2"), "Rec.Name := 'X';");
        assert_eq!(qualify_line("Customer.\"No.\" := '100';", "Rec"), "Customer.\"No.\" := '100';");
    }

    // T1202 interface implementer tests — removed, function not yet implemented
    // Helper functions and tests will be added when T1202 is implemented

    #[allow(dead_code)]
    fn make_interface_entry(name: &str, methods: Vec<al_symbols::MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Interface,
            id: 0,
            name: name.to_string(),
            methods,
            ..Default::default()
        }
    }

    fn make_method(
        name: &str,
        params: Vec<al_symbols::ParameterSymbol>,
        return_type: Option<&str>,
    ) -> al_symbols::MethodSymbol {
        al_symbols::MethodSymbol {
            name: name.to_string(),
            parameters: params,
            return_type: return_type.map(|s| s.to_string()),
            attributes: Vec::new(),
            is_local: false,
        }
    }

    fn make_param(name: &str, type_name: &str, is_var: bool) -> al_symbols::ParameterSymbol {
        al_symbols::ParameterSymbol {
            name: name.to_string(),
            type_name: type_name.to_string(),
            is_var,
        }
    }

    #[test]
    #[ignore = "T1202 not yet implemented"]
    fn implement_interface_offered_for_codeunit_with_implements() {
        let ws = Workspace::new();

        // Add interface with two methods
        ws.symbols.add_entries(&[make_interface_entry(
            "IMyInterface",
            vec![
                make_method("DoSomething", vec![
                    make_param("Input", "Text", false),
                ], None),
                make_method("GetValue", vec![], Some("Integer")),
            ],
        )]);

        let al_code = r#"codeunit 50100 "My Codeunit" implements IMyInterface
{
}
"#;
        let uri = Url::parse("file:///test/Impl.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        // Cursor on line 0 (the codeunit declaration line)
        let range = Range {
            start: super::super::Position { line: 0, character: 10 },
            end: super::super::Position { line: 0, character: 10 },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(!impl_actions.is_empty(), "Should offer 'Implement interface' action");
        assert!(impl_actions[0].title.contains("IMyInterface"));

        let edit = impl_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;

        // Should generate stubs for both methods
        assert!(new_text.contains("procedure DoSomething"), "Should have DoSomething stub");
        assert!(new_text.contains("Input: Text"), "Should have parameter");
        assert!(new_text.contains("procedure GetValue"), "Should have GetValue stub");
        assert!(new_text.contains(": Integer"), "Should have return type");
        // Stubs should have TODO bodies
        assert!(new_text.contains("// TODO"), "Should have TODO placeholder");
    }

    #[test]
    #[ignore = "T1202 not yet implemented"]
    fn implement_interface_skips_already_implemented_methods() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_interface_entry(
            "IMyInterface",
            vec![
                make_method("DoSomething", vec![], None),
                make_method("GetValue", vec![], Some("Integer")),
            ],
        )]);

        // Codeunit already has DoSomething implemented
        let al_code = r#"codeunit 50100 "My Codeunit" implements IMyInterface
{
    procedure DoSomething()
    begin
        Message('Already done');
    end;
}
"#;
        let uri = Url::parse("file:///test/Impl.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 0, character: 10 },
            end: super::super::Position { line: 0, character: 10 },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(!impl_actions.is_empty(), "Should still offer action for remaining methods");

        let edit = impl_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;

        // Should NOT generate stub for DoSomething (already exists)
        assert!(!new_text.contains("procedure DoSomething"), "Should skip existing method");
        // Should generate stub for GetValue
        assert!(new_text.contains("procedure GetValue"), "Should generate missing method");
    }

    #[test]

    fn implement_interface_not_offered_when_all_methods_present() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_interface_entry(
            "IMyInterface",
            vec![
                make_method("DoSomething", vec![], None),
            ],
        )]);

        let al_code = r#"codeunit 50100 "My Codeunit" implements IMyInterface
{
    procedure DoSomething()
    begin
        Message('Done');
    end;
}
"#;
        let uri = Url::parse("file:///test/Impl.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 0, character: 10 },
            end: super::super::Position { line: 0, character: 10 },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(impl_actions.is_empty(), "Should NOT offer when all methods already exist");
    }

    #[test]

    fn implement_interface_not_offered_for_non_implementing_codeunit() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    begin
        Message('Hello');
    end;
}
"#;
        let uri = Url::parse("file:///test/NoImpl.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 0, character: 10 },
            end: super::super::Position { line: 0, character: 10 },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(impl_actions.is_empty(), "Should NOT offer for codeunits without implements");
    }

    #[test]
    #[ignore = "T1202 not yet implemented"]
    fn implement_interface_handles_var_parameters() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_interface_entry(
            "IProcessor",
            vec![
                make_method("Process", vec![
                    make_param("Input", "Text", false),
                    make_param("Output", "Text", true),
                ], Some("Boolean")),
            ],
        )]);

        let al_code = r#"codeunit 50100 "My Processor" implements IProcessor
{
}
"#;
        let uri = Url::parse("file:///test/Proc.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 0, character: 10 },
            end: super::super::Position { line: 0, character: 10 },
        };

        let actions = source_actions(&ws, &uri, range);
        let impl_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Implement interface"))
            .collect();

        assert!(!impl_actions.is_empty());

        let edit = impl_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;

        assert!(new_text.contains("var Output: Text"), "Should include var modifier");
        assert!(new_text.contains("Input: Text"), "Should include non-var param");
        assert!(new_text.contains(": Boolean"), "Should include return type");
    }

    #[test]
    fn with_elimination_not_offered_outside_with() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    begin
        Message('Hello');
    end;
}
"#;
        let uri = Url::parse("file:///test/NoWith.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 4, character: 8 },
            end: super::super::Position { line: 4, character: 8 },
        };

        let actions = source_actions(&ws, &uri, range);
        let with_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("with"))
            .collect();

        assert!(with_actions.is_empty(), "Should NOT offer on non-with code");
    }

    // -----------------------------------------------------------------------
    // Tests for make-method-local (T1208)
    // -----------------------------------------------------------------------

    #[test]
    fn make_local_offered_for_procedure() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure InternalHelper()
    begin
        Message('helper');
    end;
}
"#;
        let uri = Url::parse("file:///test/MakeLocal.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 2, character: 4 },
            end: super::super::Position { line: 2, character: 4 },
        };

        let actions = source_actions(&ws, &uri, range);
        let local_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("local"))
            .collect();

        assert!(!local_actions.is_empty(), "Should offer 'Make local' action");
        let edit = local_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        assert!(edits[0].new_text.contains("local"), "Should insert 'local'");
    }

    #[test]
    fn make_local_not_offered_when_already_local() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    local procedure InternalHelper()
    begin
        Message('helper');
    end;
}
"#;
        let uri = Url::parse("file:///test/MakeLocal.al").unwrap();
        ws.documents.open(uri.clone(), al_code.to_string());

        let range = Range {
            start: super::super::Position { line: 2, character: 10 },
            end: super::super::Position { line: 2, character: 10 },
        };

        let actions = source_actions(&ws, &uri, range);
        let local_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Make procedure local"))
            .collect();

        assert!(local_actions.is_empty(), "Should NOT offer when already local");
    }
}
