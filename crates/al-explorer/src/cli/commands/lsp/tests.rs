//! Test commands: discovery, coverage, single/affected/all runs, classification, and result history.

use std::fmt::Write as _;
use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_tests_discover(json: bool) -> ExitCode {
    run_command(
        "tests.discover",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let tests = list_rows(result).as_array().cloned().unwrap_or_default();
            if tests.is_empty() {
                eprintln!("No test codeunits found");
            } else {
                print!("{}", test_codeunits_text(&tests));
                eprintln!("\n{} test codeunit(s) found", tests.len());
            }
        },
    )
}

/// The lines `tests` prints, one per test codeunit.
fn test_codeunits_text(tests: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for t in tests {
        let name = text_field(t, "name", "?");
        let id = t.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let count = t
            .get("tests")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let _ = writeln!(out, "  Codeunit {id} \"{name}\" -- {count} test(s)");
    }
    out
}

pub fn cmd_tests_coverage(json: bool) -> ExitCode {
    run_command(
        "tests.coverage",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            // The daemon returns `{ coverage: [{ testProcedure, covers: [...] }],
            // untested: [...] }`. The previous formatter read `coveredProcedures`/
            // `totalProcedures`, which the daemon never emits, so it always printed
            // "0/0 procedures (0%)". Derive real counts here.
            let coverage = result.get("coverage").and_then(|v| v.as_array());
            let untested = result.get("untested").and_then(|v| v.as_array());
            let test_count = coverage.map_or(0, Vec::len);
            let mut covered_set = std::collections::HashSet::new();
            if let Some(entries) = coverage {
                for entry in entries {
                    if let Some(covers) = entry.get("covers").and_then(|v| v.as_array()) {
                        for c in covers {
                            let object = c.get("object").and_then(|v| v.as_str()).unwrap_or("");
                            let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            covered_set.insert(format!("{object}::{name}"));
                        }
                    }
                }
            }
            let covered = covered_set.len();
            let untested_count = untested.map_or(0, Vec::len);
            let total = covered + untested_count;
            let pct = (covered * 100).checked_div(total).unwrap_or(0);
            println!(
                "Test coverage: {covered}/{total} procedures covered ({pct}%) \
                 across {test_count} test procedure(s)"
            );
            if let Some(entries) = untested {
                if !entries.is_empty() {
                    println!("\nUntested procedures ({untested_count}):");
                    for u in entries {
                        let object = text_field(u, "object", "?");
                        let name = text_field(u, "name", "?");
                        println!("  {object}::{name}");
                    }
                }
            }
        },
    )
}

/// `al test-run <codeunit> [--name <name>] [--method <method>] [--config <config>]`
///
/// Sends a `tests.run` JSON-RPC request to the al-lsp daemon. The daemon
/// calls the BC REST dev API and returns the test results plus diagnostics.
pub fn cmd_test_run(
    codeunit: i64,
    name: Option<&str>,
    method: Option<&str>,
    config: Option<&str>,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let mut params = serde_json::json!({ "codeunit": codeunit });
    if let Some(n) = name {
        params["codeunitName"] = serde_json::Value::String(n.to_string());
    }
    if let Some(m) = method {
        params["method"] = serde_json::Value::String(m.to_string());
    }
    if let Some(c) = config {
        params["config"] = serde_json::Value::String(c.to_string());
    }

    match request_checked(&mut client, "tests.run", Some(params)) {
        Ok(result) => {
            let exit_code = match test_run_exit_code(&result) {
                Ok(code) => code,
                Err(error) => {
                    return report_error(&error, json);
                }
            };
            if json {
                print_json(&result);
            } else {
                if let Some(run) = result.get("result") {
                    print!("{}", test_run_text(run));
                } else {
                    println!("No test results returned");
                }
            }
            exit_code
        }
        Err(e) => report_error(&e, json),
    }
}

/// The text `test-run` prints: the codeunit's counts and one line per
/// method.
fn test_run_text(run: &serde_json::Value) -> String {
    let mut out = String::new();
    let cu_name = text_field(run, "name", "?");
    let total = run.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let passed = run.get("passed").and_then(|v| v.as_u64()).unwrap_or(0);
    let failed = run.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
    let skipped = run.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);
    let _ = writeln!(
        out,
        "Codeunit \"{cu_name}\": {passed}/{total} passed, {failed} failed, {skipped} skipped"
    );
    if let Some(methods) = run.get("methods").and_then(|v| v.as_array()) {
        for m in methods {
            let mname = text_field(m, "name", "?");
            let icon = match m.get("status").and_then(|v| v.as_str()).unwrap_or("?") {
                "pass" => "✓",
                "fail" => "✗",
                _ => "~",
            };
            let _ = write!(out, "  {icon} {mname}");
            if let Some(err) = m.get("error").and_then(|v| v.as_str()) {
                let _ = write!(out, " -- {}", terminal_text(err));
            }
            let _ = writeln!(out);
        }
    }
    out
}

fn test_run_exit_code(result: &serde_json::Value) -> Result<ExitCode, String> {
    let failed = result
        .get("result")
        .and_then(|run| run.get("failed"))
        .and_then(|value| value.as_u64())
        .ok_or_else(|| {
            "validated test-run response has no non-negative integer failed count".to_string()
        })?;
    if failed > 0 {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// `al-explorer test-affected <files...>` — show which tests touch any of the given files.
pub fn cmd_test_affected(files: &[String], json: bool) -> ExitCode {
    // Canonicalize client-side against the CLI's working directory. The daemon
    // canonicalizes from its own CWD, so a relative path supplied here would not
    // match the absolute paths in the daemon's file index. Resolve each path now
    // and error if any cannot be found, matching the test-snapshot diff pattern.
    let mut abs_files = Vec::with_capacity(files.len());
    for f in files {
        match std::path::Path::new(f).canonicalize() {
            Ok(p) => abs_files.push(p.display().to_string()),
            Err(e) => return report_error(&format!("Cannot resolve path '{f}': {e}"), json),
        }
    }
    let params = serde_json::json!({ "changedFiles": abs_files });
    run_command("tests.affected", Some(params), json, None, |result| {
        let affected = result
            .get("affected")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if affected.is_empty() {
            eprintln!("No tests touch the given files");
            return;
        }
        for t in &affected {
            let cu_name = text_field(t, "codeunitName", "?");
            let cu_id = t.get("codeunitId").and_then(|v| v.as_i64()).unwrap_or(0);
            let method = text_field(t, "methodName", "?");
            let line = t.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
            println!("  Codeunit {cu_id} \"{cu_name}\" :: {method} (line {line})");
        }
        eprintln!("\n{} affected test(s)", affected.len());
    })
}

/// `al-explorer test-results [--codeunit ID] [--method NAME]` — show persisted test history.
pub fn cmd_test_results(codeunit: Option<i64>, method: Option<&str>, json: bool) -> ExitCode {
    let mut params = serde_json::json!({});
    if let Some(id) = codeunit {
        params["codeunitId"] = serde_json::Value::from(id);
    }
    if let Some(m) = method {
        params["methodName"] = serde_json::Value::String(m.to_string());
    }
    run_command("tests.last_results", Some(params), json, None, |result| {
        if let Some(last) = result.get("lastResult") {
            if last.is_null() {
                eprintln!("No prior runs recorded for that (codeunit, method) pair.");
                return;
            }
            print!("{}", history_row_text(last));
            return;
        }
        let results = result
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if results.is_empty() {
            eprintln!("No test history recorded yet — run tests first.");
            return;
        }
        for r in &results {
            print!("{}", history_row_text(r));
        }
        eprintln!("\n{} record(s)", results.len());
    })
}

/// The lines `test-results` prints for one recorded run.
fn history_row_text(r: &serde_json::Value) -> String {
    let mut out = String::new();
    let cu = text_field(r, "codeunitName", "?");
    let cu_id = r.get("codeunitId").and_then(|v| v.as_i64()).unwrap_or(0);
    let method = text_field(r, "methodName", "?");
    let status = text_field(r, "status", "?");
    let icon = match status.as_str() {
        "pass" => "✓",
        "fail" => "✗",
        "skip" => "⊘",
        _ => "?",
    };
    let dur = r
        .get("durationMs")
        .and_then(|v| v.as_u64())
        .map(|n: u64| format!("{n}ms"))
        .unwrap_or_else(|| "—".to_string());
    let ts = r.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
    let _ = writeln!(
        out,
        "  {icon} [{cu_id} {cu}::{method}] {status} {dur}  (ts={ts})"
    );
    let err = text_field(r, "error", "");
    if !err.is_empty() {
        let _ = writeln!(out, "      {err}");
    }
    out
}

/// Human-readable note clarifying where a test with the given routing decision
/// **actually executes today** — not where the class name implies.
///
/// Keep this wording in lockstep with
/// `al_test::router::RoutingDecision::execution_note` (al-explorer talks to the
/// daemon over JSON-RPC and only sees the decision string, so it cannot call
/// that method directly).
fn classify_execution_note(decision: &str) -> &'static str {
    match decision {
        "interp" => "runs locally on the Rust interpreter",
        "interpRecord" => "runs locally with the in-memory record runtime",
        "snapshot" => "replays a captured snapshot, else routes to live BC",
        "liveBc" => "routes to live BC",
        _ => "",
    }
}

/// `al-explorer test-classify` — show the routing decision for every test.
pub fn cmd_test_classify(json: bool) -> ExitCode {
    run_command(
        "tests.classify",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let classifications = result
                .get("classifications")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if classifications.is_empty() {
                eprintln!("No test codeunits discovered");
                return;
            }
            eprintln!(
                "Routing class -> where the test actually runs today \
                 (`interp` and supported `interpRecord` tests execute locally):\n"
            );
            print!("{}", classifications_text(&classifications));
            eprintln!("\n{} test(s) classified", classifications.len());
        },
    )
}

/// The lines `test-classify` prints: each test, its routing class and where
/// it runs.
fn classifications_text(classifications: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for c in classifications {
        let cu = text_field(c, "codeunitName", "?");
        let method = text_field(c, "methodName", "?");
        let decision = text_field(c, "decision", "?");
        let reasons = c
            .get("reasons")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|r| r.get("message").and_then(|m| m.as_str()))
                    .map(terminal_text)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let note = classify_execution_note(&decision);
        let suffix = if reasons.is_empty() {
            String::new()
        } else {
            format!(" -- {reasons}")
        };
        if note.is_empty() {
            let _ = writeln!(out, "  [{decision:>12}] {cu} :: {method}{suffix}");
        } else {
            let _ = writeln!(out, "  [{decision:>12}] {cu} :: {method}  ({note}){suffix}");
        }
    }
    out
}

/// `al test-run-all [--parallel] [--junit-out X] [--cobertura-out Y] [--filter PATTERN] [--timeout-ms N] [--coverage]`
///
/// Runs every discovered test codeunit through the daemon's `tests.run_auto`
/// endpoint. One blocking request runs the whole suite, then a per-codeunit
/// summary and a final totals line are printed. Exits non-zero if any test
/// failed.
///
/// `--coverage` collects *dynamic* executed-line coverage on
/// interpreter-routed tests: the daemon returns a `coverage` object and, when
/// `--cobertura-out` is also given, writes a dynamic-mode Cobertura document.
pub fn cmd_test_run_all(
    parallel: bool,
    timeout_ms: Option<u64>,
    junit_out: Option<&str>,
    cobertura_out: Option<&str>,
    filter: Option<&str>,
    coverage: bool,
    json: bool,
) -> ExitCode {
    let mut params = serde_json::json!({ "parallel": parallel });
    if let Some(ms) = timeout_ms {
        params["timeoutMs"] = serde_json::Value::from(ms);
    }
    if let Some(p) = junit_out {
        params["junitOut"] = serde_json::Value::String(p.to_string());
    }
    if let Some(p) = cobertura_out {
        params["coberturaOut"] = serde_json::Value::String(p.to_string());
    }
    if let Some(p) = filter {
        params["filter"] = serde_json::Value::String(p.to_string());
    }
    if coverage {
        params["coverage"] = serde_json::Value::Bool(true);
    }

    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    // A full test run (live BC or interpreter) can take many minutes.
    client.set_request_timeout(std::time::Duration::from_secs(1800));

    match request_checked(&mut client, "tests.run_auto", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                let failed = result
                    .get("totals")
                    .and_then(|t| t.get("failed"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if failed > 0 {
                    return ExitCode::FAILURE;
                }
                return ExitCode::SUCCESS;
            }

            let summaries = result
                .get("summaries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for cu in &summaries {
                let cu_name = text_field(cu, "name", "?");
                let total = cu.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                let passed = cu.get("passed").and_then(|v| v.as_u64()).unwrap_or(0);
                let failed = cu.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
                let skipped = cu.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);
                let icon = if failed > 0 { "✗" } else { "✓" };
                println!(
                    "{icon} {cu_name}: {passed}/{total} passed, {failed} failed, {skipped} skipped"
                );
            }

            let totals = result.get("totals");
            let total = totals
                .and_then(|t| t.get("total"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let passed = totals
                .and_then(|t| t.get("passed"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let failed = totals
                .and_then(|t| t.get("failed"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let skipped = totals
                .and_then(|t| t.get("skipped"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            eprintln!("\nTotal: {passed}/{total} passed, {failed} failed, {skipped} skipped");
            if failed > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

#[cfg(test)]
mod classify_note_tests {
    use super::classify_execution_note;

    #[test]
    fn interp_record_note_says_local_record_runtime() {
        let note = classify_execution_note("interpRecord");
        assert!(note.contains("locally"), "got: {note:?}");
        assert!(note.contains("record"), "got: {note:?}");
    }

    #[test]
    fn both_interpreter_tiers_are_local() {
        assert!(classify_execution_note("interp").contains("locally"));
        assert!(classify_execution_note("interpRecord").contains("locally"));
        assert!(!classify_execution_note("liveBc").contains("locally"));
        assert!(!classify_execution_note("snapshot").contains("locally"));
    }

    #[test]
    fn unknown_decision_has_no_note() {
        assert_eq!(classify_execution_note("???"), "");
    }
}

#[cfg(test)]
mod exit_status_tests {
    use super::*;

    #[test]
    fn single_test_run_fails_when_any_method_failed() {
        assert_eq!(
            test_run_exit_code(&serde_json::json!({"result": {"failed": 0}})).unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            test_run_exit_code(&serde_json::json!({"result": {"failed": 1}})).unwrap(),
            ExitCode::FAILURE
        );
        assert!(test_run_exit_code(&serde_json::json!({"result": {}})).is_err());
    }
}

#[cfg(test)]
mod terminal_text_tests {
    use super::{classifications_text, history_row_text, test_codeunits_text, test_run_text};

    const COLOURED: &str = "Bad\u{1b}[31m Name\u{1b}[0m";
    const TITLE: &str = "Sec7 Caller\u{1b}]0;pwned\u{7}";
    const CLEAR: &str = "Sec7 Tests\u{1b}[2J";

    fn assert_escaped(text: &str) {
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{text:?}"
        );
        assert!(text.contains(r"Sec7 Tests\u{1b}[2J"), "got: {text}");
    }

    #[test]
    fn test_codeunit_and_method_names_print_escaped() {
        let codeunits = [serde_json::json!({"name": CLEAR, "id": 50172, "tests": [{}, {}]})];
        let text = test_codeunits_text(&codeunits);
        assert_escaped(&text);
        assert_eq!(
            text,
            "  Codeunit 50172 \"Sec7 Tests\\u{1b}[2J\" -- 2 test(s)\n"
        );

        let run = serde_json::json!({
            "name": CLEAR, "total": 1, "passed": 0, "failed": 1, "skipped": 0,
            "methods": [{"name": COLOURED, "status": "fail", "error": TITLE}]
        });
        let text = test_run_text(&run);
        assert_escaped(&text);
        assert!(
            text.contains(r"  ✗ Bad\u{1b}[31m Name\u{1b}[0m -- Sec7 Caller\u{1b}]0;pwned\u{7}"),
            "got: {text}"
        );

        let row = serde_json::json!({
            "codeunitName": CLEAR, "codeunitId": 50172, "methodName": COLOURED,
            "status": "fail", "durationMs": 4, "timestamp": 1, "error": TITLE
        });
        assert_escaped(&history_row_text(&row));

        let classified = [serde_json::json!({
            "codeunitName": CLEAR, "methodName": COLOURED, "decision": "interp",
            "reasons": [{"message": TITLE}]
        })];
        let text = classifications_text(&classified);
        assert_escaped(&text);
        assert!(
            text.contains("(runs locally on the Rust interpreter)"),
            "got: {text}"
        );
    }
}
