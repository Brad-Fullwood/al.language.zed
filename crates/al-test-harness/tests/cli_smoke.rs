//! Black-box smoke test for the `al-explorer` CLI surface, driven against the
//! bundled fixture project. Replaces the CLI half of the former `smoke.sh`.
//!
//! `al-explorer` is a thin client that auto-spawns the `al-lsp` daemon; it
//! resolves `al-lsp` first as a sibling of its own executable, so running the
//! `target/debug/al-explorer` binary finds `target/debug/al-lsp` with no PATH
//! setup. The first command pays the daemon cold-start; the rest are fast.

use std::process::Command;

use al_test_harness::{al_explorer_binary, test_project_dir};

/// Run `al-explorer <args>` in the fixture project; return (success, stdout+stderr).
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

fn assert_contains(args: &[&str], needle: &str) {
    let (ok, out) = al(args);
    assert!(
        ok,
        "`al-explorer {}` exited non-zero:\n{out}",
        args.join(" ")
    );
    assert!(
        out.contains(needle),
        "`al-explorer {}` output missing {needle:?}:\n{out}",
        args.join(" ")
    );
}

#[test]
fn version() {
    assert_contains(&["version"], "al 0.2");
}

#[test]
fn parse() {
    assert_contains(&["parse", "src/HelloWorld.al"], "0 errors");
}

#[test]
fn document_symbols() {
    assert_contains(&["symbols", "src/HelloWorld.al"], "Hello World");
}

#[test]
fn metrics() {
    assert_contains(&["metrics", "src/HelloWorld.al"], "cyclomatic");
}

#[test]
fn lint() {
    assert_contains(&["lint", "src/HelloWorld.al"], "issues");
}

#[test]
fn format_check() {
    assert_contains(
        &["format", "src/HelloWorld.al", "--check"],
        "already formatted",
    );
}

#[test]
fn search_workspace_index() {
    assert_contains(&["search", "Hello"], "Hello World");
}

#[test]
fn search_json() {
    assert_contains(&["search", "Hello", "--json"], "\"id\": 50100");
}

#[test]
fn tests_discovery() {
    assert_contains(&["tests"], "test codeunit");
}

#[test]
fn dead_code() {
    assert_contains(&["dead-code"], "DEAD CODE");
}

#[test]
fn sql_scan() {
    assert_contains(&["sql-scan"], "anti-pattern");
}

#[test]
fn diag() {
    assert_contains(&["diag"], "symbolCount");
}
