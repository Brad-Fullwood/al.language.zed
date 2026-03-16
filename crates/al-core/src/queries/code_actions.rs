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

    actions
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
