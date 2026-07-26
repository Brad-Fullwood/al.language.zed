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

use al_analysis::queries::breaking_changes::analyze_breaking_changes;
use al_analysis::queries::bulk_fix::add_application_area;
use al_analysis::queries::dead_code::dead_code;
use al_analysis::queries::deps::build_dependency_graph;
use al_analysis::queries::impact::impact;
use al_analysis::queries::obsolescence::obsolescence_timeline;
use al_analysis::queries::profiler_hints::parse_profile;
use al_analysis::queries::sql_patterns::detect_sql_patterns;
use al_analysis::queries::suggest_event::{
    suggest_event, EventQuery, QuerySource, SuggestEventError,
};
use al_analysis::queries::upgrade::upgrade_report;
use al_project::project::AppManifest;
use al_symbols::model::{ObjectKind, SymbolEntry};
use al_types::AppDependency;
use al_workspace::Workspace;
use std::path::PathBuf;

#[test]
fn dead_code_empty_workspace_returns_empty() {
    // Negative: a workspace with no files must not panic and must
    // produce no findings.
    let ws = Workspace::new();
    let result = dead_code(&ws).unwrap();
    assert!(
        result.is_empty(),
        "empty workspace should yield no findings"
    );
}

#[test]
fn dead_code_workspace_with_parse_error_is_explicit() {
    // Negative: a file that fails to parse must reject the whole-project
    // report, not disappear from a plausible partial result.
    let ws = Workspace::new();
    ws.file_index.add_file(
        PathBuf::from("/src/broken.al"),
        // Missing closing paren — known parse-error trigger.
        "codeunit 50100 Broken\n{\n    procedure X(\n    begin\n    end;\n}\n".to_string(),
    );
    assert!(dead_code(&ws).is_err());
}

#[test]
fn impact_unknown_symbol_returns_empty() {
    // Negative: asking for impact of a symbol that doesn't exist in the
    // workspace must return an empty vec, not panic.
    let ws = Workspace::new();
    let result = impact(&ws, "SymbolThatDoesNotExist").unwrap();
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
    let result = impact(&ws, "").unwrap();
    assert!(result.is_empty(), "impact(\"\") must be empty");
}

#[test]
fn obsolescence_empty_workspace_returns_empty() {
    let ws = Workspace::new();
    let result = obsolescence_timeline(&ws).unwrap();
    assert!(result.is_empty());
}

#[test]
fn sql_patterns_empty_workspace_returns_empty() {
    let ws = Workspace::new();
    let result = detect_sql_patterns(&ws).unwrap();
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
    let result = detect_sql_patterns(&ws).unwrap();
    assert!(
        result.is_empty(),
        "table-only file should produce no SQL pattern findings, got {result:?}"
    );
}

#[test]
fn suggest_event_for_nonexistent_object_is_explicit() {
    // Negative: querying for an event source that doesn't exist must produce
    // a structured query error, not a plausible empty result or panic.
    let ws = Workspace::new();
    let q = EventQuery {
        source: QuerySource::Procedure {
            object: "Nope Codeunit".to_string(),
            procedure: Some("DoesNotExist".to_string()),
        },
        object_kind: None,
        filter_table: None,
        filter_field: None,
    };
    assert!(matches!(
        suggest_event(&ws, &q),
        Err(SuggestEventError::ObjectNotFound { .. })
    ));
}

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

fn dependency_manifest(dependencies: Vec<AppDependency>) -> AppManifest {
    AppManifest {
        id: "root-id".to_string(),
        name: "TestApp".to_string(),
        publisher: "Test".to_string(),
        version: "1.0.0.0".to_string(),
        dependencies,
        application: None,
        platform: None,
        runtime: None,
    }
}

#[test]
fn build_dependency_graph_empty_inputs_return_a_typed_root() {
    let manifest = dependency_manifest(Vec::new());
    let graph = build_dependency_graph(&manifest, &[], &[]);
    assert_eq!(graph.root_app.app_id, "root-id");
    assert!(graph.nodes.is_empty());
    assert!(graph.edges.is_empty());
}

#[test]
fn build_dependency_graph_missing_dependency_is_reported() {
    // Positive: an app.json that declares a dependency on a package that
    // isn't in the package list must appear in `missing`.
    let dependencies = vec![AppDependency {
        id: "deadbeef".to_string(),
        name: "Ghost".to_string(),
        publisher: "Microsoft".to_string(),
        version: "1.0.0.0".to_string(),
    }];
    let manifest = dependency_manifest(dependencies.clone());
    let graph = build_dependency_graph(&manifest, &dependencies, &[]);
    assert!(
        !graph.missing.is_empty(),
        "declared dependency with no matching package must be reported as missing, got {:?}",
        graph.missing
    );
}

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

#[test]
fn bulk_fix_add_application_area_nonexistent_dir_returns_err_or_empty() {
    // Negative: pointing the bulk-fix at a non-existent directory must
    // not panic; it should either return an error or an empty result.
    let result = add_application_area(
        std::path::Path::new("/this/path/definitely/does/not/exist/12345"),
        "All",
        true, // dry-run — never write to disk
    );
    if let Ok(r) = result {
        assert_eq!(r.changes_count, 0, "nonexistent dir must produce 0 changes");
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
