//! Transport-agnostic syntax and native semantic diagnostics query.
//!
//! Consolidates the parse+lint+config-filter pattern that was previously
//! duplicated across `al-lsp/src/server.rs` (schedule_diagnostics) and
//! `al-lsp/src/diagnostics.rs` (compute_diagnostics + publish_diagnostics).
//!
//! Uses the DocumentStore parse cache (`parsing::get_or_parse`) and falls back
//! to a direct parse only when the document is not in the store.

use url::Url;

use al_project::config::AlConfig;
use al_workspace::Workspace;

pub const ARCHITECTURE_LAYER_VIOLATION: &str = "AL-NL009";

#[derive(Debug, Clone)]
pub struct NativeWorkspaceLintRuleInfo {
    pub code: &'static str,
    pub name: &'static str,
    pub severity: SyntaxDiagnosticSeverity,
    pub description: &'static str,
}

const WORKSPACE_LINT_RULES: &[NativeWorkspaceLintRuleInfo] = &[
    NativeWorkspaceLintRuleInfo {
        code: super::obsolete_usage::OBSOLETE_USAGE,
        name: "obsolete-symbol-usage",
        severity: SyntaxDiagnosticSeverity::Warning,
        description: "A call resolves unambiguously by name to obsolete workspace or package \
                      procedures.",
    },
    NativeWorkspaceLintRuleInfo {
        code: ARCHITECTURE_LAYER_VIOLATION,
        name: "architecture-layer-violation",
        severity: SyntaxDiagnosticSeverity::Warning,
        description: "A built-in or .alarch.json architecture boundary is violated.",
    },
];

#[must_use]
pub fn native_workspace_lint_rules() -> &'static [NativeWorkspaceLintRuleInfo] {
    WORKSPACE_LINT_RULES
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxDiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone)]
pub struct SyntaxDiagnostic {
    pub message: String,
    /// Source range (0-indexed lines, UTF-16 code unit columns — LSP-ready).
    pub range: crate::queries::Range,
    pub severity: SyntaxDiagnosticSeverity,
    /// Diagnostic code, e.g. `"syntax"` or `"AL-NL001"`.
    pub code: String,
    /// Source label, e.g. `"al"` or `"al-lint"`.
    pub source: String,
}

/// Compute syntax and lint diagnostics for a single document.
///
/// Uses the DocumentStore parse cache when the document is present; falls back
/// to a direct parse on a cache miss so callers that pre-populate the store
/// (e.g. `did_open` / `did_change`) get a zero-cost cache hit.
///
/// Lint results are filtered by `config.is_lint_rule_enabled`. The result joins
/// file-local `al_syntax` rules with project-semantic and resolved
/// call/event-stack checks from the shared native workspace engine.
///
/// Returns a transport-agnostic `Vec<SyntaxDiagnostic>`. The caller is
/// responsible for converting to LSP `Diagnostic` values.
pub fn syntax_diagnostics(
    workspace: &Workspace,
    uri: &Url,
    config: &AlConfig,
) -> Vec<SyntaxDiagnostic> {
    syntax_diagnostics_at_root(workspace, uri, config, None)
}

pub fn syntax_diagnostics_at_root(
    workspace: &Workspace,
    uri: &Url,
    config: &AlConfig,
    project_root: Option<&std::path::Path>,
) -> Vec<SyntaxDiagnostic> {
    let Some((text, tree)) = al_source::parsing::get_or_parse(&workspace.documents, uri) else {
        return vec![SyntaxDiagnostic {
            message: format!(
                "Diagnostics could not be computed because document '{uri}' is not loaded"
            ),
            range: crate::queries::Range::default(),
            severity: SyntaxDiagnosticSeverity::Error,
            code: "AL-NL000".to_string(),
            source: "al-native".to_string(),
        }];
    };

    let mut diagnostics = collect_diagnostics_from_tree(&tree, &text, config);
    if let Ok(path) = uri.to_file_path() {
        diagnostics.extend(
            native_workspace_diagnostics_at_root(workspace, config, project_root)
                .into_iter()
                .filter_map(|(finding_path, diagnostic)| {
                    (finding_path == path).then_some(diagnostic)
                }),
        );
    }
    diagnostics
}

/// Compute syntax/lint diagnostics for every indexed workspace file.
///
/// Walks the [`FileIndex`](al_source::file_index::FileIndex) cached parse trees
/// (`iter_parsed`) and runs the **same** `collect_diagnostics_from_tree` pass
/// used for a single document, so a file's workspace-scope diagnostics match its
/// per-document diagnostics exactly. Crucially this reuses each file's already
/// cached tree — no file is re-parsed — which keeps the project-wide pass cheap
/// even on large workspaces.
///
/// Background (never-opened) files and open documents both appear here because
/// `on_document_change` mirrors every open/edited buffer into the file index.
/// Callers that also want bridge/semantic diagnostics for open documents should
/// layer those on top (see the LSP `workspace/diagnostic` handler).
///
/// Returns `(path, diagnostics)` for every indexed file, including files with no
/// diagnostics — the caller decides whether to drop empty entries.
pub fn workspace_syntax_diagnostics(
    workspace: &Workspace,
    config: &AlConfig,
) -> Result<Vec<(std::path::PathBuf, Vec<SyntaxDiagnostic>)>, super::WorkspaceQueryError> {
    workspace_syntax_diagnostics_at_root(workspace, config, None)
}

pub fn workspace_syntax_diagnostics_at_root(
    workspace: &Workspace,
    config: &AlConfig,
    project_root: Option<&std::path::Path>,
) -> Result<Vec<(std::path::PathBuf, Vec<SyntaxDiagnostic>)>, super::WorkspaceQueryError> {
    let sources = crate::workspace_sources::coherent_snapshot(workspace)
        .map_err(super::WorkspaceQueryError::from)?;
    let mut results: Vec<_> = sources
        .into_iter()
        .map(|source| {
            let diags = collect_diagnostics_from_tree(&source.tree, &source.text, config);
            (source.path, diags)
        })
        .collect();

    let mut native_by_file: std::collections::HashMap<std::path::PathBuf, Vec<SyntaxDiagnostic>> =
        std::collections::HashMap::new();
    for (path, diagnostic) in native_workspace_diagnostics_at_root(workspace, config, project_root)
    {
        native_by_file.entry(path).or_default().push(diagnostic);
    }
    for (path, diagnostics) in &mut results {
        if let Some(mut native) = native_by_file.remove(path) {
            diagnostics.append(&mut native);
        }
    }
    // A semantic finding should normally point at an indexed file already in
    // `results`; retain it even if a concurrently removed parse entry made the
    // local syntax pass miss that path.
    results.extend(native_by_file);
    Ok(results)
}

/// Compute every native workspace-level semantic/transaction diagnostic.
///
/// This is the bridge-free validation layer shared by editor diagnostics and
/// native build gating. It combines object/project checks (`AL-NC*`) with the
/// resolved call/event-stack transaction rules (`AL-NL003/4`).
#[must_use]
pub fn native_workspace_diagnostics(
    workspace: &Workspace,
    config: &AlConfig,
) -> Vec<(std::path::PathBuf, SyntaxDiagnostic)> {
    native_workspace_diagnostics_at_root(workspace, config, None)
}

pub fn native_workspace_diagnostics_at_root(
    workspace: &Workspace,
    config: &AlConfig,
    project_root: Option<&std::path::Path>,
) -> Vec<(std::path::PathBuf, SyntaxDiagnostic)> {
    if !config.enable_native_lint {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    for finding in super::native_check::native_semantic_checks(workspace, project_root) {
        if !config.is_lint_rule_enabled(finding.code) {
            continue;
        }
        let path = finding.file.as_deref().map_or_else(
            || diagnostic_anchor_path(workspace, project_root),
            std::path::PathBuf::from,
        );
        let location = native_finding_range(workspace, &path);
        let severity = match finding.severity {
            super::native_check::NativeSeverity::Error => SyntaxDiagnosticSeverity::Error,
            super::native_check::NativeSeverity::Warning => SyntaxDiagnosticSeverity::Warning,
        };
        let (range, severity, message) = match location {
            Ok(range) => (range, severity, finding.message),
            Err(error) => (
                crate::queries::Range::default(),
                SyntaxDiagnosticSeverity::Error,
                format!(
                    "{} Location integrity failure for '{}': {error}",
                    finding.message,
                    path.display()
                ),
            ),
        };
        diagnostics.push((
            path,
            SyntaxDiagnostic {
                message,
                range,
                severity,
                code: finding.code.to_string(),
                source: "al-native".to_string(),
            },
        ));
    }

    let transaction_rules_enabled = [
        super::transaction_lint::COMMIT_AFTER_DATABASE_CHANGE,
        super::transaction_lint::DATABASE_WRITE_IN_TRY_STACK,
    ]
    .iter()
    .any(|code| config.is_lint_rule_enabled(code));
    if transaction_rules_enabled {
        match super::transaction_lint::transaction_lints(workspace) {
            Ok(findings) => {
                for finding in findings {
                    if !config.is_lint_rule_enabled(finding.code) {
                        continue;
                    }
                    let severity = match finding.severity {
                        super::transaction_lint::WorkspaceLintSeverity::Error => {
                            SyntaxDiagnosticSeverity::Error
                        }
                        super::transaction_lint::WorkspaceLintSeverity::Warning => {
                            SyntaxDiagnosticSeverity::Warning
                        }
                        super::transaction_lint::WorkspaceLintSeverity::Info => {
                            SyntaxDiagnosticSeverity::Info
                        }
                        super::transaction_lint::WorkspaceLintSeverity::Hint => {
                            SyntaxDiagnosticSeverity::Hint
                        }
                    };
                    diagnostics.push((
                        finding.file,
                        SyntaxDiagnostic {
                            message: finding.message,
                            range: finding.range,
                            severity,
                            code: finding.code.to_string(),
                            source: "al-native".to_string(),
                        },
                    ));
                }
            }
            Err(error) => {
                diagnostics.push((
                    diagnostic_anchor_path(workspace, project_root),
                    SyntaxDiagnostic {
                        message: format!(
                            "Resolved transaction analysis could not be completed: {error}"
                        ),
                        range: crate::queries::Range::default(),
                        severity: SyntaxDiagnosticSeverity::Error,
                        code: "AL-NL000".to_string(),
                        source: "al-native".to_string(),
                    },
                ));
            }
        }
    }

    if config.is_lint_rule_enabled(super::obsolete_usage::OBSOLETE_USAGE) {
        match super::obsolete_usage::obsolete_usages(workspace) {
            Ok(findings) => {
                diagnostics.extend(findings.into_iter().map(|finding| {
                    (
                        finding.file,
                        SyntaxDiagnostic {
                            message: finding.message,
                            range: finding.range,
                            severity: SyntaxDiagnosticSeverity::Warning,
                            code: super::obsolete_usage::OBSOLETE_USAGE.to_string(),
                            source: "al-native".to_string(),
                        },
                    )
                }));
            }
            Err(error) => {
                diagnostics.push((
                    diagnostic_anchor_path(workspace, project_root),
                    SyntaxDiagnostic {
                        message: format!("Obsolete-usage analysis incomplete: {error}"),
                        range: crate::queries::Range::default(),
                        severity: SyntaxDiagnosticSeverity::Error,
                        code: "AL-NL000".to_string(),
                        source: "al-native".to_string(),
                    },
                ));
            }
        }
    }

    if config.is_lint_rule_enabled(ARCHITECTURE_LAYER_VIOLATION) {
        let mut arch_config = super::arch_lint::ArchConfig::default();
        if let Some(root) = project_root {
            let path = root.join(".alarch.json");
            match read_arch_config(&path) {
                Ok(Some(config)) => arch_config = config,
                Ok(None) => {}
                Err(message) => {
                    diagnostics.push((
                        path,
                        SyntaxDiagnostic {
                            message: format!("Invalid architecture configuration: {message}"),
                            range: crate::queries::Range::default(),
                            severity: SyntaxDiagnosticSeverity::Error,
                            code: ARCHITECTURE_LAYER_VIOLATION.to_string(),
                            source: "al-native".to_string(),
                        },
                    ));
                    // Built-in rules remain useful, but the invalid custom file
                    // must block native builds and surface in the editor rather
                    // than being treated as an empty configuration.
                }
            }
        }
        match super::arch_lint::arch_lint(workspace, &arch_config) {
            Ok(violations) => {
                for violation in violations {
                    let file = std::path::PathBuf::from(&violation.file);
                    let Some(line) = violation.line.checked_sub(1) else {
                        diagnostics.push((
                            file,
                            SyntaxDiagnostic {
                                message: format!(
                                    "[{}] Architecture analysis produced an invalid zero source line: {}",
                                    violation.rule_id, violation.message
                                ),
                                range: crate::queries::Range::default(),
                                severity: SyntaxDiagnosticSeverity::Error,
                                code: "AL-NL000".to_string(),
                                source: "al-native".to_string(),
                            },
                        ));
                        continue;
                    };
                    let end_character = match workspace.file_index.get_cached_parse(&file) {
                        Some((text, _)) => match text.lines().nth(line as usize) {
                            Some(source) => match u32::try_from(source.encode_utf16().count()) {
                                Ok(length) => length,
                                Err(_) => {
                                    diagnostics.push((
                                        file,
                                        SyntaxDiagnostic {
                                            message: format!(
                                                "[{}] Architecture diagnostic source line exceeds the LSP position range: {}",
                                                violation.rule_id, violation.message
                                            ),
                                            range: crate::queries::Range::default(),
                                            severity: SyntaxDiagnosticSeverity::Error,
                                            code: "AL-NL000".to_string(),
                                            source: "al-native".to_string(),
                                        },
                                    ));
                                    continue;
                                }
                            },
                            None => {
                                diagnostics.push((
                                    file,
                                    SyntaxDiagnostic {
                                        message: format!(
                                            "[{}] Architecture diagnostic line {} is outside the indexed source: {}",
                                            violation.rule_id, violation.line, violation.message
                                        ),
                                        range: crate::queries::Range::default(),
                                        severity: SyntaxDiagnosticSeverity::Error,
                                        code: "AL-NL000".to_string(),
                                        source: "al-native".to_string(),
                                    },
                                ));
                                continue;
                            }
                        },
                        None => {
                            diagnostics.push((
                                file,
                                SyntaxDiagnostic {
                                    message: format!(
                                        "[{}] Architecture diagnostic source is missing from the parse cache: {}",
                                        violation.rule_id, violation.message
                                    ),
                                    range: crate::queries::Range::default(),
                                    severity: SyntaxDiagnosticSeverity::Error,
                                    code: "AL-NL000".to_string(),
                                    source: "al-native".to_string(),
                                },
                            ));
                            continue;
                        }
                    };
                    diagnostics.push((
                        file,
                        SyntaxDiagnostic {
                            message: format!("[{}] {}", violation.rule_id, violation.message),
                            range: crate::queries::Range {
                                start: crate::queries::Position { line, character: 0 },
                                end: crate::queries::Position {
                                    line,
                                    character: end_character,
                                },
                            },
                            severity: SyntaxDiagnosticSeverity::Warning,
                            code: ARCHITECTURE_LAYER_VIOLATION.to_string(),
                            source: "al-native".to_string(),
                        },
                    ));
                }
            }
            Err(error) => {
                diagnostics.push((
                    diagnostic_anchor_path(workspace, project_root),
                    SyntaxDiagnostic {
                        message: format!("Architecture analysis incomplete: {error}"),
                        range: crate::queries::Range::default(),
                        severity: SyntaxDiagnosticSeverity::Error,
                        code: ARCHITECTURE_LAYER_VIOLATION.to_string(),
                        source: "al-native".to_string(),
                    },
                ));
            }
        }
    }

    diagnostics.sort_by(|(path_a, a), (path_b, b)| {
        path_a
            .cmp(path_b)
            .then(a.range.start.line.cmp(&b.range.start.line))
            .then(a.range.start.character.cmp(&b.range.start.character))
            .then(a.code.cmp(&b.code))
    });
    diagnostics
}

fn diagnostic_anchor_path(
    workspace: &Workspace,
    project_root: Option<&std::path::Path>,
) -> std::path::PathBuf {
    if let Some(root) = project_root {
        return root.join("app.json");
    }
    workspace
        .file_index
        .files
        .iter()
        .map(|entry| entry.key().clone())
        .min()
        // A relative path deliberately makes workspace/LSP URI conversion
        // fail closed if a caller invokes project diagnostics without either
        // a project root or an indexed source.
        .unwrap_or_else(|| std::path::PathBuf::from("app.json"))
}

fn native_finding_range(
    workspace: &Workspace,
    path: &std::path::Path,
) -> Result<crate::queries::Range, String> {
    if let Some((text, _)) = workspace.file_index.get_cached_parse(path) {
        let info = workspace.file_index.object_info.get(path).ok_or_else(|| {
            format!(
                "indexed source has no matching object metadata (workspace generation {})",
                workspace.generation_revision()
            )
        })?;
        return Ok(ts_range_to_query_range(info.range, text.as_bytes()));
    }

    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("al"))
    {
        return Err("AL source is missing from the coherent parse cache".to_string());
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("configuration source cannot be inspected: {error}"))?;
    if !metadata.is_file() {
        return Err("configuration source is not a regular file".to_string());
    }
    if metadata.len() > MAX_ARCH_CONFIG_BYTES {
        return Err(format!(
            "configuration source is {} bytes; maximum accepted size is {} bytes",
            metadata.len(),
            MAX_ARCH_CONFIG_BYTES
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("configuration source cannot be read as UTF-8: {error}"))?;
    let first_line = text.lines().next().unwrap_or_default();
    let end_character = u32::try_from(first_line.encode_utf16().count())
        .map_err(|_| "configuration source line exceeds the LSP position range".to_string())?;
    Ok(crate::queries::Range {
        start: crate::queries::Position {
            line: 0,
            character: 0,
        },
        end: crate::queries::Position {
            line: 0,
            character: end_character,
        },
    })
}

const MAX_ARCH_CONFIG_BYTES: u64 = 1_048_576;

fn read_arch_config(
    path: &std::path::Path,
) -> Result<Option<super::arch_lint::ArchConfig>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    };
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > MAX_ARCH_CONFIG_BYTES {
        return Err(format!(
            "{} is {} bytes; maximum accepted size is {} bytes",
            path.display(),
            metadata.len(),
            MAX_ARCH_CONFIG_BYTES
        ));
    }
    let json = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    super::arch_lint::ArchConfig::from_json(&json)
        .map(Some)
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn collect_diagnostics_from_tree(
    tree: &tree_sitter::Tree,
    text: &str,
    config: &AlConfig,
) -> Vec<SyntaxDiagnostic> {
    let mut diags = Vec::new();

    let source = text.as_bytes();

    for err in al_syntax::AlParser::errors_from_tree(tree) {
        let ts_range = err.range;
        diags.push(SyntaxDiagnostic {
            message: err.message,
            range: ts_range_to_query_range(ts_range, source),
            severity: SyntaxDiagnosticSeverity::Error,
            code: "syntax".to_string(),
            source: "al".to_string(),
        });
    }

    for lint in al_syntax::lint(tree, text) {
        if !config.is_lint_rule_enabled(&lint.code) {
            continue;
        }
        let severity = match lint.severity {
            al_syntax::LintSeverity::Error => SyntaxDiagnosticSeverity::Error,
            al_syntax::LintSeverity::Warning => SyntaxDiagnosticSeverity::Warning,
            al_syntax::LintSeverity::Info => SyntaxDiagnosticSeverity::Info,
            al_syntax::LintSeverity::Hint => SyntaxDiagnosticSeverity::Hint,
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

/// Converts tree-sitter byte-offset columns to UTF-16 code unit columns for LSP.
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
            character: al_syntax::byte_col_to_utf16_col(start_line, r.start_point.column),
        },
        end: crate::queries::Position {
            line: r.end_point.row as u32,
            character: al_syntax::byte_col_to_utf16_col(end_line, r.end_point.column),
        },
    }
}

fn source_line(source: &[u8], row: usize) -> &str {
    source
        .split(|&b| b == b'\n')
        .nth(row)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;

    fn make_workspace_with_text(text: &str) -> (Workspace, Url) {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/test.al").unwrap();
        ws.documents.open(uri.clone(), text.to_string()).unwrap();
        let _ = al_source::parsing::get_or_parse(&ws.documents, &uri);
        (ws, uri)
    }

    #[test]
    fn test_syntax_diagnostics_invalid_al_returns_errors() {
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

    #[test]
    fn test_syntax_diagnostics_missing_document_is_explicit() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent.al").unwrap();
        let config = AlConfig::default();
        let diags = syntax_diagnostics(&ws, &uri, &config);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "AL-NL000");
        assert_eq!(diags[0].severity, SyntaxDiagnosticSeverity::Error);
        assert!(diags[0].message.contains("not loaded"));
    }

    #[test]
    fn test_workspace_syntax_diagnostics_reports_indexed_file() {
        // A file present only in the FileIndex (never opened as a document) with
        // a syntax error must surface via the workspace-scope pass.
        let ws = Workspace::new();
        let bad = "codeunit 50100 Test\n{\n    procedure Broken(\n    begin\n    end;\n}\n";
        let path = std::path::PathBuf::from("/proj/Bad.al");
        let tree = al_syntax::AlParser::parse_quick(bad).tree;
        ws.file_index
            .add_file_with_tree(path.clone(), bad.to_string(), tree);

        let config = AlConfig::default();
        let results = workspace_syntax_diagnostics(&ws, &config).unwrap();
        let entry = results
            .iter()
            .find(|(p, _)| p == &path)
            .expect("indexed file should appear in workspace diagnostics");
        assert!(
            !entry.1.is_empty(),
            "expected syntax diagnostics for the indexed bad file, got none"
        );
    }

    #[test]
    fn test_workspace_syntax_diagnostics_clean_file_reports_empty() {
        let ws = Workspace::new();
        let good = "codeunit 50100 MyCodeunit\n{\n    trigger OnRun()\n    begin\n    end;\n}\n";
        let path = std::path::PathBuf::from("/proj/Good.al");
        let tree = al_syntax::AlParser::parse_quick(good).tree;
        ws.file_index
            .add_file_with_tree(path.clone(), good.to_string(), tree);

        let config = AlConfig::default();
        let results = workspace_syntax_diagnostics(&ws, &config).unwrap();
        let entry = results
            .iter()
            .find(|(p, _)| p == &path)
            .expect("indexed file should appear in workspace diagnostics");
        assert!(
            entry.1.is_empty(),
            "expected no diagnostics for a clean file, got: {:?}",
            entry.1.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn workspace_diagnostics_reject_missing_cached_parse_instead_of_omitting_file() {
        let ws = Workspace::new();
        let path = std::path::PathBuf::from("/proj/MissingCache.al");
        ws.file_index.files.insert(
            path.clone(),
            r#"codeunit 50100 "Missing Cache" { }"#.to_string(),
        );

        let error = workspace_syntax_diagnostics(&ws, &AlConfig::default()).unwrap_err();
        assert!(
            error.to_string().contains(&path.display().to_string()),
            "the incomplete path must be explicit: {error}"
        );
    }

    #[test]
    fn workspace_diagnostics_include_native_semantic_errors() {
        let ws = Workspace::new();
        let first = r#"codeunit 50100 "First" { }"#;
        let second = r#"codeunit 50100 "Second" { }"#;
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/First.al"),
            first.to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Second.al"),
            second.to_string(),
        );

        let diagnostics = native_workspace_diagnostics(&ws, &AlConfig::default());
        assert_eq!(
            diagnostics
                .iter()
                .filter(|(_, diagnostic)| diagnostic.code == "AL-NC001")
                .count(),
            2,
            "both duplicate declarations must receive a compile/editor diagnostic"
        );
        assert!(diagnostics.iter().all(|(_, diagnostic)| {
            diagnostic.code != "AL-NC001" || diagnostic.severity == SyntaxDiagnosticSeverity::Error
        }));
    }

    #[test]
    fn native_lint_master_toggle_suppresses_workspace_rules() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Try.al"),
            r#"codeunit 50100 "Try"
{
    [TryFunction]
    procedure Write()
    var
        Customer: Record Customer;
    begin
        Customer.Modify();
    end;
}"#
            .to_string(),
        );
        let config = AlConfig {
            enable_native_lint: false,
            ..AlConfig::default()
        };
        assert!(native_workspace_diagnostics(&ws, &config).is_empty());
    }

    #[test]
    fn workspace_diagnostics_include_builtin_architecture_rules() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Customer.al"),
            r#"table 50100 "Customer Extension Data"
{
    fields
    {
        field(1; Name; Text[100])
        {
            DataClassification = CustomerContent;
        }
    }

    trigger OnInsert()
    begin
        Message('Inserted');
    end;
}"#
            .to_string(),
        );

        let diagnostics = native_workspace_diagnostics(&ws, &AlConfig::default());
        let architecture = diagnostics
            .iter()
            .find(|(_, diagnostic)| diagnostic.code == ARCHITECTURE_LAYER_VIOLATION)
            .expect("table UI coupling should surface as AL-NL009");
        assert!(
            architecture.1.message.contains("BUILTIN-TABLE-NO-DIALOG"),
            "diagnostic must retain the concrete architecture rule id: {:?}",
            architecture.1.message
        );
    }

    #[test]
    fn architecture_rule_respects_per_rule_toggle() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Customer.al"),
            r#"table 50100 "Customer Extension Data"
{
    trigger OnInsert()
    begin
        Message('Inserted');
    end;
}"#
            .to_string(),
        );
        let mut config = AlConfig::default();
        config
            .native_lint_rules
            .insert(ARCHITECTURE_LAYER_VIOLATION.to_string(), false);

        let diagnostics = native_workspace_diagnostics(&ws, &config);
        assert!(
            diagnostics
                .iter()
                .all(|(_, diagnostic)| diagnostic.code != ARCHITECTURE_LAYER_VIOLATION),
            "AL-NL009 should be suppressed by its per-rule toggle"
        );
    }

    #[test]
    fn malformed_architecture_config_is_an_error_not_empty_rules() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".alarch.json"), "{not json").unwrap();
        let ws = Workspace::new();

        let diagnostics =
            native_workspace_diagnostics_at_root(&ws, &AlConfig::default(), Some(root.path()));
        let (path, diagnostic) = diagnostics
            .iter()
            .find(|(_, diagnostic)| diagnostic.code == ARCHITECTURE_LAYER_VIOLATION)
            .expect("invalid architecture config must surface");
        assert!(path.ends_with(".alarch.json"));
        assert_eq!(diagnostic.severity, SyntaxDiagnosticSeverity::Error);
        assert!(diagnostic
            .message
            .contains("Invalid architecture configuration"));
    }

    #[test]
    fn native_lint_master_toggle_suppresses_file_local_catalogue_rules() {
        let src = r#"page 50100 "Item List"
{
    layout
    {
        area(Content)
        {
            field(Description; Rec.Description)
            {
            }
        }
    }
}"#;
        let (ws, uri) = make_workspace_with_text(src);
        let config = AlConfig {
            enable_native_lint: false,
            ..AlConfig::default()
        };
        let diagnostics = syntax_diagnostics(&ws, &uri, &config);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !matches!(diagnostic.code.as_str(), "AL-NL006" | "AL-NL007")),
            "master toggle must suppress file-local native rules: {diagnostics:?}"
        );
    }

    #[test]
    fn native_lint_per_rule_toggle_suppresses_each_new_file_local_rule() {
        let cases = [
            (
                "AL-NL005",
                r#"codeunit 50100 Test
{
    procedure ReadItems()
    var
        Item: Record Item;
    begin
        Item.FindSet();
    end;
}"#,
            ),
            (
                "AL-NL006",
                r#"page 50100 "Item List"
{
    layout { area(Content) { field(Description; Rec.Description) { } } }
}"#,
            ),
            (
                "AL-NL007",
                r#"page 50100 "Item List"
{
    ApplicationArea = All;
    layout { area(Content) { field(Description; Rec.Description) { } } }
}"#,
            ),
        ];

        for (code, src) in cases {
            let (ws, uri) = make_workspace_with_text(src);
            let mut config = AlConfig::default();
            config.native_lint_rules.insert(code.to_string(), false);
            let diagnostics = syntax_diagnostics(&ws, &uri, &config);
            assert!(
                diagnostics.iter().all(|diagnostic| diagnostic.code != code),
                "{code} must honor its per-rule toggle: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn test_ts_range_to_query_range_converts_to_utf16() {
        let line = "// é好X";
        let source = line.as_bytes();
        let byte_col_x = line.find('X').unwrap();
        assert_eq!(byte_col_x, 8); // 2(//) + 1( ) + 2(é) + 3(好) = 8 bytes
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
        assert_eq!(
            q.start.character, 5,
            "expected UTF-16 column 5 for 'X', got {} — looks like raw byte column",
            q.start.character
        );
        assert_eq!(q.end.character, 6, "end column should be 6 in UTF-16");
        assert_eq!(q.start.line, 0);
    }
}
