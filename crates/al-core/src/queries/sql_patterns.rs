//! SQL anti-pattern detection (T1806).
//!
//! Detects: FindFirst in loops (N+1), FindSet without filters, Get in loops, CalcFields in loops.

use serde::Serialize;
use al_syntax::AlParser;
use crate::workspace::Workspace;

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
        let file_path = entry.key().to_string_lossy().to_string();
        let text = entry.value();
        let parsed = AlParser::parse_quick(text);

        let Some(obj_info) = al_syntax::find_object_declaration(&parsed.tree, text) else {
            continue;
        };

        scan_file_for_sql_patterns(&file_path, text, &parsed.tree, &obj_info.name, &mut violations);
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
    node: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    object_name: &str,
    violations: &mut Vec<SqlPatternViolation>,
) {
    if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
        let proc_name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("(unknown)")
            .trim_matches('"')
            .to_string();

        if let Ok(proc_text) = node.utf8_text(source) {
            let start_line = node.start_position().row as u32 + 1;
            analyze_proc_text(&proc_text, start_line, file_path, object_name, &proc_name, violations);
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_procedures(child, source, file_path, object_name, violations);
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
    let mut in_loop: i32 = 0;
    let mut has_filter_before_findset = false;

    for (offset, line) in proc_text.lines().enumerate() {
        let line_num = start_line + offset as u32;
        let lower = line.trim().to_lowercase();

        if is_loop_start(&lower) { in_loop += 1; }
        if is_loop_end(&lower) && in_loop > 0 { in_loop -= 1; }

        if lower.contains(".setrange(") || lower.contains(".setfilter(") {
            has_filter_before_findset = true;
        }

        if in_loop > 0 {
            if lower.contains(".findfirst()") || lower.contains(".findlast()") {
                violations.push(make_violation(
                    SqlAntiPattern::FindInLoop,
                    "FindFirst/FindLast inside loop causes N+1 queries",
                    object_name, proc_name, file_path, line_num,
                ));
            }
            if contains_get_call(&lower) {
                violations.push(make_violation(
                    SqlAntiPattern::GetInLoop,
                    "Get() inside loop causes N+1 queries",
                    object_name, proc_name, file_path, line_num,
                ));
            }
            if lower.contains(".calcfields(") {
                violations.push(make_violation(
                    SqlAntiPattern::CalcFieldsInLoop,
                    "CalcFields() in loop is expensive — use SetAutoCalcFields() instead",
                    object_name, proc_name, file_path, line_num,
                ));
            }
        }

        if lower.contains(".findset(") || lower.contains(".findset ()") {
            if !has_filter_before_findset {
                violations.push(make_violation(
                    SqlAntiPattern::FindSetWithoutFilters,
                    "FindSet() without filters causes full table scan",
                    object_name, proc_name, file_path, line_num,
                ));
            }
            has_filter_before_findset = false;
        }
    }
}

fn contains_get_call(lower: &str) -> bool {
    // Match .Get( but not getters like .GetValue( or .GetResult(
    if let Some(pos) = lower.find(".get(") {
        // Make sure it's a standalone .Get(
        let before = &lower[..pos];
        let last_word: &str = before.rsplit(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("");
        // If last word before .get is an identifier, it's a record .Get() call
        !last_word.is_empty()
    } else {
        false
    }
}

fn is_loop_start(lower: &str) -> bool {
    lower.starts_with("for ") || lower.starts_with("foreach ") ||
    lower.starts_with("while ") || lower == "repeat" || lower.starts_with("repeat ")
}

fn is_loop_end(lower: &str) -> bool {
    lower == "end;" || lower == "end" || lower.starts_with("until ")
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
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index.add_file(PathBuf::from(name), content.to_string());
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
        assert!(v.iter().any(|x| x.kind == SqlAntiPattern::FindInLoop), "FindFirst in loop: {:?}", v);
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
        assert!(v.iter().any(|x| x.kind == SqlAntiPattern::FindSetWithoutFilters), "FindSet no filter: {:?}", v);
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
        let filt: Vec<_> = v.iter().filter(|x| x.kind == SqlAntiPattern::FindSetWithoutFilters).collect();
        assert!(filt.is_empty(), "Filtered FindSet no warn: {:?}", filt);
    }

    #[test]
    fn empty_workspace_no_violations() {
        assert!(detect_sql_patterns(&Workspace::new()).is_empty());
    }
}
