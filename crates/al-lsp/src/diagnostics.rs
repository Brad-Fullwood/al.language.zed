//! Two-phase diagnostics — instant syntax + async analyzer.
//!
//! Phase 1 (instant): parse with tree-sitter, collect syntax errors, run native lint.
//! Phase 2 (async):   send to .NET SemanticBridge for CodeAnalysis diagnostics.

use std::path::PathBuf;

use al_syntax::AlParser;
use tower_lsp::lsp_types::*;

use crate::server::AlServer;

/// Run two-phase diagnostics and publish results to the client.
pub(crate) async fn publish_diagnostics(server: &AlServer, uri: &Url, text: &str) {
    tracing::debug!(uri = %uri, text_len = text.len(), "publish_diagnostics: entry");
    let mut diagnostics = Vec::new();

    // Phase 1: Instant syntax + lint
    {
        let parse_start = std::time::Instant::now();
        let result = AlParser::parse_quick(text);
        let parse_elapsed = parse_start.elapsed();
        let error_count = result.errors.len();
        tracing::debug!(uri = %uri, error_count, parse_us = parse_elapsed.as_micros() as u64, "publish_diagnostics: parsed");

        // Cache the tree for subsequent handler calls at this version
        let version = server.documents.get_version(uri).unwrap_or(0);
        server.documents.cache_tree(uri, version, result.tree.clone());

        // Syntax errors from tree-sitter
        for err in &result.errors {
            diagnostics.push(syntax_error_to_diagnostic(err));
        }

        // Native lint rules
        let lint_start = std::time::Instant::now();
        let lint_results = al_syntax::lint(&result.tree, text);
        let lint_elapsed = lint_start.elapsed();
        let lint_count = lint_results.len();
        tracing::debug!(uri = %uri, lint_count, lint_us = lint_elapsed.as_micros() as u64, "publish_diagnostics: linted");
        for lint in lint_results {
            diagnostics.push(lint_to_diagnostic(&lint));
        }
    }

    // Publish phase 1 immediately
    let phase1_count = diagnostics.len();
    tracing::debug!(uri = %uri, phase1_count, "publish_diagnostics: publishing phase 1");
    server
        .client
        .publish_diagnostics(uri.clone(), diagnostics.clone(), None)
        .await;

    // Phase 2: Async semantic analysis (lazy bridge init)
    if let Some(guard) = server.get_or_init_bridge().await {
        if let Some(bridge) = guard.as_ref() {
            let file_path = uri
                .to_file_path()
                .unwrap_or_else(|_| PathBuf::from(uri.path()));

            let package_cache = if let Some(project) = server.project.read().await.as_ref() {
                project.packages_dir.clone()
            } else {
                PathBuf::from(".alpackages")
            };

            let req = al_semantic::AnalyzeRequest {
                file: file_path,
                source: text.to_string(),
                analyzers: vec!["CodeCop".to_string()],
                package_cache,
            };

            let semantic_start = std::time::Instant::now();
            match bridge.analyze(req).await {
                Ok(results) => {
                    let semantic_elapsed = semantic_start.elapsed();
                    let semantic_count = results.len();
                    tracing::debug!(uri = %uri, semantic_count, semantic_us = semantic_elapsed.as_micros() as u64, "publish_diagnostics: semantic analysis complete");
                    // Load error codes if not yet cached (lazy, one-time)
                    server.ensure_error_codes_loaded().await;

                    for entry in results {
                        let mut diag = semantic_to_diagnostic(&entry);
                        // Enrich with error code description if available
                        if let Some(desc) = server.error_code_description(&entry.code) {
                            if !diag.message.contains(&desc) {
                                diag.message = format!("{} — {}", diag.message, desc);
                            }
                        }
                        diagnostics.push(diag);
                    }
                    let total_count = diagnostics.len();
                    tracing::debug!(uri = %uri, total_count, "publish_diagnostics: publishing phase 2");
                    server
                        .client
                        .publish_diagnostics(uri.clone(), diagnostics, None)
                        .await;
                }
                Err(error) => {
                    let semantic_elapsed = semantic_start.elapsed();
                    tracing::debug!(uri = %uri, %error, semantic_us = semantic_elapsed.as_micros() as u64, "publish_diagnostics: semantic analysis failed");
                }
            }
        }
    }
}

/// Convert a tree-sitter syntax error to an LSP Diagnostic.
pub fn syntax_error_to_diagnostic(err: &al_syntax::SyntaxError) -> Diagnostic {
    Diagnostic {
        range: al_syntax::ts_range_to_lsp(&err.range),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String("syntax".to_string())),
        source: Some("al".to_string()),
        message: err.message.clone(),
        ..Default::default()
    }
}

/// Convert a native lint diagnostic to an LSP Diagnostic.
pub fn lint_to_diagnostic(lint: &al_syntax::LintDiagnostic) -> Diagnostic {
    let severity = match lint.severity {
        al_syntax::LintSeverity::Error => DiagnosticSeverity::ERROR,
        al_syntax::LintSeverity::Warning => DiagnosticSeverity::WARNING,
        al_syntax::LintSeverity::Info => DiagnosticSeverity::INFORMATION,
        al_syntax::LintSeverity::Hint => DiagnosticSeverity::HINT,
    };

    Diagnostic {
        range: al_syntax::ts_range_to_lsp(&lint.range),
        severity: Some(severity),
        code: Some(NumberOrString::String(lint.code.clone())),
        source: Some("al-lint".to_string()),
        message: lint.message.clone(),
        ..Default::default()
    }
}

/// Convert a semantic diagnostic entry to an LSP Diagnostic.
pub fn semantic_to_diagnostic(entry: &al_semantic::DiagnosticEntry) -> Diagnostic {
    let severity = match entry.severity.to_lowercase().as_str() {
        "error" => DiagnosticSeverity::ERROR,
        "warning" => DiagnosticSeverity::WARNING,
        "info" | "information" => DiagnosticSeverity::INFORMATION,
        "hint" | "hidden" => DiagnosticSeverity::HINT,
        _ => DiagnosticSeverity::WARNING,
    };

    Diagnostic {
        range: Range {
            start: Position {
                line: entry.line.saturating_sub(1),
                character: entry.column.saturating_sub(1),
            },
            end: Position {
                line: entry.end_line.saturating_sub(1),
                character: entry.end_column.saturating_sub(1),
            },
        },
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
        let err = al_syntax::SyntaxError {
            message: "Missing semicolon".to_string(),
            range: tree_sitter::Range {
                start_byte: 0,
                end_byte: 5,
                start_point: tree_sitter::Point { row: 0, column: 0 },
                end_point: tree_sitter::Point { row: 0, column: 5 },
            },
        };

        let diag = syntax_error_to_diagnostic(&err);
        assert_eq!(diag.message, "Missing semicolon");
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source, Some("al".to_string()));
        assert_eq!(diag.range.start.line, 0);
        assert_eq!(diag.range.start.character, 0);
    }

    #[test]
    fn test_lint_to_diagnostic_warning() {
        let lint = al_syntax::LintDiagnostic {
            code: "AL-L001".to_string(),
            message: "Empty begin..end block".to_string(),
            range: tree_sitter::Range {
                start_byte: 10,
                end_byte: 30,
                start_point: tree_sitter::Point { row: 3, column: 4 },
                end_point: tree_sitter::Point { row: 5, column: 8 },
            },
            severity: al_syntax::LintSeverity::Warning,
        };

        let diag = lint_to_diagnostic(&lint);
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
        let lint = al_syntax::LintDiagnostic {
            code: "AL-L006".to_string(),
            message: "Empty trigger".to_string(),
            range: tree_sitter::Range {
                start_byte: 0,
                end_byte: 10,
                start_point: tree_sitter::Point { row: 0, column: 0 },
                end_point: tree_sitter::Point { row: 1, column: 0 },
            },
            severity: al_syntax::LintSeverity::Hint,
        };

        let diag = lint_to_diagnostic(&lint);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::HINT));
    }

    #[test]
    fn test_lint_to_diagnostic_info() {
        let lint = al_syntax::LintDiagnostic {
            code: "AL-L007".to_string(),
            message: "TODO comment".to_string(),
            range: tree_sitter::Range {
                start_byte: 0,
                end_byte: 10,
                start_point: tree_sitter::Point { row: 0, column: 0 },
                end_point: tree_sitter::Point { row: 0, column: 10 },
            },
            severity: al_syntax::LintSeverity::Info,
        };

        let diag = lint_to_diagnostic(&lint);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::INFORMATION));
    }

    #[test]
    fn test_semantic_to_diagnostic() {
        let entry = al_semantic::DiagnosticEntry {
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
    fn test_semantic_severity_mapping() {
        let make = |sev: &str| al_semantic::DiagnosticEntry {
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
