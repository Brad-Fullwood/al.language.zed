//! With-statement elimination source action.

use url::Url;

use super::{detect_indent, single_edit_ws};
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use crate::workspace::Workspace;

fn resolve_with_field_names(
    workspace: &Workspace,
    tree: &tree_sitter::Tree,
    text: &str,
    record_var: &str,
) -> Vec<String> {
    // Walk the entire file to find a var declaration for `record_var`.
    let source = text.as_bytes();
    let table_name = find_record_type_for_var(tree.root_node(), source, record_var);
    let table_name = match table_name {
        Some(t) => t,
        None => return Vec::new(),
    };

    // Look up the table in the symbol index and collect field names.
    let entries = workspace.symbols.get_by_name(&table_name);
    for entry in &entries {
        if (entry.kind == crate::symbols::ObjectKind::Table
            || entry.kind == crate::symbols::ObjectKind::TableExtension)
            && !entry.fields.is_empty()
        {
            return entry.fields.iter().map(|f| f.name.clone()).collect();
        }
    }

    Vec::new()
}

/// Walk an AST to find the Record type name for a given variable name.
///
/// Searches `var_section` and `parameter_list` nodes across the whole file.
fn find_record_type_for_var(
    root: tree_sitter::Node,
    source: &[u8],
    var_name: &str,
) -> Option<String> {
    let var_lower = var_name.to_lowercase();
    find_record_type_recursive(root, source, &var_lower)
}

fn find_record_type_recursive(
    root: tree_sitter::Node,
    source: &[u8],
    var_lower: &str,
) -> Option<String> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();

        if kind == "regular_variable_declaration" {
            // Check if this is the variable we're looking for.
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let name_clean = name_text.trim_matches('"').trim();
                    if name_clean.to_lowercase() == *var_lower {
                        // Extract Record type
                        if let Some(type_node) = node.child_by_field_name("type") {
                            return extract_record_subtype(type_node, source);
                        }
                    }
                }
            }
        } else if kind == "parameter" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let name_clean = name_text.trim_matches('"').trim();
                    if name_clean.to_lowercase() == *var_lower {
                        if let Some(type_node) = node.child_by_field_name("type") {
                            return extract_record_subtype(type_node, source);
                        }
                    }
                }
            }
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }

    None
}

/// Extract the table name from a `type_reference` node for `Record "TableName"`.
fn extract_record_subtype(type_node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut found_record_kw = false;
    let mut cursor = type_node.walk();
    for child in type_node.children(&mut cursor) {
        let kind = child.kind();
        if !found_record_kw {
            if kind.starts_with("kw_") || kind == "identifier" || kind == "name_or_keyword" {
                if let Ok(text) = child.utf8_text(source) {
                    if text.trim().to_lowercase() == "record" {
                        found_record_kw = true;
                    }
                }
            }
        } else {
            // Next token is the table name (possibly quoted)
            if let Ok(text) = child.utf8_text(source) {
                let trimmed = text.trim().trim_matches('"').trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

/// Eliminate a `with` statement by qualifying field references with the record variable.
/// Detects `with X do begin...end` and replaces with the body, prefixing unqualified
/// identifiers with `X.` (AA0205 compliance).
pub(super) fn source_action_eliminate_with(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();
    let source = text.as_bytes();

    // Find with_statement at cursor.
    // LSP positions use UTF-16 code units; tree-sitter uses byte offsets.
    let cursor_line = text.lines().nth(range.start.line as usize)?;
    let col_bytes =
        crate::resolution::utf16_col_to_byte_offset(cursor_line, range.start.character as usize);
    let point = tree_sitter::Point::new(range.start.line as usize, col_bytes);
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

    // Try to resolve field names for the record variable so the field-name pass can
    // qualify occurrences inside `if`, `filter(...)`, etc. (issue #33).
    let field_names = resolve_with_field_names(workspace, &tree, text, &record_var);

    // Qualify unqualified identifiers in the body with `record_var.`
    let qualified_body = qualify_with_references(&body_text, &record_var, &indent, &field_names);

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
        edit: Some(single_edit_ws(uri, vec![edit])),
        is_preferred: false,
    })
}

/// Find a with_statement node at the given point.
fn find_with_at_point(
    root: tree_sitter::Node,
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node> {
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
///
/// Two-pass approach:
/// 1. Structural: if a line starts with an unqualified identifier followed by `:=` or `(`,
///    prepend `record_var.` to the whole line.
/// 2. Field-name: for each known field name, substitute unqualified occurrences with
///    `record_var.field` anywhere in the line (word-boundary, not already qualified).
fn qualify_with_references(
    body: &str,
    record_var: &str,
    indent: &str,
    field_names: &[String],
) -> String {
    let mut result = String::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            result.push_str(&format!("{}{}\n", indent, trimmed));
            continue;
        }

        // Pass 1+2: structural qualification then field-name substitution
        let qualified_line = qualify_line(trimmed, record_var, field_names);
        result.push_str(&format!("{}{}\n", indent, qualified_line));
    }

    result
}

/// Qualify a single line's identifiers with the record variable.
///
/// Two-pass approach:
/// 1. **Structural pass**: if the line starts with an unqualified identifier followed by
///    `:=` or `(`, prepend `record_var.` to the whole line.  Keywords like `if`, `begin`,
///    `end`, etc. are detected with word-boundary matching so field names that start with
///    a keyword prefix (e.g. `EndDate`, `FormatText`) are still qualified correctly.
/// 2. **Field-name pass**: for each known field name, substitute unqualified occurrences
///    anywhere in the line with `record_var.field` (word-boundary, not already preceded
///    by `.` or `:`).  This handles `if Field > 0`, `filter(Field = ...)`, etc.
fn qualify_line(line: &str, record_var: &str, field_names: &[String]) -> String {
    let trimmed = line.trim();

    // --- Pass 1: structural qualification ---

    // Skip lines that start with AL keywords (whole-word match).
    // Does NOT skip field names that begin with a keyword prefix (e.g. EndDate, IfFlag).
    // Keywords are looked up via LanguageData (loaded from tree-sitter-al/data/keywords.json).
    // "//" (line comment) and "end;" are not grammar keywords but must also be skipped here.
    let lower = trimmed.to_lowercase();
    let starts_with_keyword = {
        // Check LanguageData keywords first (covers if/then/else/begin/end/for/while/etc.)
        let keyword_match = lower
            .split_once(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|(word, _)| crate::syntax::language_data::is_keyword(word))
            .unwrap_or_else(|| crate::syntax::language_data::is_keyword(&lower));
        // Also skip "//" (line comment start) and "end;" (not a grammar keyword but structural)
        let special_match = lower.starts_with("//") || lower.starts_with("end;");
        keyword_match || special_match
    };

    let structurally_qualified = if starts_with_keyword {
        // Cannot structurally qualify a keyword-led line — fall through to field-name pass.
        trimmed.to_string()
    } else if let Some(after_open) = trimmed.strip_prefix('"') {
        // Quoted identifier at line start
        if let Some(end_quote) = after_open.find('"') {
            let after = after_open[end_quote + 1..].trim_start();
            if after.starts_with(":=") || after.starts_with('(') {
                format!("{}.{}", record_var, trimmed)
            } else {
                trimmed.to_string()
            }
        } else {
            trimmed.to_string()
        }
    } else {
        // Plain identifier at line start
        let first_word_end = trimmed
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(trimmed.len());
        let first_word = &trimmed[..first_word_end];
        if first_word.is_empty() {
            trimmed.to_string()
        } else {
            let after_word = trimmed[first_word_end..].trim_start();
            if after_word.starts_with('.') || after_word.starts_with("::") {
                // Already qualified — leave as-is
                trimmed.to_string()
            } else if after_word.starts_with(":=") || after_word.starts_with('(') {
                format!("{}.{}", record_var, trimmed)
            } else {
                trimmed.to_string()
            }
        }
    };

    // --- Pass 2: field-name substitution ---
    // For each known field name, replace unqualified occurrences in the line with
    // `record_var.field`.  Skip occurrences already preceded by `.` or `:`.
    if field_names.is_empty() {
        return structurally_qualified;
    }

    substitute_field_names(&structurally_qualified, record_var, field_names)
}

/// Replace all unqualified occurrences of known field names in `line` with
/// `record_var.field_name`.  An occurrence is unqualified when the character
/// immediately before it is NOT `.` or `:`.
///
/// Matching is case-insensitive; the replacement uses the original field name
/// casing from `field_names`.
fn substitute_field_names(line: &str, record_var: &str, field_names: &[String]) -> String {
    let mut result = line.to_string();

    // Sort longest-first to avoid shorter names shadowing longer ones.
    let mut sorted: Vec<&String> = field_names.iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.len()));

    for field in sorted {
        if field.is_empty() {
            continue;
        }
        let field_lower = field.to_lowercase();
        let qualified = format!("{}.{}", record_var, field);

        // Walk through the string finding word-boundary matches.
        let mut output = String::with_capacity(result.len() + qualified.len());
        let bytes = result.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            // Try to match field_lower at position i (case-insensitive).
            let remaining = &result[i..];
            let remaining_lower = remaining.to_lowercase();
            if remaining_lower.starts_with(field_lower.as_str()) {
                let match_end = i + field.len();
                // Word-boundary check: char before must not be `.`, `:`, alphanumeric, or `_`.
                let prev_ok = if i == 0 {
                    true
                } else {
                    // Get previous char
                    let prev_char = result[..i].chars().next_back().unwrap_or(' ');
                    prev_char != '.'
                        && prev_char != ':'
                        && !prev_char.is_alphanumeric()
                        && prev_char != '_'
                };
                // Word-boundary check: char after must not be alphanumeric or `_`.
                let next_ok = if match_end >= result.len() {
                    true
                } else {
                    let next_char = result[match_end..].chars().next().unwrap_or(' ');
                    !next_char.is_alphanumeric() && next_char != '_'
                };

                if prev_ok && next_ok {
                    output.push_str(&qualified);
                    i = match_end;
                    continue;
                }
            }
            // Push one character and advance.
            let Some(ch) = result[i..].chars().next() else {
                break;
            };
            output.push(ch);
            i += ch.len_utf8();
        }

        result = output;
    }

    result
}

/// Parse the file's namespace and using directives.
/// Returns (list of imported namespaces, line number to insert new using directives).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::code_actions::source_actions;
    use crate::workspace::Workspace;
    use url::Url;

    fn open_doc(ws: &crate::workspace::Workspace, uri: &url::Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string());
    }

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
        open_doc(&ws, &uri, al_code);

        // Cursor on the `with` line (line 6)
        let range = Range {
            start: super::super::Position {
                line: 6,
                character: 8,
            },
            end: super::super::Position {
                line: 6,
                character: 8,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let with_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("with"))
            .collect();

        assert!(
            !with_actions.is_empty(),
            "Should offer 'Eliminate with' action"
        );

        let edit = with_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;
        // Should qualify field references with Cust.
        assert!(
            new_text.contains("Cust.Name"),
            "Should qualify Name with Cust"
        );
        assert!(
            new_text.contains("Cust.\"No.\""),
            "Should qualify \"No.\" with Cust"
        );
        // Should NOT have with...do wrapper
        assert!(
            !new_text.contains("with Cust do"),
            "Should remove with wrapper"
        );
    }

    #[test]
    fn qualify_line_does_not_skip_keyword_prefixed_identifiers() {
        // Field names that start with AL keywords must still be qualified.
        // e.g. EndDate, FormatText, CaseNo must not be silently dropped by
        // the whole-word keyword guard.
        assert!(
            qualify_line("EndDate := Today;", "Rec", &[]).starts_with("Rec."),
            "EndDate starts with 'end' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("FormatText := 'X';", "Rec", &[]).starts_with("Rec."),
            "FormatText starts with 'for' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("CaseNo := 1;", "Rec", &[]).starts_with("Rec."),
            "CaseNo starts with 'case' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("IfFlag := true;", "Rec", &[]).starts_with("Rec."),
            "IfFlag starts with 'if' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("MessageText := '';", "Rec", &[]).starts_with("Rec."),
            "MessageText starts with 'message' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("ExitCode := 0;", "Rec", &[]).starts_with("Rec."),
            "ExitCode starts with 'exit' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("ErrorText := '';", "Rec", &[]).starts_with("Rec."),
            "ErrorText starts with 'error' but is not a keyword — must be qualified"
        );
    }

    #[test]
    fn qualify_line_skips_al_keywords_exactly() {
        // Exact keyword lines must not be qualified.
        assert_eq!(qualify_line("end;", "Rec", &[]), "end;");
        assert_eq!(qualify_line("begin", "Rec", &[]), "begin");
        assert_eq!(qualify_line("end", "Rec", &[]), "end");
        assert!(qualify_line("if x = 1 then", "Rec", &[]).starts_with("if"));
        assert!(qualify_line("for i := 1 to 10 do", "Rec", &[]).starts_with("for"));
        assert!(qualify_line("while x > 0 do", "Rec", &[]).starts_with("while"));
        assert!(qualify_line("case x of", "Rec", &[]).starts_with("case"));
    }

    #[test]
    fn qualify_line_skips_already_qualified_references() {
        // Rec.Field := X should not become Rec2.Rec.Field := X
        assert_eq!(
            qualify_line("Rec.Name := 'X';", "Rec2", &[]),
            "Rec.Name := 'X';"
        );
        assert_eq!(
            qualify_line("Customer.\"No.\" := '100';", "Rec", &[]),
            "Customer.\"No.\" := '100';"
        );
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
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 4,
                character: 8,
            },
            end: super::super::Position {
                line: 4,
                character: 8,
            },
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
}
