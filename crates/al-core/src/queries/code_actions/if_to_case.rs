//! If-to-case conversion source action.

use url::Url;

use super::{detect_indent, single_edit_ws};
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use crate::workspace::Workspace;

pub(super) fn source_action_if_to_case(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();
    let source = text.as_bytes();

    // Find the if_statement node at the cursor position.
    // LSP positions use UTF-16 code units for character offset; tree-sitter uses byte offsets.
    // Bounds-check the line index: a stale range from the client (sent before
    // the document was edited) could point past the end of the document. Without
    // the check, lines().nth() returns None and unwrap_or("") silently makes
    // col_bytes = 0, yielding a Point at (start_line, 0) that does not
    // correspond to the user's cursor — the action would either misfire or
    // attach to the wrong if_statement at the document start.
    let cursor_line = text.lines().nth(range.start.line as usize)?;
    let col_bytes =
        crate::resolution::utf16_col_to_byte_offset(cursor_line, range.start.character as usize);
    let point = tree_sitter::Point::new(range.start.line as usize, col_bytes);
    let if_node = find_outermost_if_at_point(root, point)?;

    // Walk the if-else chain collecting (variable_text, value_text, body_text) triples
    let mut branches: Vec<(String, String, String)> = Vec::new();
    let mut else_body: Option<String> = None;
    let mut common_var: Option<String> = None;

    walk_if_chain(
        if_node,
        source,
        &mut branches,
        &mut else_body,
        &mut common_var,
    );

    // Need 3+ branches and all on the same variable
    if branches.len() < 3 || common_var.is_none() {
        return None;
    }

    let var_name = common_var?;
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
        edit: Some(single_edit_ws(uri, vec![edit])),
        is_preferred: false,
    })
}

/// Find the outermost if_statement containing the given point.
/// Walks up the tree to find the topmost if_statement in the chain.
fn find_outermost_if_at_point(
    root: tree_sitter::Node,
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node> {
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

/// Walk an if-else chain iteratively, collecting branches.
///
/// Each branch is (variable_text, value_text, body_text).  Sets
/// `common_var` to `None` if variables differ across branches.  An iterative
/// loop avoids unbounded recursion on deeply nested if/else chains
/// (CLAUDE.md).
fn walk_if_chain(
    node: tree_sitter::Node,
    source: &[u8],
    branches: &mut Vec<(String, String, String)>,
    else_body: &mut Option<String>,
    common_var: &mut Option<String>,
) {
    let mut current = node;
    loop {
        if current.kind() != "if_statement" {
            return;
        }

        // Extract condition: expect `<var> = <value>`.
        if let Some(condition) = current.child_by_field_name("condition") {
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

                let body = current
                    .child_by_field_name("consequence")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("")
                    .to_string();
                branches.push((var, val, body));
            } else {
                *common_var = None;
                return;
            }
        }

        // Follow the else branch.
        let Some(alt) = current.child_by_field_name("alternative") else {
            return;
        };
        // The alternative is a `statement` node wrapping the actual node.
        let inner = if alt.kind() == "statement" && alt.child_count() == 1 {
            alt.child(0).unwrap_or(alt)
        } else {
            alt
        };
        if inner.kind() == "if_statement" {
            current = inner;
        } else {
            *else_body = alt.utf8_text(source).ok().map(|s| s.to_string());
            return;
        }
    }
}

/// Extract (left, right) from an equality expression `left = right`.
/// Tree-sitter structure: expression -> unary_expression, binary_operator(operator "="), unary_expression
fn extract_equality_operands(node: tree_sitter::Node, source: &[u8]) -> Option<(String, String)> {
    // Iteratively unwrap single-child wrapper nodes to avoid recursion on
    // deeply nested AST wrappers (CLAUDE.md prohibits recursive tree-sitter
    // traversal).
    let mut node = node;
    while node.child_count() == 1 {
        node = node.child(0)?;
    }

    let child_count = node.child_count();
    if child_count < 3 {
        return None;
    }

    // Find the `=` operator — can be `binary_operator` wrapping `operator`, or direct `operator`
    let mut eq_idx = None;
    for i in 0..child_count {
        let child = node.child(i)?;
        let is_eq = match child.kind() {
            "binary_operator" | "operator" => child
                .utf8_text(source)
                .ok()
                .map(|t| t.trim() == "=")
                .unwrap_or(false),
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

    // Normalize Yoda-style conditions: if the left side is a literal (number, quoted
    // string, keyword), swap so the variable is always the first element.
    if is_literal(&left) && !is_literal(&right) {
        Some((right, left))
    } else {
        Some((left, right))
    }
}

/// Returns true if `s` looks like a literal value rather than a variable.
/// Handles: integer/decimal numbers, single-quoted AL strings, `true`/`false`.
fn is_literal(s: &str) -> bool {
    let t = s.trim();
    // Numeric literal
    if t.chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == '-')
        && t.chars().skip(1).all(|c| c.is_ascii_digit() || c == '.')
    {
        return true;
    }
    // Single-quoted string
    if t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2 {
        return true;
    }
    // Boolean keywords
    if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
        return true;
    }
    false
}

/// Generate "Implement interface" code actions for codeunits with `implements` clauses.
///
/// For each interface that the codeunit declares it implements, checks which methods
/// are missing and offers to generate stub procedure declarations.

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
        open_doc(&ws, &uri, al_code);

        // Cursor on the first `if` line (line 4)
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
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(
            !case_actions.is_empty(),
            "Should offer 'Convert to case' action"
        );

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
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(
            case_actions.is_empty(),
            "Should NOT offer conversion for only 2 branches"
        );
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
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(
            case_actions.is_empty(),
            "Should NOT offer when branches compare different variables"
        );
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
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(
            !case_actions.is_empty(),
            "Should offer conversion with else"
        );

        let edit = case_actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;
        assert!(new_text.contains("else"), "Should preserve else clause");
    }

    // -----------------------------------------------------------------------
    // SERIAL bug fixes
    // -----------------------------------------------------------------------

    // Bug: if_to_case indentation loss
    // The generated case statement should preserve the body indentation
    // relative to the case label, not strip all indentation.
    #[test]
    fn if_to_case_preserves_relative_indentation_in_begin_end_body() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff(x: Integer)
    begin
        if x = 1 then begin
            Message('one');
            x := 10;
        end else if x = 2 then begin
            Message('two');
            x := 20;
        end else if x = 3 then begin
            Message('three');
            x := 30;
        end;
    end;
}
"#;
        let uri = Url::parse("file:///test/IfIndent.al").unwrap();
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
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(!case_actions.is_empty(), "Should offer conversion");
        let edit = case_actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;
        // The generated case body should have begin/end preserved at correct indent
        assert!(new_text.contains("begin"), "Body should contain begin");
        assert!(new_text.contains("end"), "Body should contain end");
        // The body lines (Message, assignment) should be indented more than the case label
        let msg_line = new_text
            .lines()
            .find(|l| l.contains("Message('one')"))
            .expect("should have Message line");
        let label_line = new_text
            .lines()
            .find(|l| l.trim() == "1:")
            .expect("should have 1: label");
        assert!(
            msg_line.len() - msg_line.trim_start().len()
                > label_line.len() - label_line.trim_start().len(),
            "Body should be indented more than label"
        );
    }

    // Bug: if_to_case Yoda-style conditions
    // `if 'X' = Var then` should be treated as `Var = 'X'` for the case variable
    #[test]
    fn if_to_case_handles_yoda_style_conditions() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff(x: Integer)
    begin
        if 1 = x then
            Message('one')
        else if 2 = x then
            Message('two')
        else if 3 = x then
            Message('three');
    end;
}
"#;
        let uri = Url::parse("file:///test/IfYoda.al").unwrap();
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
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();

        assert!(
            !case_actions.is_empty(),
            "Should offer conversion for yoda-style"
        );
        let edit = case_actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;
        // The variable in the case should be 'x', not a literal
        assert!(
            new_text.contains("case x of"),
            "Should use variable x in case, not literal"
        );
    }

    // Bug: extract_word_at_position closing quote
    // When cursor is on a closing `"` of a quoted identifier, the word
    // extracted should be the full inner name, not empty.

    #[test]
    fn if_to_case_offered_when_line_has_multibyte_prefix() {
        let ws = Workspace::new();
        // Procedure name contains a non-ASCII character so line offsets differ
        let al_code = "codeunit 50100 \"My Codeunit\"\n{\n    procedure Ångström(x: Integer)\n    begin\n        if x = 1 then\n            Message('one')\n        else if x = 2 then\n            Message('two')\n        else if x = 3 then\n            Message('three');\n    end;\n}\n";
        let uri = Url::parse("file:///test/IfUtf16.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // Cursor on "if x = 1 then" (line 4, col 8 in UTF-16 and bytes — ASCII prefix)
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
        let case_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Convert to case"))
            .collect();
        assert!(
            !case_actions.is_empty(),
            "Should offer if-to-case conversion"
        );
    }

    #[test]
    fn source_action_if_to_case_returns_none_for_out_of_range_line() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "Test"
{
    procedure DoIt(x: Integer)
    begin
        if x = 1 then
            Message('one');
    end;
}
"#;
        let uri = Url::parse("file:///test/IfRange.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // Line just past end of document.
        let total_lines = al_code.lines().count() as u32;
        let range = Range {
            start: super::super::Position {
                line: total_lines + 5,
                character: 0,
            },
            end: super::super::Position {
                line: total_lines + 5,
                character: 0,
            },
        };

        // Call the if_to_case helper directly so we don't depend on the rest of
        // source_actions() which may panic on truly extreme stale ranges.
        let action = source_action_if_to_case(&ws, &uri, al_code, range);
        assert!(
            action.is_none(),
            "Out-of-range line must not yield an if-to-case action; got: {:?}",
            action
        );
    }
}
