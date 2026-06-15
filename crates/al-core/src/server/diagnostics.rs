//! Two-phase diagnostics — instant syntax + async analyzer.
//!
//! Phase 1 (instant): parse with tree-sitter and collect syntax errors. Native
//!                     lint rules are not yet implemented — `crate::syntax::lint()`
//!                     returns an empty `Vec` — so this phase only surfaces
//!                     parse-error diagnostics today.
//! Phase 2 (async):   send to .NET SemanticBridge for CodeAnalysis diagnostics.

use std::path::PathBuf;

use tower_lsp::lsp_types::*;

use super::AlServer;

/// Wrap diagnostics in a full pull-diagnostics report.
pub(crate) fn full_diagnostic_report(items: Vec<Diagnostic>) -> DocumentDiagnosticReportResult {
    DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
        RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: None,
                items,
            },
        },
    ))
}

/// Return true if the URI points to a file inside the al-lsp symbol cache.
///
/// Cache files are virtual AL outlines extracted from .app packages — Zed can
/// navigate to them for go-to-definition, but they are not workspace files so
/// diagnostics must not be published for them (ISSUE-072).
pub(crate) fn is_cache_path(uri: &Url) -> bool {
    if let Ok(path) = uri.to_file_path() {
        let cache_root = crate::symbols::virtual_file::cache_dir();
        path.starts_with(&cache_root)
    } else {
        false
    }
}

/// Compute diagnostics for a document (syntax + lint + optional bridge).
///
/// Runs both Phase 1 (syntax errors + lint) and Phase 2 (bridge/semantic analysis)
/// and returns the combined `Vec<Diagnostic>`.
///
/// Used by the pull path (`textDocument/diagnostic`). The push path
/// (`publish_diagnostics`) keeps its own two-phase publish logic for instant
/// Phase-1 feedback.
pub(crate) async fn compute_diagnostics(
    server: &AlServer,
    uri: &Url,
    text: &str,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    // Phase 1: Instant syntax + lint via shared al-core query.
    {
        let config_guard = server.workspace.config.read().await;
        let syntax_diags =
            crate::queries::diagnostics::syntax_diagnostics(&server.workspace, uri, &config_guard);
        drop(config_guard);
        diagnostics.extend(syntax_diags.iter().map(syntax_diag_to_lsp));
    }

    // Phase 2: Async semantic analysis via .NET bridge.
    diagnostics.extend(run_semantic_analysis(server, uri, text).await);

    diagnostics
}

/// Run two-phase diagnostics and publish results to the client.
pub(crate) async fn publish_diagnostics(server: &AlServer, uri: &Url, text: &str) {
    // ISSUE-072: skip diagnostics for virtual symbol cache files — they are not
    // workspace files and Zed logs a warning for every publishDiagnostics on them.
    if is_cache_path(uri) {
        tracing::debug!(uri = %uri, "publish_diagnostics: skipping cache file");
        return;
    }

    tracing::debug!(uri = %uri, text_len = text.len(), "publish_diagnostics: entry");
    let mut diagnostics = Vec::new();

    // Phase 1: Instant syntax + lint via shared al-core query.
    // Reuse the parse tree already cached by update_workspace_index to avoid a
    // redundant parse on every did_open / did_change (ISSUE-056 fix).
    {
        let parse_start = std::time::Instant::now();
        let config_guard = server.workspace.config.read().await;
        let syntax_diags =
            crate::queries::diagnostics::syntax_diagnostics(&server.workspace, uri, &config_guard);
        drop(config_guard);
        let parse_elapsed = parse_start.elapsed();
        let error_count = syntax_diags.len();
        tracing::debug!(uri = %uri, error_count, parse_us = parse_elapsed.as_micros() as u64, "publish_diagnostics: diagnostics from query");
        diagnostics.extend(syntax_diags.iter().map(syntax_diag_to_lsp));
    }

    // Publish phase 1 immediately
    let phase1_count = diagnostics.len();
    tracing::debug!(uri = %uri, phase1_count, "publish_diagnostics: publishing phase 1");
    server
        .client
        .publish_diagnostics(uri.clone(), diagnostics.clone(), None)
        .await;

    // Phase 2: Async semantic analysis via .NET bridge.
    let semantic_diags = run_semantic_analysis(server, uri, text).await;
    if !semantic_diags.is_empty() {
        diagnostics.extend(semantic_diags);
        let total_count = diagnostics.len();
        tracing::debug!(uri = %uri, total_count, "publish_diagnostics: publishing phase 2");
        server
            .client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }
}

/// Run semantic analysis via .NET bridge if enabled. Returns diagnostics or empty vec.
///
/// Shared between `compute_diagnostics` (pull) and `publish_diagnostics` (push Phase 2).
async fn run_semantic_analysis(server: &AlServer, uri: &Url, text: &str) -> Vec<Diagnostic> {
    let (enable_analysis, bg_analysis) = {
        let cfg = server.workspace.config.read().await;
        (cfg.enable_code_analysis, cfg.background_code_analysis)
    };
    if !enable_analysis || !bg_analysis {
        tracing::debug!(uri = %uri, enable_analysis, bg_analysis, "semantic analysis disabled by config");
        return vec![];
    }

    let guard = match server.get_or_init_bridge().await {
        Some(g) => g,
        None => return vec![],
    };
    let bridge = match guard.as_ref() {
        Some(b) => b,
        None => return vec![],
    };

    let file_path = uri
        .to_file_path()
        .unwrap_or_else(|_| PathBuf::from(uri.path()));

    let config_guard = server.workspace.config.read().await;
    let analyzers = config_guard.code_analyzers.clone();
    let package_cache = if let Some(project) = server.workspace.project.read().await.as_ref() {
        project.packages_dir.clone()
    } else {
        PathBuf::from(".alpackages")
    };
    drop(config_guard);

    let req = crate::semantic::AnalyzeRequest {
        file: file_path,
        source: text.to_string(),
        analyzers,
        package_cache,
    };

    let semantic_start = std::time::Instant::now();
    match bridge.analyze(req).await {
        Ok(results) => {
            let semantic_elapsed = semantic_start.elapsed();
            tracing::debug!(uri = %uri, count = results.len(), elapsed_us = semantic_elapsed.as_micros() as u64, "semantic analysis complete");
            server.ensure_error_codes_loaded().await;

            results
                .into_iter()
                .map(|entry| {
                    let mut diag = semantic_to_diagnostic(&entry);
                    if let Some(desc) = server.error_code_description(&entry.code) {
                        if !diag.message.contains(&desc) {
                            diag.message = format!("{} — {}", diag.message, desc);
                        }
                    }
                    diag
                })
                .collect()
        }
        Err(error) => {
            let semantic_elapsed = semantic_start.elapsed();
            tracing::warn!(uri = %uri, %error, elapsed_us = semantic_elapsed.as_micros() as u64, "semantic analysis failed");

            // For persistent-failure states (Timeout / Poisoned), surface a
            // user-visible window/showMessage(WARNING) so the user knows the
            // semantic pipeline is broken. Throttle to once per session via
            // `should_report_semantic_failure` so opening many files with a
            // poisoned bridge does not spam the editor.
            // Cooldown is transient (recovers in ~30s) — do NOT notify the user.
            // Timeout and Poisoned indicate persistent breakage.
            let is_persistent = matches!(
                &error,
                crate::semantic::SemanticError::Timeout(_)
                    | crate::semantic::SemanticError::Poisoned
            );
            if is_persistent && server.should_report_semantic_failure() {
                if let Some(sink) = server.workspace.notify_sink.get() {
                    sink(&format!(
                        "AL semantic analysis is unavailable: {error}. \
                         Restart the editor to retry."
                    ));
                }
            }
            vec![]
        }
    }
}

/// Convert a transport-agnostic `SyntaxDiagnostic` (from al-core) to an LSP `Diagnostic`.
///
/// `SyntaxDiagnostic.range` already carries UTF-16 code unit columns — the
/// `queries::diagnostics::ts_range_to_query_range` helper runs `byte_col_to_utf16_col`
/// at the query layer. This is a thin shim that maps the already-converted
/// `queries::Range` to LSP `Range` without re-walking the source.
pub(crate) fn syntax_diag_to_lsp(
    diag: &crate::queries::diagnostics::SyntaxDiagnostic,
) -> Diagnostic {
    use crate::queries::diagnostics::SyntaxDiagnosticSeverity;

    let severity = match diag.severity {
        SyntaxDiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
        SyntaxDiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
        SyntaxDiagnosticSeverity::Info => DiagnosticSeverity::INFORMATION,
        SyntaxDiagnosticSeverity::Hint => DiagnosticSeverity::HINT,
    };

    Diagnostic {
        range: Range {
            start: Position {
                line: diag.range.start.line,
                character: diag.range.start.character,
            },
            end: Position {
                line: diag.range.end.line,
                character: diag.range.end.character,
            },
        },
        severity: Some(severity),
        code: Some(NumberOrString::String(diag.code.clone())),
        source: Some(diag.source.clone()),
        message: diag.message.clone(),
        ..Default::default()
    }
}

/// Convert a tree-sitter syntax error to an LSP Diagnostic.
///
/// `source` is the full file content as bytes, needed to convert tree-sitter byte-offset
/// columns to LSP UTF-16 code unit columns.
pub fn syntax_error_to_diagnostic(err: &crate::syntax::SyntaxError, source: &[u8]) -> Diagnostic {
    Diagnostic {
        range: crate::syntax_lsp::ts_range_to_lsp(&err.range, source),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String("syntax".to_string())),
        source: Some("al".to_string()),
        message: err.message.clone(),
        ..Default::default()
    }
}

/// Convert a native lint diagnostic to an LSP Diagnostic.
///
/// `source` is the full file content as bytes, needed to convert tree-sitter byte-offset
/// columns to LSP UTF-16 code unit columns.
pub fn lint_to_diagnostic(lint: &crate::syntax::LintDiagnostic, source: &[u8]) -> Diagnostic {
    let severity = match lint.severity {
        crate::syntax::LintSeverity::Error => DiagnosticSeverity::ERROR,
        crate::syntax::LintSeverity::Warning => DiagnosticSeverity::WARNING,
        crate::syntax::LintSeverity::Info => DiagnosticSeverity::INFORMATION,
        crate::syntax::LintSeverity::Hint => DiagnosticSeverity::HINT,
    };

    Diagnostic {
        range: crate::syntax_lsp::ts_range_to_lsp(&lint.range, source),
        severity: Some(severity),
        code: Some(NumberOrString::String(lint.code.clone())),
        source: Some("al-lint".to_string()),
        message: lint.message.clone(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// T1503: Test diagnostics — convert test results to LSP publishDiagnostics
// ---------------------------------------------------------------------------

/// Convert a `TestDiagnostic` (from al-core's test runner) to an LSP `Diagnostic`.
///
/// Lines in `TestDiagnostic` are 1-based; LSP positions are 0-based.
pub fn test_diag_to_lsp(td: &crate::queries::test_diagnostics::TestDiagnostic) -> Diagnostic {
    use crate::queries::test_diagnostics::DiagnosticSeverity as TDSev;

    let severity = match td.severity {
        TDSev::Error => DiagnosticSeverity::ERROR,
        TDSev::Warning => DiagnosticSeverity::WARNING,
        TDSev::Information => DiagnosticSeverity::INFORMATION,
        TDSev::Hint => DiagnosticSeverity::HINT,
    };

    let line = td.line.saturating_sub(1); // 1-based → 0-based
    Diagnostic {
        range: Range {
            start: Position { line, character: 0 },
            end: Position { line, character: 0 },
        },
        severity: Some(severity),
        code: Some(NumberOrString::String("AL-TEST".to_string())),
        source: Some("al-test-runner".to_string()),
        message: format!("[{}] {}", td.test_name, td.message),
        ..Default::default()
    }
}

/// Publish test-result diagnostics for all affected files.
///
/// Groups the flat list by file and calls `publishDiagnostics` once per file.
/// Pass an empty `diagnostics` slice to clear test diagnostics.
pub async fn publish_test_diagnostics(
    client: &tower_lsp::Client,
    diagnostics: &[crate::queries::test_diagnostics::TestDiagnostic],
) {
    use crate::queries::test_diagnostics::group_by_file;

    let grouped = group_by_file(diagnostics.to_vec());

    for (file, tds) in grouped {
        let uri = match Url::from_file_path(&file) {
            Ok(u) => u,
            Err(_) => {
                tracing::warn!(file = %file, "publish_test_diagnostics: cannot convert path to URI");
                continue;
            }
        };
        let lsp_diags: Vec<Diagnostic> = tds.iter().map(test_diag_to_lsp).collect();
        client.publish_diagnostics(uri, lsp_diags, None).await;
    }
}

/// Convert a semantic diagnostic entry to an LSP Diagnostic.
pub fn semantic_to_diagnostic(entry: &crate::semantic::DiagnosticEntry) -> Diagnostic {
    let severity = match entry.severity.to_lowercase().as_str() {
        "error" => DiagnosticSeverity::ERROR,
        "warning" => DiagnosticSeverity::WARNING,
        "info" | "information" => DiagnosticSeverity::INFORMATION,
        "hint" | "hidden" => DiagnosticSeverity::HINT,
        _ => DiagnosticSeverity::WARNING,
    };

    // Normalize the range so start <= end. A malformed bridge entry (e.g.
    // end_line < line) would otherwise produce a backwards LSP Range, which
    // violates the spec and can be silently discarded or mis-rendered by editors.
    let start = Position {
        line: entry.line.saturating_sub(1),
        character: entry.column.saturating_sub(1),
    };
    let end = Position {
        line: entry.end_line.saturating_sub(1),
        character: entry.end_column.saturating_sub(1),
    };
    let (start, end) = if (end.line, end.character) < (start.line, start.character) {
        (end, start)
    } else {
        (start, end)
    };

    Diagnostic {
        range: Range { start, end },
        severity: Some(severity),
        code: Some(NumberOrString::String(entry.code.clone())),
        source: Some("al-analyzer".to_string()),
        message: entry.message.clone(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syntax_error_to_diagnostic() {
        let src = "codeunit 50100 T { }";
        let err = crate::syntax::SyntaxError {
            message: "Missing semicolon".to_string(),
            range: crate::syntax::AlParser::parse_quick(src)
                .tree
                .root_node()
                .range(),
        };

        let diag = syntax_error_to_diagnostic(&err, src.as_bytes());
        assert_eq!(diag.message, "Missing semicolon");
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source, Some("al".to_string()));
    }

    #[test]
    fn test_lint_to_diagnostic_warning() {
        let src = "codeunit 50100 T { }";
        let lint = crate::syntax::LintDiagnostic {
            code: "AL-L001".to_string(),
            message: "Empty begin..end block".to_string(),
            range: crate::syntax::AlParser::parse_quick(src)
                .tree
                .root_node()
                .range(),
            severity: crate::syntax::LintSeverity::Warning,
        };

        let diag = lint_to_diagnostic(&lint, src.as_bytes());
        assert_eq!(diag.message, "Empty begin..end block");
        assert_eq!(diag.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("AL-L001".to_string()))
        );
        assert_eq!(diag.source, Some("al-lint".to_string()));
    }

    #[test]
    fn test_lint_to_diagnostic_hint() {
        let src = "codeunit 50100 T { }";
        let lint = crate::syntax::LintDiagnostic {
            code: "AL-L006".to_string(),
            message: "Empty trigger".to_string(),
            range: crate::syntax::AlParser::parse_quick(src)
                .tree
                .root_node()
                .range(),
            severity: crate::syntax::LintSeverity::Hint,
        };

        let diag = lint_to_diagnostic(&lint, src.as_bytes());
        assert_eq!(diag.severity, Some(DiagnosticSeverity::HINT));
    }

    #[test]
    fn test_lint_to_diagnostic_info() {
        let src = "codeunit 50100 T { }";
        let lint = crate::syntax::LintDiagnostic {
            code: "AL-L007".to_string(),
            message: "TODO comment".to_string(),
            range: crate::syntax::AlParser::parse_quick(src)
                .tree
                .root_node()
                .range(),
            severity: crate::syntax::LintSeverity::Info,
        };

        let diag = lint_to_diagnostic(&lint, src.as_bytes());
        assert_eq!(diag.severity, Some(DiagnosticSeverity::INFORMATION));
    }

    #[test]
    fn test_semantic_to_diagnostic() {
        let entry = crate::semantic::DiagnosticEntry {
            file: std::path::PathBuf::from("/src/test.al"),
            line: 10,
            column: 5,
            end_line: 10,
            end_column: 15,
            severity: "Error".to_string(),
            code: "AL0001".to_string(),
            message: "Syntax error".to_string(),
        };

        let diag = semantic_to_diagnostic(&entry);
        assert_eq!(diag.message, "Syntax error");
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("AL0001".to_string()))
        );
        assert_eq!(diag.source, Some("al-analyzer".to_string()));
        // Lines are 1-based in semantic, 0-based in LSP
        assert_eq!(diag.range.start.line, 9);
        assert_eq!(diag.range.start.character, 4);
    }

    #[test]
    fn semantic_to_diagnostic_normalizes_backwards_range() {
        // A malformed bridge entry with end before start must be normalized so
        // the resulting LSP Range satisfies start <= end (LSP spec requirement).
        let entry = crate::semantic::DiagnosticEntry {
            file: std::path::PathBuf::from("/src/test.al"),
            line: 10,
            column: 15,
            end_line: 5,
            end_column: 2,
            severity: "Error".to_string(),
            code: "AL0001".to_string(),
            message: "Backwards".to_string(),
        };

        let diag = semantic_to_diagnostic(&entry);
        let start = (diag.range.start.line, diag.range.start.character);
        let end = (diag.range.end.line, diag.range.end.character);
        assert!(
            start <= end,
            "Range must not be backwards: start={:?} end={:?}",
            start,
            end
        );
    }

    // T1503: test diagnostic conversion
    #[test]
    fn test_diag_fail_maps_to_error() {
        use crate::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
        let td = TestDiagnostic {
            file: "/src/Tests.al".to_string(),
            line: 10,
            severity: TDSev::Error,
            message: "Assert.AreEqual failed".to_string(),
            test_name: "TestSomething".to_string(),
            codeunit: "MyTests".to_string(),
        };
        let diag = test_diag_to_lsp(&td);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.range.start.line, 9); // 1-based → 0-based
        assert_eq!(diag.source, Some("al-test-runner".to_string()));
        assert!(diag.message.contains("TestSomething"));
        assert!(diag.message.contains("Assert.AreEqual failed"));
    }

    #[test]
    fn test_diag_skip_maps_to_warning() {
        use crate::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
        let td = TestDiagnostic {
            file: "/src/Tests.al".to_string(),
            line: 5,
            severity: TDSev::Warning,
            message: "Test was skipped".to_string(),
            test_name: "TestSkipped".to_string(),
            codeunit: "MyTests".to_string(),
        };
        let diag = test_diag_to_lsp(&td);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(diag.range.start.line, 4);
    }

    #[test]
    fn test_diag_line_zero_stays_zero() {
        use crate::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
        let td = TestDiagnostic {
            file: "".to_string(),
            line: 0,
            severity: TDSev::Error,
            message: "fail".to_string(),
            test_name: "T".to_string(),
            codeunit: "CU".to_string(),
        };
        let diag = test_diag_to_lsp(&td);
        assert_eq!(diag.range.start.line, 0); // saturating_sub(1) on 0 stays 0
    }

    fn sample_diag(msg: &str) -> Diagnostic {
        Diagnostic {
            message: msg.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn full_diagnostic_report_wraps_items() {
        let items = vec![sample_diag("a"), sample_diag("b")];
        let report = full_diagnostic_report(items);

        // Drill into the nested report variant and assert the items survived.
        match report {
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => {
                assert!(full.related_documents.is_none());
                assert!(full.full_document_diagnostic_report.result_id.is_none());
                let msgs: Vec<_> = full
                    .full_document_diagnostic_report
                    .items
                    .iter()
                    .map(|d| d.message.clone())
                    .collect();
                assert_eq!(msgs, vec!["a".to_string(), "b".to_string()]);
            }
            _ => panic!("expected a Full report variant"),
        }
    }

    #[test]
    fn full_diagnostic_report_empty_is_full_not_unchanged() {
        // An empty input must still produce a Full report (not an "unchanged"
        // report), otherwise the client would never clear stale diagnostics.
        let report = full_diagnostic_report(vec![]);
        match report {
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => {
                assert!(full.full_document_diagnostic_report.items.is_empty());
            }
            _ => panic!("expected a Full report variant even for empty items"),
        }
    }

    #[test]
    fn is_cache_path_true_for_cache_file() {
        let cache_root = crate::symbols::virtual_file::cache_dir();
        let file = cache_root.join("SomePackage").join("Table 27 Item.al");
        let uri = Url::from_file_path(&file).expect("cache path must convert to a file URL");
        assert!(
            is_cache_path(&uri),
            "URI under the symbol cache dir must be recognized as a cache path"
        );
    }

    #[test]
    fn is_cache_path_false_for_workspace_file() {
        // A normal project file outside the cache dir must NOT be treated as a
        // cache file, or diagnostics would be wrongly suppressed for it.
        let uri = Url::from_file_path("/home/dev/project/src/MyCodeunit.al")
            .expect("path must convert to a file URL");
        assert!(!is_cache_path(&uri));
    }

    #[test]
    fn is_cache_path_false_for_non_file_uri() {
        // A non-file URI (e.g. untitled:) cannot be a cache path.
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert!(!is_cache_path(&uri));
    }

    fn make_syntax_diag(
        sev: crate::queries::diagnostics::SyntaxDiagnosticSeverity,
    ) -> crate::queries::diagnostics::SyntaxDiagnostic {
        crate::queries::diagnostics::SyntaxDiagnostic {
            message: "boom".to_string(),
            range: crate::queries::Range {
                start: crate::queries::Position {
                    line: 3,
                    character: 7,
                },
                end: crate::queries::Position {
                    line: 3,
                    character: 12,
                },
            },
            severity: sev,
            code: "syntax".to_string(),
            source: "al".to_string(),
        }
    }

    #[test]
    fn syntax_diag_to_lsp_maps_fields_and_range() {
        use crate::queries::diagnostics::SyntaxDiagnosticSeverity;
        let diag = syntax_diag_to_lsp(&make_syntax_diag(SyntaxDiagnosticSeverity::Error));
        assert_eq!(diag.message, "boom");
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source, Some("al".to_string()));
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("syntax".to_string()))
        );
        // Range must be copied verbatim (already UTF-16 at the query layer).
        assert_eq!(diag.range.start.line, 3);
        assert_eq!(diag.range.start.character, 7);
        assert_eq!(diag.range.end.line, 3);
        assert_eq!(diag.range.end.character, 12);
    }

    #[test]
    fn syntax_diag_to_lsp_severity_mapping_all_variants() {
        use crate::queries::diagnostics::SyntaxDiagnosticSeverity;
        assert_eq!(
            syntax_diag_to_lsp(&make_syntax_diag(SyntaxDiagnosticSeverity::Error)).severity,
            Some(DiagnosticSeverity::ERROR)
        );
        assert_eq!(
            syntax_diag_to_lsp(&make_syntax_diag(SyntaxDiagnosticSeverity::Warning)).severity,
            Some(DiagnosticSeverity::WARNING)
        );
        assert_eq!(
            syntax_diag_to_lsp(&make_syntax_diag(SyntaxDiagnosticSeverity::Info)).severity,
            Some(DiagnosticSeverity::INFORMATION)
        );
        assert_eq!(
            syntax_diag_to_lsp(&make_syntax_diag(SyntaxDiagnosticSeverity::Hint)).severity,
            Some(DiagnosticSeverity::HINT)
        );
    }

    // -----------------------------------------------------------------------
    // semantic_to_diagnostic — additional edge / branch coverage
    // -----------------------------------------------------------------------

    #[test]
    fn semantic_to_diagnostic_unknown_severity_defaults_to_warning() {
        // Any unrecognized severity string must fall through to WARNING, not
        // panic or default to ERROR — this is the documented `_ =>` arm.
        let entry = crate::semantic::DiagnosticEntry {
            file: std::path::PathBuf::from("test.al"),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
            severity: "totally-bogus".to_string(),
            code: "X".to_string(),
            message: "m".to_string(),
        };
        assert_eq!(
            semantic_to_diagnostic(&entry).severity,
            Some(DiagnosticSeverity::WARNING)
        );
    }

    #[test]
    fn semantic_to_diagnostic_severity_is_case_insensitive() {
        let entry = crate::semantic::DiagnosticEntry {
            file: std::path::PathBuf::from("test.al"),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
            severity: "ERROR".to_string(),
            code: "X".to_string(),
            message: "m".to_string(),
        };
        assert_eq!(
            semantic_to_diagnostic(&entry).severity,
            Some(DiagnosticSeverity::ERROR)
        );
    }

    #[test]
    fn semantic_to_diagnostic_info_alias_maps_to_information() {
        // Both "info" and "information" must map to INFORMATION.
        for sev in ["info", "information", "INFO"] {
            let entry = crate::semantic::DiagnosticEntry {
                file: std::path::PathBuf::from("test.al"),
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 1,
                severity: sev.to_string(),
                code: "X".to_string(),
                message: "m".to_string(),
            };
            assert_eq!(
                semantic_to_diagnostic(&entry).severity,
                Some(DiagnosticSeverity::INFORMATION),
                "severity {sev:?} should map to INFORMATION"
            );
        }
    }

    #[test]
    fn semantic_to_diagnostic_clamps_line_and_column_zero() {
        // A bridge entry with line/column 0 (1-based) must saturate to 0 rather
        // than underflow when converted to 0-based LSP positions.
        let entry = crate::semantic::DiagnosticEntry {
            file: std::path::PathBuf::from("test.al"),
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 0,
            severity: "Warning".to_string(),
            code: "X".to_string(),
            message: "m".to_string(),
        };
        let diag = semantic_to_diagnostic(&entry);
        assert_eq!(diag.range.start.line, 0);
        assert_eq!(diag.range.start.character, 0);
        assert_eq!(diag.range.end.line, 0);
        assert_eq!(diag.range.end.character, 0);
    }

    #[test]
    fn semantic_to_diagnostic_keeps_forward_range_unchanged() {
        // A well-formed (start <= end) range must be preserved exactly, not
        // swapped — this guards the normalization branch in the false direction.
        let entry = crate::semantic::DiagnosticEntry {
            file: std::path::PathBuf::from("test.al"),
            line: 2,
            column: 3,
            end_line: 4,
            end_column: 9,
            severity: "Error".to_string(),
            code: "X".to_string(),
            message: "m".to_string(),
        };
        let diag = semantic_to_diagnostic(&entry);
        assert_eq!((diag.range.start.line, diag.range.start.character), (1, 2));
        assert_eq!((diag.range.end.line, diag.range.end.character), (3, 8));
    }

    // -----------------------------------------------------------------------
    // test_diag_to_lsp — remaining severity variants
    // -----------------------------------------------------------------------

    #[test]
    fn test_diag_information_and_hint_severities() {
        use crate::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
        let mk = |sev: TDSev| TestDiagnostic {
            file: "/src/Tests.al".to_string(),
            line: 3,
            severity: sev,
            message: "m".to_string(),
            test_name: "T".to_string(),
            codeunit: "CU".to_string(),
        };
        assert_eq!(
            test_diag_to_lsp(&mk(TDSev::Information)).severity,
            Some(DiagnosticSeverity::INFORMATION)
        );
        assert_eq!(
            test_diag_to_lsp(&mk(TDSev::Hint)).severity,
            Some(DiagnosticSeverity::HINT)
        );
    }

    #[test]
    fn test_diag_message_and_code_format() {
        use crate::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
        let td = TestDiagnostic {
            file: "/src/Tests.al".to_string(),
            line: 7,
            severity: TDSev::Error,
            message: "expected 1 got 2".to_string(),
            test_name: "MyTest".to_string(),
            codeunit: "MyTests".to_string(),
        };
        let diag = test_diag_to_lsp(&td);
        assert_eq!(diag.message, "[MyTest] expected 1 got 2");
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("AL-TEST".to_string()))
        );
    }

    #[test]
    fn test_semantic_severity_mapping() {
        let make = |sev: &str| crate::semantic::DiagnosticEntry {
            file: std::path::PathBuf::from("test.al"),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
            severity: sev.to_string(),
            code: "X".to_string(),
            message: "test".to_string(),
        };

        assert_eq!(
            semantic_to_diagnostic(&make("Error")).severity,
            Some(DiagnosticSeverity::ERROR)
        );
        assert_eq!(
            semantic_to_diagnostic(&make("Warning")).severity,
            Some(DiagnosticSeverity::WARNING)
        );
        assert_eq!(
            semantic_to_diagnostic(&make("Information")).severity,
            Some(DiagnosticSeverity::INFORMATION)
        );
        assert_eq!(
            semantic_to_diagnostic(&make("Hidden")).severity,
            Some(DiagnosticSeverity::HINT)
        );
    }
}
