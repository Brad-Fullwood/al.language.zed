//! Dynamic statement and branch coverage integration tests.
//!
//! These drive the real statement evaluator over parsed AL and assert on the
//! collected [`Coverage`]: a taken `if`/`case` branch's body lines are
//! recorded, the not-taken branch's are not, branch decisions are tallied, and
//! a disabled collector records nothing (opt-in / zero-cost).

use std::path::PathBuf;
use std::sync::Arc;

use tree_sitter::Node;

use crate::interpreter::coverage::Coverage;
use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_stmt::eval_stmt;
use crate::interpreter::scope::{CallFrame, Eval, ScopeStack};
use crate::interpreter::value::Value;
use crate::test_support::MockSource;

/// 1-based line number of the first source line containing `needle`.
fn line_of(src: &str, needle: &str) -> u32 {
    src.lines()
        .position(|l| l.contains(needle))
        .map(|i| i as u32 + 1)
        .unwrap_or_else(|| panic!("needle {needle:?} not found in source"))
}

/// Find the `begin_end_block` body of a named procedure (iterative walk).
fn find_proc_body<'a>(root: Node<'a>, proc_name: &str, src: &[u8]) -> Option<Node<'a>> {
    let mut stack = vec![root];
    while let Some(cur) = stack.pop() {
        if cur.kind() == "procedure_declaration" {
            if let Some(name) = cur.child_by_field_name("name") {
                let n = name.utf8_text(src).unwrap_or("").trim_matches('"');
                if n.eq_ignore_ascii_case(proc_name) {
                    let mut c = cur.walk();
                    for ch in cur.named_children(&mut c) {
                        if ch.kind() == "begin_end_block" {
                            return Some(ch);
                        }
                    }
                }
            }
        }
        let mut c = cur.walk();
        stack.extend(cur.named_children(&mut c));
    }
    None
}

const COV_FILE: &str = "/cov/Cov.al";

/// Run `proc_name`'s body from `src` with coverage enabled, attributed to
/// `COV_FILE`. Returns the resulting `Eval` and the populated collector.
fn run_with_coverage(src: &str, proc_name: &str) -> (Eval, Coverage) {
    let parsed = al_syntax::AlParser::parse_quick(src);
    let tree = parsed.tree;
    let bytes = src.as_bytes();
    let body = find_proc_body(tree.root_node(), proc_name, bytes).expect("procedure body");

    let mut stack = ScopeStack::new();
    let mut frame = CallFrame::new("Cov", proc_name);
    frame.bind("x", Value::Integer(0));
    // Bind the procedure's declared locals (the body's parent is its
    // `procedure_declaration`) — assignment to unbound names is an error.
    if let Some(proc_node) = body.parent() {
        crate::interpreter::dispatch::bind_procedure_locals(proc_node, bytes, &mut frame);
    }
    stack.push(frame);

    let mut ctx = DispatchCtx::new_pure(Arc::new(MockSource::new()));
    ctx.coverage = Some(Coverage::new());
    ctx.cov_enter_file(COV_FILE);

    let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
    (eval, ctx.coverage.expect("coverage enabled"))
}

#[test]
fn if_else_records_taken_branch_excludes_untaken() {
    // The THEN branch (x := 1) runs; the ELSE branch (x := 2) does not.
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
    begin
        x := 0;
        if x = 0 then
            x := 1
        else
            x := 2;
    end;
}
"#;
    let (eval, cov) = run_with_coverage(src, "Test");
    assert!(
        matches!(eval, Eval::Normal(_)),
        "run must succeed, got {eval:?}"
    );

    let assign0 = line_of(src, "x := 0");
    let if_line = line_of(src, "if x = 0");
    let taken = line_of(src, "x := 1");
    let untaken = line_of(src, "x := 2");

    assert!(
        cov.is_line_executed(COV_FILE, assign0),
        "x := 0 must be covered"
    );
    assert!(
        cov.is_line_executed(COV_FILE, if_line),
        "the if line must be covered"
    );
    assert!(
        cov.is_line_executed(COV_FILE, taken),
        "taken THEN branch (x := 1) must be covered"
    );
    assert!(
        !cov.is_line_executed(COV_FILE, untaken),
        "not-taken ELSE branch (x := 2) must NOT be covered"
    );

    let tally = cov
        .branch(COV_FILE, if_line)
        .expect("if branch decision recorded");
    assert_eq!(tally.then_taken, 1, "THEN was taken exactly once");
    assert_eq!(tally.else_taken, 0, "ELSE was never taken");

    // The report mirrors the live view.
    let report = cov.report();
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].file, COV_FILE);
    assert!(report.files[0].executed_lines.contains(&taken));
    assert!(!report.files[0].executed_lines.contains(&untaken));
}

#[test]
fn else_branch_taken_when_condition_false() {
    // Mirror image: the ELSE branch runs; the THEN branch does not.
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
    begin
        x := 5;
        if x = 0 then
            x := 1
        else
            x := 2;
    end;
}
"#;
    let (eval, cov) = run_with_coverage(src, "Test");
    assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");

    let if_line = line_of(src, "if x = 0");
    let then_line = line_of(src, "x := 1");
    let else_line = line_of(src, "x := 2");

    assert!(
        !cov.is_line_executed(COV_FILE, then_line),
        "not-taken THEN branch must NOT be covered"
    );
    assert!(
        cov.is_line_executed(COV_FILE, else_line),
        "taken ELSE branch must be covered"
    );
    let tally = cov
        .branch(COV_FILE, if_line)
        .expect("branch decision recorded");
    assert_eq!(tally.then_taken, 0);
    assert_eq!(tally.else_taken, 1);
}

#[test]
fn case_records_matched_arm_excludes_else() {
    // A matching arm runs (x := 10); the else arm (x := 99) does not.
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
    begin
        x := 1;
        case x of
            1:
                x := 10;
            else
                x := 99;
        end;
    end;
}
"#;
    let (eval, cov) = run_with_coverage(src, "Test");
    assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");

    let case_line = line_of(src, "case x of");
    let arm_match = line_of(src, "x := 10");
    let arm_else = line_of(src, "x := 99");

    assert!(
        cov.is_line_executed(COV_FILE, arm_match),
        "matched arm body (x := 10) must be covered"
    );
    assert!(
        !cov.is_line_executed(COV_FILE, arm_else),
        "else arm (x := 99) must NOT be covered"
    );

    // An arm matched: recorded as the THEN side of the case decision.
    let tally = cov
        .branch(COV_FILE, case_line)
        .expect("case decision recorded");
    assert_eq!(tally.then_taken, 1, "an arm matched");
    assert_eq!(tally.else_taken, 0, "else was not taken");
}

#[test]
fn case_else_taken_when_no_arm_matches() {
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
    begin
        x := 7;
        case x of
            1:
                x := 10;
            else
                x := 99;
        end;
    end;
}
"#;
    let (eval, cov) = run_with_coverage(src, "Test");
    assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");

    let case_line = line_of(src, "case x of");
    let arm_match = line_of(src, "x := 10");
    let arm_else = line_of(src, "x := 99");

    assert!(
        !cov.is_line_executed(COV_FILE, arm_match),
        "non-matching arm (x := 10) must NOT be covered"
    );
    assert!(
        cov.is_line_executed(COV_FILE, arm_else),
        "else arm (x := 99) must be covered"
    );
    let tally = cov
        .branch(COV_FILE, case_line)
        .expect("case decision recorded");
    assert_eq!(tally.then_taken, 0, "no arm matched");
    assert_eq!(tally.else_taken, 1, "else was taken");
}

#[test]
fn case_range_label_matches_inclusively() {
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
    begin
        x := 10;
        case x of
            10 .. 20:
                x := 99;
            else
                x := 0;
        end;
    end;
}
"#;
    let (eval, cov) = run_with_coverage(src, "Test");
    assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
    assert!(cov.is_line_executed(COV_FILE, line_of(src, "x := 99")));
    assert!(!cov.is_line_executed(COV_FILE, line_of(src, "x := 0")));
}

#[test]
fn case_reports_each_arm_as_a_distinct_path() {
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
    begin
        x := 2;
        case x of
            1:
                x := 10;
            2:
                x := 20;
            else
                x := 99;
        end;
    end;
}
"#;
    let (eval, cov) = run_with_coverage(src, "Test");
    assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");

    let branch = cov
        .branch(COV_FILE, line_of(src, "case x of"))
        .expect("case branch");
    assert_eq!(branch.paths.len(), 3, "two arms plus else: {branch:?}");
    let selected = branch
        .paths
        .iter()
        .find(|(path, _)| path.starts_with("arm:2:"))
        .expect("second arm registered");
    assert_eq!(*selected.1, 1);
    assert_eq!(
        branch.paths.iter().filter(|(_, hits)| **hits > 0).count(),
        1,
        "only one case path can execute per evaluation"
    );
}

#[test]
fn loop_decisions_record_entry_and_natural_exit() {
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
    begin
        x := 0;
        while x < 2 do
            x += 1;
        for x := 1 to 2 do
            x := x;
        repeat
            x -= 1;
        until x = 0;
    end;
}
"#;
    let (eval, cov) = run_with_coverage(src, "Test");
    assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");

    let while_tally = cov
        .branch(COV_FILE, line_of(src, "while x < 2"))
        .expect("while decision");
    assert_eq!((while_tally.then_taken, while_tally.else_taken), (2, 1));

    let for_tally = cov
        .branch(COV_FILE, line_of(src, "for x := 1"))
        .expect("for decision");
    assert_eq!((for_tally.then_taken, for_tally.else_taken), (2, 1));

    let repeat_tally = cov
        .branch(COV_FILE, line_of(src, "repeat"))
        .expect("repeat decision");
    assert_eq!((repeat_tally.then_taken, repeat_tally.else_taken), (1, 1));
}

#[test]
fn compound_decision_records_real_mcdc_condition_vectors() {
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
        a: Boolean;
        b: Boolean;
    begin
        for x := 0 to 2 do begin
            a := x <> 0;
            b := x <> 1;
            if a and b then
                x := x;
        end;
    end;
}
"#;
    let (eval, cov) = run_with_coverage(src, "Test");
    assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");

    let report = cov.report();
    let branch = report.files[0]
        .branches
        .iter()
        .find(|branch| branch.line == line_of(src, "if a and b"))
        .expect("compound IF branch");
    let mcdc = branch.mcdc.as_ref().expect("MC/DC detail");
    assert_eq!(mcdc.conditions.len(), 2);
    assert_eq!(mcdc.covered_count(), 2, "{mcdc:?}");
    assert_eq!(
        mcdc.observations
            .iter()
            .map(|observation| (observation.conditions.clone(), observation.outcome))
            .collect::<Vec<_>>(),
        vec![
            (vec![false, true], false),
            (vec![true, false], false),
            (vec![true, true], true),
        ]
    );
}

#[test]
fn disabled_coverage_records_nothing() {
    // Opt-in / zero-cost: with no collector attached the run produces no
    // coverage at all (`ctx.coverage` stays `None`).
    let src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        x: Integer;
    begin
        x := 0;
        if x = 0 then
            x := 1
        else
            x := 2;
    end;
}
"#;
    let parsed = al_syntax::AlParser::parse_quick(src);
    let tree = parsed.tree;
    let bytes = src.as_bytes();
    let body = find_proc_body(tree.root_node(), "Test", bytes).expect("Test body");

    let mut stack = ScopeStack::new();
    let mut frame = CallFrame::new("Cov", "Test");
    frame.bind("x", Value::Integer(0));
    stack.push(frame);

    // No collector attached (the default).
    let mut ctx = DispatchCtx::new_pure(Arc::new(MockSource::new()));
    assert!(ctx.coverage.is_none(), "coverage must default to disabled");

    let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
    assert!(
        matches!(eval, Eval::Normal(_)),
        "run must still succeed, got {eval:?}"
    );

    assert!(
        ctx.coverage.is_none(),
        "a disabled run must produce no coverage data"
    );
}

#[test]
fn cross_procedure_call_attributes_lines_to_callee_file() {
    // Coverage from a procedure defined in another file is attributed to that
    // file, exercising the dispatcher's cov_enter_file/cov_restore_file wiring.
    let caller_src = r#"codeunit 50100 "Cov"
{
    procedure Test()
    var
        r: Integer;
    begin
        r := Helper.Work();
    end;
}
"#;
    let helper_src = r#"codeunit 50999 "Helper"
{
    procedure Work(): Integer
    begin
        exit(42);
    end;
}
"#;
    let caller_path = "/x/Caller.al";
    let helper_path = "/x/Helper.al";

    let ws = MockSource::new();
    ws.file_index
        .add_file(PathBuf::from(caller_path), caller_src.to_string());
    ws.file_index
        .add_file(PathBuf::from(helper_path), helper_src.to_string());

    // Parse the caller and find Test's body to run directly.
    let parsed = al_syntax::AlParser::parse_quick(caller_src);
    let tree = parsed.tree;
    let bytes = caller_src.as_bytes();
    let body = find_proc_body(tree.root_node(), "Test", bytes).expect("Test body");

    let mut stack = ScopeStack::new();
    let mut frame = CallFrame::new("Cov", "Test");
    frame.bind("r", Value::Integer(0));
    stack.push(frame);

    let mut ctx = DispatchCtx::new_pure(Arc::new(ws));
    ctx.coverage = Some(Coverage::new());
    ctx.cov_enter_file(caller_path);

    let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
    assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");

    let cov = ctx.coverage.expect("coverage enabled");

    // The caller's call statement is attributed to the caller file.
    let call_line = line_of(caller_src, "Helper.Work()");
    assert!(
        cov.is_line_executed(caller_path, call_line),
        "caller's call line must be attributed to the caller file"
    );

    // The helper's executed body is attributed to the helper file, NOT the
    // caller file — proving cross-file attribution.
    let exit_line = line_of(helper_src, "exit(42)");
    assert!(
        cov.is_line_executed(helper_path, exit_line),
        "helper body must be attributed to the helper file"
    );
    assert!(
        !cov.is_line_executed(caller_path, exit_line),
        "helper body must NOT leak into the caller file"
    );

    // Both files appear in the aggregated report.
    let report = cov.report();
    let files: Vec<&str> = report.files.iter().map(|f| f.file.as_str()).collect();
    assert!(files.contains(&caller_path));
    assert!(files.contains(&helper_path));
}
