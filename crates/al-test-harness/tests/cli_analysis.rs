//! Regression coverage for the `al-explorer` analysis/insight commands — the
//! capabilities with no VS Code AL-extension equivalent (dead-code, SQL scan,
//! event tracing, impact, entry points, data-classification audit, metrics).
//! Keeps the "Zed differentiators" honest by asserting they actually produce
//! results on the bundled fixture.

use std::process::{Command, ExitStatus};

use al_test_harness::{al_explorer_binary, test_project_dir};

fn al(args: &[&str]) -> (ExitStatus, String) {
    let out = Command::new(al_explorer_binary())
        .args(args)
        .current_dir(test_project_dir())
        .output()
        .expect("run al-explorer");
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status, combined)
}

fn assert_success_has(args: &[&str], needle: &str) {
    let (status, out) = al(args);
    assert!(
        status.success(),
        "`al-explorer {}` failed with {status}:\n{out}",
        args.join(" ")
    );
    assert!(
        out.contains(needle),
        "`al-explorer {}` missing {needle:?}:\n{out}",
        args.join(" ")
    );
}

fn assert_quality_gate_finds(args: &[&str], needle: &str) {
    let (status, out) = al(args);
    assert_eq!(
        status.code(),
        Some(1),
        "`al-explorer {}` must exit 1 when it reports findings, got {status}:\n{out}",
        args.join(" ")
    );
    assert!(
        out.contains(needle),
        "`al-explorer {}` missing {needle:?}:\n{out}",
        args.join(" ")
    );
}

#[test]
fn dead_code_finds_unused_local_helper() {
    // MultiProcedure.al defines LocalHelper with no callers.
    assert_quality_gate_finds(&["dead-code"], "LocalHelper");
}

#[test]
fn sql_scan_flags_findset_without_filters() {
    assert_quality_gate_finds(&["sql-scan"], "findSetWithoutFilters");
}

#[test]
fn entrypoints_discovers_procedures() {
    assert_success_has(&["entrypoints"], "Entry points");
}

#[test]
fn events_lists_integration_event() {
    assert_success_has(&["events", "OnAfter"], "OnAfterProcess");
}

#[test]
fn impact_reports_consumers_of_a_table() {
    assert_success_has(&["impact", "Customer"], "Sales Order Pageext");
}

#[test]
fn audit_data_flags_missing_data_classification() {
    assert_quality_gate_finds(&["audit-data"], "unclassified");
}

#[test]
fn metrics_all_reports_complexity() {
    assert_quality_gate_finds(&["metrics", "--all"], "cyclomatic");
}

#[test]
fn intercept_runs_workspace_wide() {
    let (status, out) = al(&["intercept"]);
    assert!(status.success(), "intercept failed with {status}:\n{out}");
    assert!(!out.trim().is_empty(), "intercept produced no output");
}
