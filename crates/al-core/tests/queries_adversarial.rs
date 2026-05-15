//! Adversarial-input tests for advanced-analytics queries.
//!
//! Each of the 10 "lightly-tested" queries (breaking_changes, bulk_fix,
//! dead_code, deps, impact, obsolescence, profiler_hints, sql_patterns,
//! suggest_event, upgrade) has thorough happy-path inline unit tests.
//! What they don't all cover is the negative / degenerate / malformed
//! input paths. This file pins down those edge cases — anything that
//! could plausibly come from a real user workspace and crash the daemon
//! gets a regression test here.
//!
//! Conventions:
//!   - One test per query per failure mode.
//!   - Every test must assert *some* observable property (panic-free
//!     return is the minimum bar — see `*_does_not_panic` tests).
//!   - No real filesystem I/O except into `tempfile::TempDir`.

use al_core::queries::breaking_changes::analyze_breaking_changes;
use al_core::queries::bulk_fix::add_application_area;
use al_core::queries::dead_code::dead_code;
use al_core::queries::deps::build_dependency_graph;
use al_core::queries::impact::impact;
use al_core::queries::obsolescence::obsolescence_timeline;
use al_core::queries::profiler_hints::parse_profile;
use al_core::queries::sql_patterns::detect_sql_patterns;
use al_core::queries::suggest_event::{suggest_event, EventQuery, QuerySource};
use al_core::queries::upgrade::upgrade_report;
use al_core::symbols::model::{ObjectKind, SymbolEntry};
use al_core::workspace::Workspace;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// dead_code
// ---------------------------------------------------------------------------

#[test]
fn dead_code_empty_workspace_returns_empty() {
    // Negative: a workspace with no files must not panic and must
    // produce no findings.
    let ws = Workspace::new();
    let result = dead_code(&ws);
    assert!(
        result.is_empty(),
        "empty workspace should yield no findings"
    );
}

#[test]
fn dead_code_workspace_with_parse_error_does_not_panic() {
    // Negative: a file that fails to parse via tree-sitter must not
    // bring down the dead-code query (it should silently skip the file).
    let ws = Workspace::new();
    ws.file_index.add_file(
        PathBuf::from("/src/broken.al"),
        // Missing closing paren — known parse-error trigger.
        "codeunit 50100 Broken\n{\n    procedure X(\n    begin\n    end;\n}\n".to_string(),
    );
    let _ = dead_code(&ws);
}

// ---------------------------------------------------------------------------
// impact
// ---------------------------------------------------------------------------

#[test]
fn impact_unknown_symbol_returns_empty() {
    // Negative: asking for impact of a symbol that doesn't exist in the
    // workspace must return an empty vec, not panic.
    let ws = Workspace::new();
    let result = impact(&ws, "SymbolThatDoesNotExist");
    assert!(
        result.is_empty(),
        "impact() of an unknown symbol must be empty"
    );
}

#[test]
fn impact_empty_query_returns_empty() {
    // Negative: empty string is a legitimate degenerate input — must not
    // panic and must not match every symbol.
    let ws = Workspace::new();
    let result = impact(&ws, "");
    assert!(result.is_empty(), "impact(\"\") must be empty");
}

// ---------------------------------------------------------------------------
// obsolescence
// ---------------------------------------------------------------------------

#[test]
fn obsolescence_empty_workspace_returns_empty() {
    let ws = Workspace::new();
    let result = obsolescence_timeline(&ws);
    assert!(result.is_empty());
}

// ---------------------------------------------------------------------------
// sql_patterns
// ---------------------------------------------------------------------------

#[test]
fn sql_patterns_empty_workspace_returns_empty() {
    let ws = Workspace::new();
    let result = detect_sql_patterns(&ws);
    assert!(result.is_empty());
}

#[test]
fn sql_patterns_file_without_procedures_returns_empty() {
    // Negative: a table-only object with no procedures should produce
    // no SQL anti-pattern findings.
    let ws = Workspace::new();
    ws.file_index.add_file(
        PathBuf::from("/src/Just.al"),
        r#"table 50100 "Just"
{
    fields
    {
        field(1; "No"; Code[20]) { }
    }
}
"#
        .to_string(),
    );
    let result = detect_sql_patterns(&ws);
    assert!(
        result.is_empty(),
        "table-only file should produce no SQL pattern findings, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// suggest_event
// ---------------------------------------------------------------------------

#[test]
fn suggest_event_for_nonexistent_procedure_does_not_panic() {
    // Negative: querying for an event source that doesn't exist must
    // produce a structured empty result, not panic.
    let ws = Workspace::new();
    let q = EventQuery {
        source: QuerySource::Procedure {
            object: "Nope Codeunit".to_string(),
            procedure: Some("DoesNotExist".to_string()),
        },
        filter_table: None,
        filter_field: None,
    };
    let _ = suggest_event(&ws, &q);
}

// ---------------------------------------------------------------------------
// breaking_changes / upgrade
// ---------------------------------------------------------------------------

#[test]
fn analyze_breaking_changes_both_empty_returns_empty() {
    // Negative: comparing two empty symbol sets must produce zero
    // breaking changes.
    let result = analyze_breaking_changes(&[], &[]);
    assert!(
        result.is_empty(),
        "empty vs empty must yield no breaking changes, got {result:?}"
    );
}

#[test]
fn analyze_breaking_changes_identical_inputs_returns_empty() {
    // Positive: comparing a symbol set against itself must yield no
    // breaking changes.
    let sym = SymbolEntry {
        kind: ObjectKind::Table,
        id: 50100,
        name: "MyTable".to_string(),
        ..Default::default()
    };
    let result = analyze_breaking_changes(std::slice::from_ref(&sym), std::slice::from_ref(&sym));
    assert!(
        result.is_empty(),
        "identical baseline and current must yield zero breaking changes, got {result:?}"
    );
}

#[test]
fn analyze_breaking_changes_removed_object_is_reported() {
    // Positive: an object present in baseline but missing in current is a
    // breaking change.
    let sym = SymbolEntry {
        kind: ObjectKind::Codeunit,
        id: 50100,
        name: "WillBeRemoved".to_string(),
        ..Default::default()
    };
    let result = analyze_breaking_changes(std::slice::from_ref(&sym), &[]);
    assert!(
        !result.is_empty(),
        "removed object must show up as a breaking change"
    );
}

#[test]
fn upgrade_report_empty_inputs_returns_empty() {
    let result = upgrade_report(&[], &[]);
    assert!(result.is_empty());
}

// ---------------------------------------------------------------------------
// deps (build_dependency_graph)
// ---------------------------------------------------------------------------

#[test]
fn build_dependency_graph_malformed_app_json_does_not_panic() {
    // Negative: an app.json with invalid JSON must not panic — the
    // function falls back to a minimal/empty graph.
    let graph = build_dependency_graph("{ not valid json", &[]);
    assert!(graph.nodes.is_empty() || graph.root_app.name.is_empty());
}

#[test]
fn build_dependency_graph_empty_app_json_does_not_panic() {
    let graph = build_dependency_graph("", &[]);
    let _ = graph; // any well-typed return is fine
}

#[test]
fn build_dependency_graph_missing_dependency_is_reported() {
    // Positive: an app.json that declares a dependency on a package that
    // isn't in the package list must appear in `missing`.
    let app_json = r#"{
        "id": "0000",
        "name": "TestApp",
        "version": "1.0.0.0",
        "publisher": "Test",
        "dependencies": [
            { "id": "deadbeef", "name": "Ghost", "publisher": "Microsoft", "version": "1.0.0.0" }
        ]
    }"#;
    let graph = build_dependency_graph(app_json, &[]);
    assert!(
        !graph.missing.is_empty(),
        "declared dependency with no matching package must be reported as missing, got {:?}",
        graph.missing
    );
}

// ---------------------------------------------------------------------------
// profiler_hints (parse_profile)
// ---------------------------------------------------------------------------

#[test]
fn parse_profile_malformed_json_returns_err() {
    // Negative: parse_profile is the one entry point that returns Result;
    // malformed JSON must produce Err, not panic.
    let result = parse_profile("not json at all");
    assert!(result.is_err(), "malformed profile JSON must return Err");
}

#[test]
fn parse_profile_empty_string_returns_err() {
    let result = parse_profile("");
    assert!(result.is_err());
}

#[test]
fn parse_profile_empty_nodes_array_returns_ok_empty() {
    // Positive: a valid profile JSON with an empty `nodes` array must
    // parse cleanly and yield no hints.
    let result = parse_profile(r#"{"nodes": []}"#);
    assert!(
        result.is_ok(),
        "valid profile with empty nodes must parse cleanly, got {result:?}"
    );
    assert!(
        result.unwrap().is_empty(),
        "empty nodes array must yield no hints"
    );
}

#[test]
fn parse_profile_missing_nodes_array_returns_err() {
    // Negative: valid JSON without the required `nodes` array must
    // produce a structured error, not panic.
    let result = parse_profile(r#"{"startTime": 0, "endTime": 100}"#);
    assert!(
        result.is_err(),
        "profile JSON without 'nodes' must return Err"
    );
}

// ---------------------------------------------------------------------------
// bulk_fix (add_application_area)
// ---------------------------------------------------------------------------

#[test]
fn bulk_fix_add_application_area_nonexistent_dir_returns_err_or_empty() {
    // Negative: pointing the bulk-fix at a non-existent directory must
    // not panic; it should either return an error or an empty result.
    let result = add_application_area(
        std::path::Path::new("/this/path/definitely/does/not/exist/12345"),
        "All",
        true, // dry-run — never write to disk
    );
    match result {
        Ok(r) => assert_eq!(r.changes_count, 0, "nonexistent dir must produce 0 changes"),
        Err(_) => {} // also acceptable
    }
}

#[test]
fn bulk_fix_add_application_area_empty_dir_dry_run_is_zero_changes() {
    // Positive: dry-run over an empty temp directory must report zero
    // changes and zero modified files.
    let tmp = tempfile::tempdir().expect("create temp dir");
    let result = add_application_area(tmp.path(), "All", true).expect("bulk fix should succeed");
    assert_eq!(result.changes_count, 0);
    assert!(result.modified_files.is_empty());
    assert!(result.dry_run, "dry_run flag must be preserved in result");
}
