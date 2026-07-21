//! Regression coverage for the `al-explorer` analysis/insight commands — the
//! capabilities with no VS Code AL-extension equivalent (dead-code, SQL scan,
//! event tracing, impact, entry points, data-classification audit, metrics).
//! Keeps the "Zed differentiators" honest by asserting they actually produce
//! results on the bundled fixture.

use std::process::Command;

use al_test_harness::{al_explorer_binary, test_project_dir};

fn al(args: &[&str]) -> (bool, String) {
    let out = Command::new(al_explorer_binary())
        .args(args)
        .current_dir(test_project_dir())
        .output()
        .expect("run al-explorer");
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), combined)
}

fn assert_has(args: &[&str], needle: &str) {
    let (ok, out) = al(args);
    assert!(ok, "`al-explorer {}` failed:\n{out}", args.join(" "));
    assert!(
        out.contains(needle),
        "`al-explorer {}` missing {needle:?}:\n{out}",
        args.join(" ")
    );
}

#[test]
fn dead_code_finds_unused_local_helper() {
    // MultiProcedure.al defines LocalHelper with no callers.
    assert_has(&["dead-code"], "LocalHelper");
}

#[test]
fn sql_scan_flags_findset_without_filters() {
    assert_has(&["sql-scan"], "findSetWithoutFilters");
}

#[test]
fn entrypoints_discovers_procedures() {
    assert_has(&["entrypoints"], "Entry points");
}

#[test]
fn events_lists_integration_event() {
    assert_has(&["events", "OnAfter"], "OnAfterProcess");
}

#[test]
fn impact_reports_consumers_of_a_table() {
    assert_has(&["impact", "Customer"], "Sales Order Pageext");
}

#[test]
fn audit_data_flags_missing_data_classification() {
    assert_has(&["audit-data"], "missing");
}

#[test]
fn metrics_all_reports_complexity() {
    assert_has(&["metrics", "--all"], "cyclomatic");
}

#[test]
fn intercept_runs_workspace_wide() {
    let (ok, out) = al(&["intercept"]);
    assert!(ok, "intercept failed:\n{out}");
    assert!(!out.trim().is_empty(), "intercept produced no output");
}
