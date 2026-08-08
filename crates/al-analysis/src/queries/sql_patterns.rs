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

pub fn detect_sql_patterns(
    workspace: &Workspace,
) -> Result<Vec<SqlPatternViolation>, super::WorkspaceQueryError> {
    let sources = crate::workspace_sources::snapshot(workspace)?;
    let mut violations = Vec::new();

    for source in sources {
        scan_file_for_sql_patterns(
            &source.path.to_string_lossy(),
            &source.text,
            &source.tree,
            &source.object.info.name,
            &mut violations,
        );
    }

    Ok(violations)
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

/// One entry of the block-nesting stack used to decide whether a line sits
/// inside a loop.
#[derive(Debug, PartialEq, Eq)]
enum Frame {
    /// An open `begin … end` (or `repeat … until`). `is_loop_body` marks the
    /// block as the body of a `for`/`foreach`/`while`/`repeat`.
    Block { is_loop_body: bool },
    /// A `for … do` / `while … do` whose body is a *single* statement rather
    /// than a `begin … end` block. It stays on the stack for exactly one
    /// statement; the old implementation pushed these and never popped them,
    /// so everything after such a loop was reported as "in loop".
    PendingSingleStatementLoop,
}

fn analyze_proc_text(
    proc_text: &str,
    start_line: u32,
    file_path: &str,
    object_name: &str,
    proc_name: &str,
    violations: &mut Vec<SqlPatternViolation>,
) {
    let mut stack: Vec<Frame> = Vec::new();
    // Filters are tracked *per record variable*: a single procedure-wide flag
    // meant `Customer.SetRange(...); Vendor.FindSet();` suppressed the warning
    // for the unrelated `Vendor`.
    let mut filtered_records: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (offset, line) in proc_text.lines().enumerate() {
        let line_num = start_line + offset as u32;
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let cleaned = strip_string_literals(line);
        let lower = cleaned.trim().to_lowercase();
        if lower.is_empty() {
            continue;
        }

        let loop_head = is_loop_start(&lower);
        let is_repeat = lower == "repeat" || lower.starts_with("repeat ");
        let opens_block = lower == "begin"
            || lower.ends_with(" begin")
            || lower.ends_with("\tbegin")
            || lower.ends_with("do begin");
        let closes_block = lower == "end"
            || lower.starts_with("end;")
            || lower.starts_with("end ")
            || lower.starts_with("end)");
        let closes_repeat = lower.starts_with("until ") || lower == "until";

        // A closing token first: `end else begin` both closes and opens.
        if closes_block {
            pop_block(&mut stack);
        }
        if closes_repeat {
            pop_block(&mut stack);
        }

        if loop_head {
            if is_repeat || opens_block {
                stack.push(Frame::Block { is_loop_body: true });
            } else {
                stack.push(Frame::PendingSingleStatementLoop);
            }
        } else if opens_block {
            // `for … do` on one line and `begin` on the next: the block *is*
            // the loop body.
            if matches!(stack.last(), Some(Frame::PendingSingleStatementLoop)) {
                stack.pop();
                stack.push(Frame::Block { is_loop_body: true });
            } else {
                stack.push(Frame::Block {
                    is_loop_body: false,
                });
            }
        }

        let in_loop = stack.iter().any(|frame| {
            matches!(
                frame,
                Frame::Block { is_loop_body: true } | Frame::PendingSingleStatementLoop
            )
        });

        for (receiver, method) in record_method_calls(&lower) {
            match method {
                "setrange" | "setfilter" => {
                    filtered_records.insert(receiver.to_string());
                }
                // `Reset` clears every filter on the record variable.
                "reset" => {
                    filtered_records.remove(receiver);
                }
                _ => {}
            }
        }

        if in_loop {
            if contains_record_method(&lower, "findfirst")
                || contains_record_method(&lower, "findlast")
            {
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
            if contains_record_method(&lower, "calcfields") {
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

        for (receiver, method) in record_method_calls(&lower) {
            if method != "findset" {
                continue;
            }
            if !filtered_records.contains(receiver) {
                violations.push(make_violation(
                    SqlAntiPattern::FindSetWithoutFilters,
                    "FindSet() without filters causes full table scan",
                    object_name,
                    proc_name,
                    file_path,
                    line_num,
                ));
            }
            filtered_records.remove(receiver);
        }

        // A statement consumed the pending single-statement loop body.
        let is_statement = !loop_head && !opens_block && !closes_block && !closes_repeat;
        if is_statement && matches!(stack.last(), Some(Frame::PendingSingleStatementLoop)) {
            stack.pop();
        }
    }
}

/// Pop the innermost real block, discarding any unresolved single-statement
/// loop markers that sit above it.
fn pop_block(stack: &mut Vec<Frame>) {
    while matches!(stack.last(), Some(Frame::PendingSingleStatementLoop)) {
        stack.pop();
    }
    stack.pop();
}

/// Whether `lower` calls the record method `method` on some receiver, in either
/// the parenthesised (`Item.FindFirst()`) or the bare (`Item.FindFirst;`,
/// `if Item.FindFirst then`) form — both are legal AL.
fn contains_record_method(lower: &str, method: &str) -> bool {
    record_method_calls(lower).any(|(_, name)| name == method)
}

/// Iterate `(receiver, method)` pairs for every `<receiver>.<method>` in
/// `lower`, where `lower` is an already-lowercased, literal-stripped line.
fn record_method_calls(lower: &str) -> impl Iterator<Item = (&str, &str)> + '_ {
    let bytes = lower.as_bytes();
    let mut index = 0usize;
    std::iter::from_fn(move || {
        while index < lower.len() {
            let dot = lower[index..].find('.')? + index;
            index = dot + 1;
            let method_start = dot + 1;
            let mut method_end = method_start;
            while method_end < lower.len()
                && (bytes[method_end].is_ascii_alphanumeric() || bytes[method_end] == b'_')
            {
                method_end += 1;
            }
            if method_end == method_start {
                continue;
            }
            let receiver = receiver_before(lower, dot);
            if receiver.is_empty() {
                continue;
            }
            return Some((receiver, &lower[method_start..method_end]));
        }
        None
    })
}

/// The identifier chain immediately preceding the `.` at `dot`.
fn receiver_before(lower: &str, dot: usize) -> &str {
    let before = &lower[..dot];
    if let Some(stripped) = before.strip_suffix('"') {
        if let Some(open) = stripped.rfind('"') {
            return &before[open..];
        }
    }
    let start = before
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '"'))
        .map(|index| index + 1)
        .unwrap_or(0);
    before[start..].trim_end_matches('.')
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

        let v = detect_sql_patterns(&ws).unwrap();
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

        let v = detect_sql_patterns(&ws).unwrap();
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

        let v = detect_sql_patterns(&ws).unwrap();
        let filt: Vec<_> = v
            .iter()
            .filter(|x| x.kind == SqlAntiPattern::FindSetWithoutFilters)
            .collect();
        assert!(filt.is_empty(), "Filtered FindSet no warn: {:?}", filt);
    }

    #[test]
    fn empty_workspace_no_violations() {
        assert!(detect_sql_patterns(&Workspace::new()).unwrap().is_empty());
    }

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

        let v = detect_sql_patterns(&ws).unwrap();
        assert!(
            v.iter().any(|x| x.kind == SqlAntiPattern::FindInLoop),
            "FindFirst after nested begin..end inside loop should be flagged: {:?}",
            v
        );
    }

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

        let v = detect_sql_patterns(&ws).unwrap();
        assert!(
            !v.iter().any(|x| x.kind == SqlAntiPattern::FindInLoop),
            "FindFirst inside string literal/comment must not be flagged. Got: {:?}",
            v
        );
    }

    /// The `end;` that closes a loop only decremented the begin counter, so
    /// everything after the loop stayed "in loop" until the next `end;`.
    #[test]
    fn statements_after_a_loop_are_not_reported_as_in_loop() {
        let ws = workspace_with(vec![(
            "/src/AfterLoop.al",
            r#"codeunit 50100 "After Loop"
{
    procedure Run()
    var
        i: Integer;
        Item: Record Item;
    begin
        for i := 1 to 10 do begin
            Message('x');
        end;
        if Item.FindFirst() then
            Message('after the loop');
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws).unwrap();
        assert!(
            !v.iter().any(|x| x.kind == SqlAntiPattern::FindInLoop),
            "FindFirst after the loop must not be flagged: {:?}",
            v
        );
    }

    /// A single-statement loop body was pushed but never popped.
    #[test]
    fn single_statement_loop_is_tracked_and_then_popped() {
        let ws = workspace_with(vec![(
            "/src/SingleStatement.al",
            r#"codeunit 50100 "Single Statement"
{
    procedure Run()
    var
        i: Integer;
        Item: Record Item;
        Other: Record Item;
    begin
        for i := 1 to 10 do
            Item.CalcFields(Inventory);
        Other.CalcFields(Inventory);
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws).unwrap();
        let calc: Vec<_> = v
            .iter()
            .filter(|x| x.kind == SqlAntiPattern::CalcFieldsInLoop)
            .collect();
        assert_eq!(
            calc.len(),
            1,
            "exactly the loop body must be flagged, not the statement after it: {:?}",
            v
        );
    }

    /// `Item.FindFirst;` / `if Item.FindFirst then` are legal AL and were missed.
    #[test]
    fn detects_bare_findfirst_without_parentheses() {
        let ws = workspace_with(vec![(
            "/src/Bare.al",
            r#"codeunit 50100 "Bare Find"
{
    procedure Run()
    var
        Item: Record Item;
        Line: Record "Sales Line";
    begin
        repeat
            if Item.FindFirst then
                Item.FindLast;
        until Line.Next() = 0;
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws).unwrap();
        let finds: Vec<_> = v
            .iter()
            .filter(|x| x.kind == SqlAntiPattern::FindInLoop)
            .collect();
        assert_eq!(
            finds.len(),
            2,
            "both bare FindFirst and bare FindLast must be flagged: {:?}",
            v
        );
    }

    /// A filter on one record variable must not suppress the warning for another.
    #[test]
    fn findset_filter_tracking_is_per_record_variable() {
        let ws = workspace_with(vec![(
            "/src/PerVar.al",
            r#"codeunit 50100 "Per Var"
{
    procedure Run()
    var
        Customer: Record Customer;
        Vendor: Record Vendor;
    begin
        Customer.SetRange(Blocked, false);
        if Customer.FindSet() then
            Message('customers');
        if Vendor.FindSet() then
            Message('vendors');
    end;
}"#,
        )]);

        let v = detect_sql_patterns(&ws).unwrap();
        let unfiltered: Vec<_> = v
            .iter()
            .filter(|x| x.kind == SqlAntiPattern::FindSetWithoutFilters)
            .collect();
        assert_eq!(
            unfiltered.len(),
            1,
            "only the unfiltered Vendor.FindSet must be flagged: {:?}",
            v
        );
    }

    #[test]
    fn receiver_before_reads_the_identifier_chain() {
        assert_eq!(
            receiver_before("if customer.findset() then", 11),
            "customer"
        );
        // A quoted receiver keeps its quotes so two different quoted names stay
        // distinct in the per-variable filter set.
        assert_eq!(receiver_before("\"my rec\".setrange(", 8), "\"my rec\"");
        assert_eq!(receiver_before("salesline.findset()", 9), "salesline");
    }
}
