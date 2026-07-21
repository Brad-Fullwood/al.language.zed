//! SQL anti-pattern detection.
//!
//! Detects: FindFirst in loops (N+1), FindSet without filters, Get in loops, CalcFields in loops.

use al_workspace::Workspace;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SqlAntiPattern {
    FindInLoop,
    FindSetWithoutFilters,
    GetInLoop,
    CalcFieldsInLoop,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlPatternViolation {
    pub kind: SqlAntiPattern,
    pub message: String,
    pub object: String,
    pub procedure: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub line: u32,
}

pub fn detect_sql_patterns(workspace: &Workspace) -> Vec<SqlPatternViolation> {
    let mut violations = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key();
        let file_path = path.to_string_lossy().to_string();
        let Some((text, parsed_tree)) = workspace.file_index.get_cached_parse(path) else {
            continue;
        };

        let Some(obj_info) = al_syntax::find_object_declaration(&parsed_tree, &text) else {
            continue;
        };

        scan_file_for_sql_patterns(
            &file_path,
            &text,
            &parsed_tree,
            &obj_info.name,
            &mut violations,
        );
    }

    violations
}

fn scan_file_for_sql_patterns(
    file_path: &str,
    text: &str,
    tree: &tree_sitter::Tree,
    object_name: &str,
    violations: &mut Vec<SqlPatternViolation>,
) {
    let root = tree.root_node();
    let source = text.as_bytes();
    scan_procedures(root, source, file_path, object_name, violations);
}

fn scan_procedures(
    root: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    object_name: &str,
    violations: &mut Vec<SqlPatternViolation>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            let proc_name = al_syntax::node_name_or(node, source, "(unknown)");

            if let Ok(proc_text) = node.utf8_text(source) {
                let start_line = node.start_position().row as u32 + 1;
                analyze_proc_text(
                    proc_text,
                    start_line,
                    file_path,
                    object_name,
                    &proc_name,
                    violations,
                );
            }
            // Do not recurse into procedure body
            continue;
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn analyze_proc_text(
    proc_text: &str,
    start_line: u32,
    file_path: &str,
    object_name: &str,
    proc_name: &str,
    violations: &mut Vec<SqlPatternViolation>,
) {
    // fix: track a per-loop begin..end nesting depth to prevent an
    // inner "end;" from prematurely decrementing the loop counter.
    //
    // `loop_begin_depth` is a stack — one entry per active loop level.
    // Each entry counts the number of nested begin..end blocks currently open
    // inside that loop.  An `end;` only pops the loop itself when the top entry
    // reaches 0; otherwise it closes a nested block.
    let mut loop_begin_depth: Vec<u32> = Vec::new();
    let mut has_filter_before_findset = false;

    for (offset, line) in proc_text.lines().enumerate() {
        let line_num = start_line + offset as u32;
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let cleaned = strip_string_literals(line);
        let lower = cleaned.trim().to_lowercase();

        if is_loop_start(&lower) {
            let opens_body =
                lower.ends_with(" begin") || lower.ends_with("\tbegin") || lower == "begin";
            loop_begin_depth.push(if opens_body { 1 } else { 0 });
        } else if lower == "begin" {
            if let Some(top) = loop_begin_depth.last_mut() {
                *top += 1;
            }
        } else if lower == "end;" || lower == "end" {
            if let Some(top) = loop_begin_depth.last_mut() {
                if *top > 0 {
                    *top -= 1;
                } else {
                    loop_begin_depth.pop();
                }
            }
        } else if lower.starts_with("until ") {
            loop_begin_depth.pop();
        }

        let in_loop = !loop_begin_depth.is_empty();

        if lower.contains(".setrange(") || lower.contains(".setfilter(") {
            has_filter_before_findset = true;
        }

        if in_loop {
            if lower.contains(".findfirst()") || lower.contains(".findlast()") {
                violations.push(make_violation(
                    SqlAntiPattern::FindInLoop,
                    "FindFirst/FindLast inside loop causes N+1 queries",
                    object_name,
                    proc_name,
                    file_path,
                    line_num,
                ));
            }
            if contains_get_call(&lower) {
                violations.push(make_violation(
                    SqlAntiPattern::GetInLoop,
                    "Get() inside loop causes N+1 queries",
                    object_name,
                    proc_name,
                    file_path,
                    line_num,
                ));
            }
            if lower.contains(".calcfields(") {
                violations.push(make_violation(
                    SqlAntiPattern::CalcFieldsInLoop,
                    "CalcFields() in loop is expensive — use SetAutoCalcFields() instead",
                    object_name,
                    proc_name,
                    file_path,
                    line_num,
                ));
            }
        }

        if lower.contains(".findset(") || lower.contains(".findset ()") {
            if !has_filter_before_findset {
                violations.push(make_violation(
                    SqlAntiPattern::FindSetWithoutFilters,
                    "FindSet() without filters causes full table scan",
                    object_name,
                    proc_name,
                    file_path,
                    line_num,
                ));
            }
            has_filter_before_findset = false;
        }
    }
}

fn contains_get_call(lower: &str) -> bool {
    // Match .Get( but not .GetValue(, .GetResult( etc.
    if let Some(pos) = lower.find(".get(") {
        let before = &lower[..pos];
        let last_word: &str = before
            .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("");
        !last_word.is_empty()
    } else {
        false
    }
}

/// Replace AL string literal contents (`'...'` and `"..."`) with spaces so
/// that text-pattern matchers do not see method-call-like substrings buried
/// inside literals (e.g. `if x = 'FindFirst()' then ...`). Quotes themselves
/// are preserved so token shape is unchanged for downstream loop/begin/end
/// detection.
fn strip_string_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_single = false;
    let mut in_double = false;
    for ch in line.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(ch);
            }
            _ if in_single || in_double => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

fn is_loop_start(lower: &str) -> bool {
    lower.starts_with("for ")
        || lower.starts_with("foreach ")
        || lower.starts_with("while ")
        || lower == "repeat"
        || lower.starts_with("repeat ")
}

fn make_violation(
    kind: SqlAntiPattern,
    message: &str,
    object: &str,
    procedure: &str,
    file: &str,
    line: u32,
) -> SqlPatternViolation {
    SqlPatternViolation {
        kind,
        message: message.to_string(),
        object: object.to_string(),
        procedure: procedure.to_string(),
        file: Some(file.to_string()),
        line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    fn detects_findfirst_in_loop() {
        let ws = workspace_with(vec![(
            "/src/Bad.al",
            r#"codeunit 50100 "Bad CU"
{
    procedure ProcessItems()
    var
        Item: Record Item;
        Line: Record "Sales Line";
    begin
        repeat
            if Item.FindFirst() then
                Message(Item."No.");
        until Line.Next() = 0;
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws);
        assert!(
            v.iter().any(|x| x.kind == SqlAntiPattern::FindInLoop),
            "FindFirst in loop: {:?}",
            v
        );
    }

    #[test]
    fn detects_findset_without_filters() {
        let ws = workspace_with(vec![(
            "/src/Scan.al",
            r#"codeunit 50100 "Scanner"
{
    procedure ScanAll()
    var
        Customer: Record Customer;
    begin
        if Customer.FindSet() then
            repeat
                Message(Customer.Name);
            until Customer.Next() = 0;
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws);
        assert!(
            v.iter()
                .any(|x| x.kind == SqlAntiPattern::FindSetWithoutFilters),
            "FindSet no filter: {:?}",
            v
        );
    }

    #[test]
    fn filtered_findset_no_violation() {
        let ws = workspace_with(vec![(
            "/src/Good.al",
            r#"codeunit 50100 "Good CU"
{
    procedure ProcessActive()
    var
        Customer: Record Customer;
    begin
        Customer.SetRange(Blocked, false);
        if Customer.FindSet() then
            repeat
                Message(Customer.Name);
            until Customer.Next() = 0;
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws);
        let filt: Vec<_> = v
            .iter()
            .filter(|x| x.kind == SqlAntiPattern::FindSetWithoutFilters)
            .collect();
        assert!(filt.is_empty(), "Filtered FindSet no warn: {:?}", filt);
    }

    #[test]
    fn empty_workspace_no_violations() {
        assert!(detect_sql_patterns(&Workspace::new()).is_empty());
    }

    /// A FindFirst() call after a nested if..begin..end inside a for loop
    /// must still be flagged.  Previously the inner "end;" prematurely zeroed
    /// `in_loop`, making code after the nested block invisible to the detector.
    #[test]
    fn detects_findfirst_after_nested_begin_end_in_loop() {
        let ws = workspace_with(vec![(
            "/src/Nested.al",
            r#"codeunit 50100 "Nested CU"
{
    procedure ProcessItems()
    var
        SalesLine: Record "Sales Line";
        Item: Record Item;
    begin
        for SalesLine.Next() = 1 to 10 do begin
            if SalesLine.Quantity > 0 then begin
                Message('Positive');
            end;
            if Item.FindFirst() then
                Message(Item."No.");
        end;
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws);
        assert!(
            v.iter().any(|x| x.kind == SqlAntiPattern::FindInLoop),
            "FindFirst after nested begin..end inside loop should be flagged: {:?}",
            v
        );
    }

    /// Regression: a FindFirst() substring buried inside
    /// a string literal or a // comment must not trigger a false-positive
    /// FindInLoop violation. The text scanner now strips literal contents
    /// and skips comment lines.
    #[test]
    fn no_false_positive_for_findfirst_in_string_literal_or_comment() {
        let ws = workspace_with(vec![(
            "/src/Strings.al",
            r#"codeunit 50100 "Strings CU"
{
    procedure DoIt()
    var
        i: Integer;
        msg: Text;
    begin
        for i := 1 to 10 do begin
            msg := 'FindFirst() should not match';
            // .FindFirst() in a comment should not match either
            Message(msg);
        end;
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws);
        assert!(
            !v.iter().any(|x| x.kind == SqlAntiPattern::FindInLoop),
            "FindFirst inside string literal/comment must not be flagged. Got: {:?}",
            v
        );
    }
}
