//! Test results as diagnostics.
//!
//! Converts `TestCodeunitResult` (from the BC REST API) and static test
//! discovery (from `queries::tests`) into transport-agnostic `Diagnostic`
//! values. Failing tests become error-severity diagnostics pointing at the
//! procedure declaration line inside the source file.
//!
//! No network I/O here — callers supply the run results and the workspace.

use serde::{Deserialize, Serialize};

use crate::queries::tests::TestCodeunit;
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
    /// 1-based line of the procedure declaration, or `None` when the run
    /// results name a test the static discovery did not see.
    ///
    /// A numeric sentinel does not survive the LSP boundary: `line` is 1-based
    /// and the conversion subtracts one, so both `0` and `1` became LSP line 0
    /// and the editor drew the squiggle on the codeunit header — the exact
    /// outcome the sentinel existed to avoid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
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
/// diagnostic `file` is empty and `line` is `None`.
pub fn results_to_diagnostics(
    results: &[TestCodeunitResult],
    workspace: &Workspace,
) -> Result<Vec<TestDiagnostic>, crate::queries::tests::TestQueryError> {
    let discovered = crate::queries::tests::discover_tests(workspace)?;
    Ok(results_to_diagnostics_with_codeunits(results, &discovered))
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

            // A method present in the run results but missing from static
            // discovery (added since the last index, or a file whose parse
            // failed) has no line. Say so rather than pick one.
            let (file, line) = if let Some(cu) = cu_info {
                let proc_line = cu
                    .tests
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&method.name))
                    .map(|p| p.line);
                (cu.file.clone(), proc_line)
            } else {
                (String::new(), None)
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
                failure_kind: None,
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

    /// The module's stated job is to point a failing test at its procedure
    /// declaration inside the source file. Every other test here builds an
    /// empty workspace, so discovery returns nothing, `file` comes out empty
    /// and `line` unset — the lookup could return a constant and they would
    /// all still pass. At the LSP boundary an empty `file` fails
    /// `Url::from_file_path`, so those diagnostics never reach the editor.
    #[test]
    fn a_failing_test_resolves_to_its_source_line() {
        let workspace = al_workspace::Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/src/MyTests.al"),
            "codeunit 50100 \"My Tests\"\n\
             {\n\
             \x20   Subtype = Test;\n\
             \n\
             \x20   [Test]\n\
             \x20   procedure TestOne()\n\
             \x20   begin\n\
             \x20   end;\n\
             \n\
             \x20   [Test]\n\
             \x20   procedure TestTwo()\n\
             \x20   begin\n\
             \x20   end;\n\
             }\n"
            .to_string(),
        );

        let results = vec![make_result(
            50100,
            "My Tests",
            vec![
                ("TestOne", TestStatus::Pass, None),
                ("TestTwo", TestStatus::Fail, Some("boom")),
            ],
        )];

        let diags = results_to_diagnostics(&results, &workspace).unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].test_name, "TestTwo");
        assert_eq!(diags[0].file, "/src/MyTests.al");
        assert_eq!(
            diags[0].line,
            Some(11),
            "line 10 is the [Test] attribute, line 11 is the procedure"
        );
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
        let diags = results_to_diagnostics(&results, &workspace).unwrap();
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
        let diags = results_to_diagnostics(&results, &workspace).unwrap();
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
        let diags = results_to_diagnostics(&results, &workspace).unwrap();
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
        let diags = results_to_diagnostics(&results, &workspace).unwrap();
        assert_eq!(diags[0].message, "Test failed");
    }

    #[test]
    fn group_by_file_groups_correctly() {
        let diagnostics = vec![
            TestDiagnostic {
                file: "FileA.al".to_string(),
                line: Some(10),
                severity: DiagnosticSeverity::Error,
                message: "fail".to_string(),
                test_name: "T1".to_string(),
                codeunit: "CU1".to_string(),
            },
            TestDiagnostic {
                file: "FileB.al".to_string(),
                line: Some(20),
                severity: DiagnosticSeverity::Warning,
                message: "skip".to_string(),
                test_name: "T2".to_string(),
                codeunit: "CU2".to_string(),
            },
            TestDiagnostic {
                file: "FileA.al".to_string(),
                line: Some(30),
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
    fn an_undiscovered_method_has_no_line() {
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
            test_initializers: vec![],
            test_cleanups: vec![],
        }];
        let diags = results_to_diagnostics_with_codeunits(&results, &codeunits);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].test_name, "TestGhost");
        assert_eq!(
            diags[0].line, None,
            "an undiscovered method has no line; a 0 sentinel became the codeunit header at the LSP boundary"
        );
        assert_eq!(diags[0].file, "/src/MyTests.al");
        assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn test_diagnostic_serializes() {
        let d = TestDiagnostic {
            file: "Tests.al".to_string(),
            line: Some(42),
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
