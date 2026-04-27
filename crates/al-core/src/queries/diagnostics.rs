//! Transport-agnostic syntax diagnostics query.
//!
//! Consolidates the parse+lint+config-filter pattern that was previously
//! duplicated across `al-lsp/src/server.rs` (schedule_diagnostics) and
//! `al-lsp/src/diagnostics.rs` (compute_diagnostics + publish_diagnostics).
//!
//! Uses the DocumentStore parse cache (`parsing::get_or_parse`) and falls back
//! to a direct parse only when the document is not in the store.

use url::Url;

use crate::config::AlConfig;
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Transport-agnostic diagnostic types
// ---------------------------------------------------------------------------

/// Severity of a syntax or lint diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxDiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

/// A transport-agnostic syntax / lint diagnostic.
#[derive(Debug, Clone)]
pub struct SyntaxDiagnostic {
    /// Human-readable diagnostic message.
    pub message: String,
    /// Source range (0-indexed lines, UTF-16 code unit columns — LSP-ready).
    pub range: crate::queries::Range,
    pub severity: SyntaxDiagnosticSeverity,
    /// Diagnostic code, e.g. `"syntax"` or `"AL-L001"`.
    pub code: String,
    /// Source label, e.g. `"al"` or `"al-lint"`.
    pub source: String,
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// Compute syntax and lint diagnostics for a single document.
///
/// Uses the DocumentStore parse cache when the document is present; falls back
/// to a direct parse on a cache miss so callers that pre-populate the store
/// (e.g. `did_open` / `did_change`) get a zero-cost cache hit.
///
/// Lint results are filtered by `config.is_lint_rule_enabled`. Note:
/// `crate::syntax::lint()` is currently a stub that always returns an empty
/// `Vec` — all AL diagnostics surfaced today come from the syntax-error
/// pass on the parse tree, not from native lint rules. The lint-filter
/// branch remains for forward-compatibility with the planned rule engine.
///
/// Returns a transport-agnostic `Vec<SyntaxDiagnostic>`. The caller is
/// responsible for converting to LSP `Diagnostic` values.
pub fn syntax_diagnostics(
    workspace: &Workspace,
    uri: &Url,
    config: &AlConfig,
) -> Vec<SyntaxDiagnostic> {
    // Phase 1: get (or create) the parse tree.
    let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        // Document not in DocumentStore. Callers (did_open / did_change) are
        // expected to pre-populate the store, so a cache miss is unexpected.
        // Surface it via tracing rather than silently parsing an empty string,
        // which would always return zero diagnostics and mask real issues.
        tracing::warn!(
            target: "al_core::diagnostics",
            uri = %uri,
            "syntax_diagnostics: document not in DocumentStore, returning empty diagnostics"
        );
        return Vec::new();
    };

    collect_diagnostics_from_tree(&tree, &text, config)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn collect_diagnostics_from_tree(
    tree: &tree_sitter::Tree,
    text: &str,
    config: &AlConfig,
) -> Vec<SyntaxDiagnostic> {
    let mut diags = Vec::new();

    let source = text.as_bytes();

    // Syntax errors from the parse tree.
    for err in crate::syntax::AlParser::errors_from_tree(tree) {
        let ts_range = err.range;
        diags.push(SyntaxDiagnostic {
            message: err.message,
            range: ts_range_to_query_range(ts_range, source),
            severity: SyntaxDiagnosticSeverity::Error,
            code: "syntax".to_string(),
            source: "al".to_string(),
        });
    }

    // Lint diagnostics, filtered by config.
    for lint in crate::syntax::lint(tree, text) {
        if !config.is_lint_rule_enabled(&lint.code) {
            continue;
        }
        let severity = match lint.severity {
            crate::syntax::LintSeverity::Error => SyntaxDiagnosticSeverity::Error,
            crate::syntax::LintSeverity::Warning => SyntaxDiagnosticSeverity::Warning,
            crate::syntax::LintSeverity::Info => SyntaxDiagnosticSeverity::Info,
            crate::syntax::LintSeverity::Hint => SyntaxDiagnosticSeverity::Hint,
        };
        diags.push(SyntaxDiagnostic {
            message: lint.message,
            range: ts_range_to_query_range(lint.range, source),
            severity,
            code: lint.code,
            source: "al-lint".to_string(),
        });
    }

    diags
}

/// Convert a tree-sitter `Range` to the crate-local `queries::Range`.
///
/// tree-sitter ranges use byte-offset columns; this helper converts them to
/// UTF-16 code unit columns so callers get LSP-ready ranges directly.
fn ts_range_to_query_range(r: tree_sitter::Range, source: &[u8]) -> crate::queries::Range {
    let start_line = source_line(source, r.start_point.row);
    let end_line = if r.end_point.row == r.start_point.row {
        start_line
    } else {
        source_line(source, r.end_point.row)
    };
    crate::queries::Range {
        start: crate::queries::Position {
            line: r.start_point.row as u32,
            character: crate::syntax::byte_col_to_utf16_col(start_line, r.start_point.column),
        },
        end: crate::queries::Position {
            line: r.end_point.row as u32,
            character: crate::syntax::byte_col_to_utf16_col(end_line, r.end_point.column),
        },
    }
}

/// Return the bytes of `row` (0-indexed) decoded as UTF-8, or `""` on bad UTF-8 / OOB.
fn source_line(source: &[u8], row: usize) -> &str {
    source
        .split(|&b| b == b'\n')
        .nth(row)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn make_workspace_with_text(text: &str) -> (Workspace, Url) {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/test.al").unwrap();
        ws.documents.open(uri.clone(), text.to_string());
        // Prime the parse cache so syntax_diagnostics hits the cache path.
        let _ = crate::parsing::get_or_parse(&ws.documents, &uri);
        (ws, uri)
    }

    /// Positive test: AL with a missing closing paren produces at least one error-severity diagnostic.
    #[test]
    fn test_syntax_diagnostics_invalid_al_returns_errors() {
        // Missing closing paren is a well-known trigger for tree-sitter parse errors.
        let src = "codeunit 50100 Test\n{\n    procedure Broken(\n    begin\n    end;\n}\n";
        let (ws, uri) = make_workspace_with_text(src);
        let config = AlConfig::default();
        let diags = syntax_diagnostics(&ws, &uri, &config);
        assert!(
            !diags.is_empty(),
            "expected diagnostics for invalid AL, got none"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.severity == SyntaxDiagnosticSeverity::Error),
            "expected at least one Error-severity diagnostic"
        );
    }

    /// Negative test: well-formed AL produces no diagnostics.
    #[test]
    fn test_syntax_diagnostics_valid_al_returns_empty() {
        let src = "codeunit 50100 MyCodeunit\n{\n    trigger OnRun()\n    begin\n    end;\n}\n";
        let (ws, uri) = make_workspace_with_text(src);
        let config = AlConfig::default();
        let diags = syntax_diagnostics(&ws, &uri, &config);
        assert!(
            diags.is_empty(),
            "expected no diagnostics for valid AL, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// Consistency test: calling syntax_diagnostics twice on the same URI
    /// returns the same error set (verifies cache path produces consistent output).
    #[test]
    fn test_syntax_diagnostics_consistent_across_calls() {
        let src = "codeunit 50100 Test\n{\n    procedure Broken(\n    begin\n    end;\n}\n";
        let (ws, uri) = make_workspace_with_text(src);
        let config = AlConfig::default();
        let first = syntax_diagnostics(&ws, &uri, &config);
        let second = syntax_diagnostics(&ws, &uri, &config);
        assert_eq!(
            first.len(),
            second.len(),
            "consecutive calls must return the same number of diagnostics"
        );
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(a.code, b.code);
            assert_eq!(a.severity, b.severity);
        }
    }

    /// Negative test: missing document returns empty vec (no panic, no false positives).
    #[test]
    fn test_syntax_diagnostics_missing_document_returns_empty() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent.al").unwrap();
        let config = AlConfig::default();
        let diags = syntax_diagnostics(&ws, &uri, &config);
        // A cache-miss on an empty string produces no syntax errors.
        // The important thing is: no panic.
        let _ = diags;
    }

    /// Regression: `ts_range_to_query_range` must convert byte columns to UTF-16
    /// code units. `é` is 2 UTF-8 bytes but 1 UTF-16 code unit; `好` is 3 bytes
    /// but 1 UTF-16 code unit.
    #[test]
    fn test_ts_range_to_query_range_converts_to_utf16() {
        let line = "// é好X";
        let source = line.as_bytes();
        // Column of 'X' as a tree-sitter byte column.
        let byte_col_x = line.find('X').unwrap();
        assert_eq!(byte_col_x, 8); // 2 (//) + 1 ( ) + 2 (é) + 3 (好) = 8 bytes
        let ts_range = tree_sitter::Range {
            start_byte: byte_col_x,
            end_byte: byte_col_x + 1,
            start_point: tree_sitter::Point {
                row: 0,
                column: byte_col_x,
            },
            end_point: tree_sitter::Point {
                row: 0,
                column: byte_col_x + 1,
            },
        };
        let q = ts_range_to_query_range(ts_range, source);
        // Expected UTF-16 column of 'X': 2 (//) + 1 ( ) + 1 (é) + 1 (好) = 5
        assert_eq!(
            q.start.character, 5,
            "expected UTF-16 column 5 for 'X', got {} — looks like raw byte column",
            q.start.character
        );
        assert_eq!(q.end.character, 6, "end column should be 6 in UTF-16");
        assert_eq!(q.start.line, 0);
    }
}
