//! Two-phase diagnostics — instant syntax + async analyzer.
//!
//! Phase 1 (instant): parse with tree-sitter, collect syntax errors, run native lint.
//! Phase 2 (async):   send to .NET SemanticBridge for CodeAnalysis diagnostics.

use std::path::PathBuf;

use al_core::syntax::AlParser;
use tower_lsp::lsp_types::*;

use crate::server::AlServer;

/// Return true if the URI points to a file inside the al-lsp symbol cache.
///
/// Cache files are virtual AL outlines extracted from .app packages — Zed can
/// navigate to them for go-to-definition, but they are not workspace files so
/// diagnostics must not be published for them (ISSUE-072).
pub(crate) fn is_cache_path(uri: &Url) -> bool {
    if let Ok(path) = uri.to_file_path() {
        let cache_root = al_core::symbols::virtual_file::cache_dir();
        path.starts_with(&cache_root)
    } else {
        false
    }
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

    // Phase 1: Instant syntax + lint.
    // Reuse the parse tree already cached by update_workspace_index to avoid a
    // redundant parse on every did_open / did_change (ISSUE-056 fix).
    {
        let parse_start = std::time::Instant::now();

        // get_or_parse returns the cached tree when the version matches, so when called
        // immediately after update_workspace_index this is a zero-cost cache hit.
        let tree = match al_core::parsing::get_or_parse(&server.workspace.documents, uri) {
            Some((_cached_text, t)) => t,
            None => {
                // Document not in store yet — parse directly and cache.
                // This path should not occur in normal LSP flows (did_open stores before calling us)
                // but is kept as a safe fallback.
                tracing::warn!(uri = %uri, "publish_diagnostics: document not in store, parsing directly");
                let result = AlParser::parse_quick(text);
                let version = server.workspace.documents.get_version(uri).unwrap_or(0);
                server.workspace.documents.cache_tree(uri, version, result.tree.clone());
                result.tree
            }
        };

        let parse_elapsed = parse_start.elapsed();

        // Extract errors from the (possibly cached) tree without re-parsing.
        let errors = AlParser::errors_from_tree(&tree);
        let error_count = errors.len();
        tracing::debug!(uri = %uri, error_count, parse_us = parse_elapsed.as_micros() as u64, "publish_diagnostics: diagnostics from tree");

        let source_bytes = text.as_bytes();

        // Syntax errors from tree-sitter
        for err in &errors {
            diagnostics.push(syntax_error_to_diagnostic(err, source_bytes));
        }

        // Native lint rules — filtered by per-rule config
        let lint_start = std::time::Instant::now();
        let lint_results = al_core::syntax::lint(&tree, text);
        let lint_elapsed = lint_start.elapsed();
        let lint_count = lint_results.len();
        tracing::debug!(uri = %uri, lint_count, lint_us = lint_elapsed.as_micros() as u64, "publish_diagnostics: linted");
        let config_guard = server.workspace.config.read().await;
        for lint in lint_results {
            if config_guard.is_lint_rule_enabled(&lint.code) {
                diagnostics.push(lint_to_diagnostic(&lint, source_bytes));
            }
        }
        drop(config_guard);
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

            let package_cache = if let Some(project) = server.workspace.project.read().await.as_ref() {
                project.packages_dir.clone()
            } else {
                PathBuf::from(".alpackages")
            };

            let analyzers = server
                .workspace
                .config
                .read()
                .await
                .code_analyzers
                .clone();
            let req = al_core::semantic_types::AnalyzeRequest {
                file: file_path,
                source: text.to_string(),
                analyzers,
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
///
/// `source` is the full file content as bytes, needed to convert tree-sitter byte-offset
/// columns to LSP UTF-16 code unit columns.
pub fn syntax_error_to_diagnostic(err: &al_core::syntax::SyntaxError, source: &[u8]) -> Diagnostic {
    Diagnostic {
        range: al_core::syntax::ts_range_to_lsp(&err.range, source),
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
pub fn lint_to_diagnostic(lint: &al_core::syntax::LintDiagnostic, source: &[u8]) -> Diagnostic {
    let severity = match lint.severity {
        al_core::syntax::LintSeverity::Error => DiagnosticSeverity::ERROR,
        al_core::syntax::LintSeverity::Warning => DiagnosticSeverity::WARNING,
        al_core::syntax::LintSeverity::Info => DiagnosticSeverity::INFORMATION,
        al_core::syntax::LintSeverity::Hint => DiagnosticSeverity::HINT,
    };

    Diagnostic {
        range: al_core::syntax::ts_range_to_lsp(&lint.range, source),
        severity: Some(severity),
        code: Some(NumberOrString::String(lint.code.clone())),
        source: Some("al-lint".to_string()),
        message: lint.message.clone(),
        ..Default::default()
    }
}

/// Convert a semantic diagnostic entry to an LSP Diagnostic.
pub fn semantic_to_diagnostic(entry: &al_core::semantic_types::DiagnosticEntry) -> Diagnostic {
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
        let src = "codeunit 50100 T { }";
        let err = al_core::syntax::SyntaxError {
            message: "Missing semicolon".to_string(),
            range: al_core::syntax::AlParser::parse_quick(src).tree.root_node().range(),
        };

        let diag = syntax_error_to_diagnostic(&err, src.as_bytes());
        assert_eq!(diag.message, "Missing semicolon");
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source, Some("al".to_string()));
    }

    #[test]
    fn test_lint_to_diagnostic_warning() {
        let src = "codeunit 50100 T { }";
        let lint = al_core::syntax::LintDiagnostic {
            code: "AL-L001".to_string(),
            message: "Empty begin..end block".to_string(),
            range: al_core::syntax::AlParser::parse_quick(src).tree.root_node().range(),
            severity: al_core::syntax::LintSeverity::Warning,
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
        let lint = al_core::syntax::LintDiagnostic {
            code: "AL-L006".to_string(),
            message: "Empty trigger".to_string(),
            range: al_core::syntax::AlParser::parse_quick(src).tree.root_node().range(),
            severity: al_core::syntax::LintSeverity::Hint,
        };

        let diag = lint_to_diagnostic(&lint, src.as_bytes());
        assert_eq!(diag.severity, Some(DiagnosticSeverity::HINT));
    }

    #[test]
    fn test_lint_to_diagnostic_info() {
        let src = "codeunit 50100 T { }";
        let lint = al_core::syntax::LintDiagnostic {
            code: "AL-L007".to_string(),
            message: "TODO comment".to_string(),
            range: al_core::syntax::AlParser::parse_quick(src).tree.root_node().range(),
            severity: al_core::syntax::LintSeverity::Info,
        };

        let diag = lint_to_diagnostic(&lint, src.as_bytes());
        assert_eq!(diag.severity, Some(DiagnosticSeverity::INFORMATION));
    }

    #[test]
    fn test_semantic_to_diagnostic() {
        let entry = al_core::semantic_types::DiagnosticEntry {
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
        let make = |sev: &str| al_core::semantic_types::DiagnosticEntry {
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
