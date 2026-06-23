//! Test results as diagnostics — T1503.
//!
//! Converts `TestCodeunitResult` (from the BC REST API) and static test
//! discovery (from `queries::tests`) into transport-agnostic `Diagnostic`
//! values. Failing tests become error-severity diagnostics pointing at the
//! procedure declaration line inside the source file.
//!
//! No network I/O here — callers supply the run results and the workspace.

use serde::{Deserialize, Serialize};

use crate::queries::tests::{collect_test_procedures, TestCodeunit};
use al_types::{TestCodeunitResult, TestStatus};
use al_workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// A single test-result diagnostic.
///
/// Maps a failed (or skipped) test method to its source location so that
/// the LSP layer can publish it via `textDocument/publishDiagnostics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestDiagnostic {
    /// Absolute file path containing the test procedure.
    pub file: String,
    /// 1-based line number of the procedure declaration.
    pub line: u32,
    /// Severity: failed tests → Error, skipped tests → Warning.
    pub severity: DiagnosticSeverity,
    /// Human-readable message (the error/assertion message from BC, or a
    /// generic "skipped" message).
    pub message: String,
    pub test_name: String,
    pub codeunit: String,
}

/// Convert test run results into diagnostics using a pre-discovered codeunit
/// list. Callers that run this in a hot loop (e.g. iterating many codeunits)
/// should call `crate::queries::tests::discover_tests` once up front and pass
/// the result here, avoiding an O(N) workspace scan per call.
pub fn results_to_diagnostics_with_codeunits(
    results: &[TestCodeunitResult],
    codeunits: &[TestCodeunit],
) -> Vec<TestDiagnostic> {
    let discovered_by_id: std::collections::HashMap<i32, &TestCodeunit> =
        codeunits.iter().map(|cu| (cu.id, cu)).collect();
    results_to_diagnostics_inner(results, &discovered_by_id)
}

/// Convert test run results into diagnostics, using the workspace to find
/// source locations for each failing test method.
///
/// Convenience wrapper — calls `discover_tests` once. For hot loops, prefer
/// `results_to_diagnostics_with_codeunits` and pass a cached codeunit list.
///
/// - Failing tests (`TestStatus::Fail`) → `DiagnosticSeverity::Error`
/// - Skipped tests (`TestStatus::Skip`) → `DiagnosticSeverity::Warning`
/// - Passing tests produce no diagnostics.
///
/// If the source file for a codeunit cannot be found in the workspace, the
/// diagnostic `file` is set to an empty string and `line` to 0.
pub fn results_to_diagnostics(
    results: &[TestCodeunitResult],
    workspace: &Workspace,
) -> Vec<TestDiagnostic> {
    let discovered = crate::queries::tests::discover_tests(workspace);
    results_to_diagnostics_with_codeunits(results, &discovered)
}

fn results_to_diagnostics_inner(
    results: &[TestCodeunitResult],
    discovered_by_id: &std::collections::HashMap<i32, &TestCodeunit>,
) -> Vec<TestDiagnostic> {
    let mut diagnostics = Vec::new();

    for result in results {
        let cu_info = discovered_by_id.get(&result.id);

        for method in &result.methods {
            if method.status == TestStatus::Pass {
                continue;
            }

            let (file, line) = if let Some(cu) = cu_info {
                // Find the exact line for this test procedure. If the method
                // is in the run results but was not seen during static
                // discovery (added after the last scan, stale cache, or a
                // discovery parse failure), fall back to line 0 — matching the
                // documented "unknown location" contract and the
                // codeunit-not-found case below. Line 1 would point at the
                // codeunit header, misleading jump-to-diagnostic.
                let proc_line = cu
                    .tests
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&method.name))
                    .map(|p| p.line)
                    .unwrap_or(0);
                (cu.file.clone(), proc_line)
            } else {
                (String::new(), 0)
            };

            let (severity, message) = match &method.status {
                TestStatus::Fail => {
                    let msg = method
                        .error
                        .clone()
                        .unwrap_or_else(|| "Test failed".to_string());
                    (DiagnosticSeverity::Error, msg)
                }
                TestStatus::Skip => (DiagnosticSeverity::Warning, "Test was skipped".to_string()),
                TestStatus::Pass => continue,
            };

            diagnostics.push(TestDiagnostic {
                file,
                line,
                severity,
                message,
                test_name: method.name.clone(),
                codeunit: result.name.clone(),
            });
        }
    }

    diagnostics
}

pub fn clear_diagnostics() -> Vec<TestDiagnostic> {
    Vec::new()
}

/// Build "not run" informational diagnostics for all discovered test
/// procedures in a workspace. Useful for showing which tests exist but
/// have no run results yet.
pub fn unrun_test_hints(workspace: &Workspace) -> Vec<TestDiagnostic> {
    let discovered = crate::queries::tests::discover_tests(workspace);
    let mut hints = Vec::new();

    for cu in &discovered {
        for proc in &cu.tests {
            hints.push(TestDiagnostic {
                file: cu.file.clone(),
                line: proc.line,
                severity: DiagnosticSeverity::Hint,
                message: format!("Test '{}' has not been run", proc.name),
                test_name: proc.name.clone(),
                codeunit: cu.name.clone(),
            });
        }
    }

    hints
}

pub fn group_by_file(
    diagnostics: Vec<TestDiagnostic>,
) -> std::collections::HashMap<String, Vec<TestDiagnostic>> {
    let mut map: std::collections::HashMap<String, Vec<TestDiagnostic>> =
        std::collections::HashMap::new();
    for d in diagnostics {
        map.entry(d.file.clone()).or_default().push(d);
    }
    map
}

/// Given a source string, find the 1-based line number of a procedure
/// declaration by name.  Used when the workspace file index is not available
/// (e.g., in tests).
pub fn find_proc_line(source: &str, proc_name: &str) -> Option<u32> {
    use al_syntax::AlParser;
    let result = AlParser::parse_quick(source);
    let root = result.tree.root_node();
    let source_bytes = source.as_bytes();
    let procs = collect_test_procedures(root, source_bytes);
    procs
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(proc_name))
        .map(|p| p.line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_types::{TestCodeunitResult, TestMethodResult, TestStatus};

    fn make_result(
        id: i32,
        name: &str,
        methods: Vec<(&str, TestStatus, Option<&str>)>,
    ) -> TestCodeunitResult {
        let method_results = methods
            .into_iter()
            .map(|(n, s, e)| TestMethodResult {
                name: n.to_string(),
                status: s,
                error: e.map(|s| s.to_string()),
                duration_ms: None,
            })
            .collect::<Vec<_>>();
        let total = method_results.len();
        let passed = method_results
            .iter()
            .filter(|m| m.status == TestStatus::Pass)
            .count();
        let failed = method_results
            .iter()
            .filter(|m| m.status == TestStatus::Fail)
            .count();
        let skipped = method_results
            .iter()
            .filter(|m| m.status == TestStatus::Skip)
            .count();
        TestCodeunitResult {
            name: name.to_string(),
            id,
            methods: method_results,
            total,
            passed,
            failed,
            skipped,
        }
    }

    #[test]
    fn passing_tests_produce_no_diagnostics() {
        let results = vec![make_result(
            50100,
            "MyTests",
            vec![
                ("TestA", TestStatus::Pass, None),
                ("TestB", TestStatus::Pass, None),
            ],
        )];
        let workspace = al_workspace::Workspace::new();
        let diags = results_to_diagnostics(&results, &workspace);
        assert!(diags.is_empty(), "No diagnostics for all-passing tests");
    }

    #[test]
    fn failed_test_produces_error_diagnostic() {
        let results = vec![make_result(
            50100,
            "MyTests",
            vec![
                ("TestOk", TestStatus::Pass, None),
                ("TestFail", TestStatus::Fail, Some("Assert failed: 1 != 2")),
            ],
        )];
        let workspace = al_workspace::Workspace::new();
        let diags = results_to_diagnostics(&results, &workspace);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].test_name, "TestFail");
        assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
        assert!(diags[0].message.contains("Assert failed"));
    }

    #[test]
    fn skipped_test_produces_warning_diagnostic() {
        let results = vec![make_result(
            50100,
            "MyTests",
            vec![("TestSkip", TestStatus::Skip, None)],
        )];
        let workspace = al_workspace::Workspace::new();
        let diags = results_to_diagnostics(&results, &workspace);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(diags[0].test_name, "TestSkip");
    }

    #[test]
    fn failed_test_without_message_gets_default() {
        let results = vec![make_result(
            50100,
            "MyTests",
            vec![("TestFail", TestStatus::Fail, None)],
        )];
        let workspace = al_workspace::Workspace::new();
        let diags = results_to_diagnostics(&results, &workspace);
        assert_eq!(diags[0].message, "Test failed");
    }

    #[test]
    fn clear_diagnostics_returns_empty() {
        let cleared = clear_diagnostics();
        assert!(cleared.is_empty());
    }

    #[test]
    fn group_by_file_groups_correctly() {
        let diagnostics = vec![
            TestDiagnostic {
                file: "FileA.al".to_string(),
                line: 10,
                severity: DiagnosticSeverity::Error,
                message: "fail".to_string(),
                test_name: "T1".to_string(),
                codeunit: "CU1".to_string(),
            },
            TestDiagnostic {
                file: "FileB.al".to_string(),
                line: 20,
                severity: DiagnosticSeverity::Warning,
                message: "skip".to_string(),
                test_name: "T2".to_string(),
                codeunit: "CU2".to_string(),
            },
            TestDiagnostic {
                file: "FileA.al".to_string(),
                line: 30,
                severity: DiagnosticSeverity::Error,
                message: "fail2".to_string(),
                test_name: "T3".to_string(),
                codeunit: "CU1".to_string(),
            },
        ];
        let grouped = group_by_file(diagnostics);
        assert_eq!(grouped["FileA.al"].len(), 2);
        assert_eq!(grouped["FileB.al"].len(), 1);
    }

    #[test]
    fn unrun_test_hints_returns_hints_for_empty_workspace() {
        let workspace = al_workspace::Workspace::new();
        let hints = unrun_test_hints(&workspace);
        assert!(hints.is_empty());
    }

    #[test]
    fn find_proc_line_locates_test_procedure() {
        let source = r#"codeunit 50100 "My Tests"
{
    Subtype = Test;

    [Test]
    procedure TestFoo()
    begin
    end;
}
"#;
        let line = find_proc_line(source, "TestFoo");
        assert!(line.is_some(), "Should find TestFoo");
        assert!(line.unwrap() > 0);
    }

    #[test]
    fn find_proc_line_returns_none_for_missing() {
        let source = r#"codeunit 50100 "My Tests"
{
    Subtype = Test;

    [Test]
    procedure TestFoo()
    begin
    end;
}
"#;
        let line = find_proc_line(source, "NoSuchProc");
        assert!(line.is_none());
    }

    /// Regression: a failing test method that is present in the run results
    /// but was NOT seen during static discovery (its codeunit was discovered,
    /// but the method is absent from `cu.tests`) must fall back to line 0 — the
    /// documented "unknown location" value — not line 1 (the codeunit header).
    #[test]
    fn discovered_codeunit_undiscovered_method_falls_back_to_line_zero() {
        let results = vec![make_result(
            50100,
            "MyTests",
            vec![("TestGhost", TestStatus::Fail, Some("boom"))],
        )];
        // Codeunit is discovered (id matches) but has no procedures recorded,
        // simulating a discovery gap (stale cache / parse failure / added late).
        let codeunits = vec![TestCodeunit {
            name: "MyTests".to_string(),
            id: 50100,
            file: "/src/MyTests.al".to_string(),
            tests: vec![],
        }];
        let diags = results_to_diagnostics_with_codeunits(&results, &codeunits);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].test_name, "TestGhost");
        assert_eq!(
            diags[0].line, 0,
            "Undiscovered method must fall back to line 0, not the codeunit header"
        );
        assert_eq!(diags[0].file, "/src/MyTests.al");
        assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn test_diagnostic_serializes() {
        let d = TestDiagnostic {
            file: "Tests.al".to_string(),
            line: 42,
            severity: DiagnosticSeverity::Error,
            message: "Test failed".to_string(),
            test_name: "TestSomething".to_string(),
            codeunit: "MyCU".to_string(),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("testName"));
        assert!(json.contains("TestSomething"));
        assert!(json.contains("error") || json.contains("Error"));
    }
}
