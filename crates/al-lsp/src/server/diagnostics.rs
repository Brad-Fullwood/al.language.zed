//! Two-phase diagnostics — instant syntax + async analyzer.
//!
//! Phase 1 (instant): parse with tree-sitter and collect syntax, file-local,
//!                     project-semantic, and resolved call/event-stack native
//!                     diagnostics.
//! Phase 2 (async):   send to .NET SemanticBridge for CodeAnalysis diagnostics.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tower_lsp::lsp_types::*;

use super::AlServer;

pub(crate) struct CachedSemanticDiagnostics {
    pub(crate) text: Arc<String>,
    pub(crate) client_version: i32,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

#[derive(Clone)]
pub(crate) struct DiagnosticPublicationState {
    pub(crate) semantic_cache: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<Url, CachedSemanticDiagnostics>>,
    >,
    pub(crate) published_uris: std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<Url>>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkspaceDiagnosticError {
    #[error("workspace diagnostic path cannot be represented as a file URI: {}", .0.display())]
    InvalidFilePath(PathBuf),
    #[error("workspace diagnostic analysis failed: {0}")]
    Analysis(String),
    #[error("workspace diagnostic worker failed: {0}")]
    Worker(String),
}

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
/// diagnostics must not be published for them.
pub(crate) fn is_cache_path(uri: &Url) -> bool {
    if let Ok(path) = uri.to_file_path() {
        let cache_root = al_symbols::virtual_file::cache_dir();
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

    {
        let config_guard = server.workspace.config.read().await;
        let project_root = server
            .workspace
            .project
            .read()
            .await
            .as_ref()
            .map(|project| project.root.clone());
        let syntax_diags = al_analysis::queries::diagnostics::syntax_diagnostics_at_root(
            &server.workspace,
            uri,
            &config_guard,
            project_root.as_deref(),
        );
        drop(config_guard);
        diagnostics.extend(syntax_diags.iter().map(syntax_diag_to_lsp));
    }

    diagnostics.extend(run_semantic_analysis(server, uri, text).await);

    diagnostics
}

/// Compute project-scope diagnostics keyed by file for `workspace/diagnostic`.
///
/// Aggregates the diagnostics the project already produces per file:
///
/// * **Open documents** get the full per-document treatment (syntax + bridge)
///   via [`compute_diagnostics`], so they match the single-document pull handler
///   exactly. The DocumentStore text is authoritative — it carries unsaved edits.
/// * **Every other indexed workspace file** gets syntax/parse diagnostics from
///   its cached parse tree (no re-parse). Bridge/semantic analysis is *not* run
///   across unopened files: each is a multi-second CLR round-trip and is only
///   meaningful for files the user has open. Workspace scope therefore means
///   "parse errors everywhere, semantic errors for open files".
///
/// Files with no diagnostics are dropped, so a clean workspace yields an empty
/// `Vec`. Returns `(uri, document_version, diagnostics)`; `version` is `Some`
/// only for open documents.
pub(crate) async fn compute_workspace_diagnostics(
    server: &AlServer,
) -> Result<Vec<(Url, Option<i64>, Vec<Diagnostic>)>, WorkspaceDiagnosticError> {
    let mut reports: Vec<(Url, Option<i64>, Vec<Diagnostic>)> = Vec::new();
    let mut covered: std::collections::HashSet<Url> = std::collections::HashSet::new();

    // Phase 1 — open documents (syntax + bridge), reusing the canonical path.
    for uri in server.workspace.documents.open_uris() {
        if is_cache_path(&uri) {
            continue;
        }
        let Some(text) = server.workspace.documents.get_text_arc(&uri) else {
            continue;
        };
        let diags = compute_diagnostics(server, &uri, &text).await;
        let version = server
            .workspace
            .documents
            .get_client_version(&uri)
            .map(|v| v as i64);
        covered.insert(uri.clone());
        if !diags.is_empty() {
            reports.push((uri, version, diags));
        }
    }

    // Phase 2 — remaining indexed files (syntax only, from cached trees).
    let config = server.workspace.config.read().await.clone();
    let project_root = server
        .workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let native = al_analysis::queries::diagnostics::workspace_syntax_diagnostics_at_root(
        &server.workspace,
        &config,
        project_root.as_deref(),
    )
    .map_err(|error| WorkspaceDiagnosticError::Analysis(error.to_string()))?;
    for (path, diags) in native {
        if diags.is_empty() {
            continue;
        }
        let uri = Url::from_file_path(&path)
            .map_err(|()| WorkspaceDiagnosticError::InvalidFilePath(path.clone()))?;
        if covered.contains(&uri) || is_cache_path(&uri) {
            continue;
        }
        let lsp: Vec<Diagnostic> = diags.iter().map(syntax_diag_to_lsp).collect();
        reports.push((uri, None, lsp));
    }

    Ok(reports)
}

/// Compute the bridge-free project diagnostic generation used by push
/// diagnostics. Cached semantic diagnostics are merged only when they belong
/// to the exact current open-document `Arc` and client version.
async fn compute_workspace_push_diagnostics(
    workspace: std::sync::Arc<al_workspace::Workspace>,
    semantic_cache: &tokio::sync::Mutex<std::collections::HashMap<Url, CachedSemanticDiagnostics>>,
) -> Result<Vec<(Url, Option<i32>, Vec<Diagnostic>)>, WorkspaceDiagnosticError> {
    let config = workspace.config.read().await.clone();
    let project_root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let worker_workspace = std::sync::Arc::clone(&workspace);
    let native = tokio::task::spawn_blocking(move || {
        al_analysis::queries::diagnostics::workspace_syntax_diagnostics_at_root(
            &worker_workspace,
            &config,
            project_root.as_deref(),
        )
    })
    .await
    .map_err(|error| WorkspaceDiagnosticError::Worker(error.to_string()))?
    .map_err(|error| WorkspaceDiagnosticError::Analysis(error.to_string()))?;

    let semantic_cache = semantic_cache.lock().await;
    let mut reports = Vec::new();
    for (path, diagnostics) in native {
        let uri = Url::from_file_path(&path)
            .map_err(|()| WorkspaceDiagnosticError::InvalidFilePath(path))?;
        if is_cache_path(&uri) {
            continue;
        }
        let mut diagnostics: Vec<Diagnostic> = diagnostics.iter().map(syntax_diag_to_lsp).collect();
        let snapshot = workspace.documents.get_text_and_client_version(&uri);
        let version = snapshot.as_ref().map(|(_, version)| *version);
        if let (Some((text, version)), Some(cached)) = (snapshot.as_ref(), semantic_cache.get(&uri))
        {
            if *version == cached.client_version && Arc::ptr_eq(text, &cached.text) {
                diagnostics.extend(cached.diagnostics.clone());
            }
        }
        if !diagnostics.is_empty() {
            reports.push((uri, version, diagnostics));
        }
    }
    Ok(reports)
}

/// Re-publish the complete workspace diagnostic generation, including empty
/// arrays for files that became clean.
///
/// Cross-file native rules (duplicate identities, dependency and architecture
/// checks) can change on an open, close, save, or reindex of a *different*
/// document. Publishing only non-empty results leaves stale diagnostics in the
/// client indefinitely, so this helper explicitly covers every indexed/open
/// URI and overlays the computed non-empty reports.
pub(crate) async fn publish_workspace_diagnostics(server: &AlServer) {
    publish_workspace_diagnostics_parts(
        std::sync::Arc::clone(&server.workspace),
        server.client.clone(),
        std::sync::Arc::clone(&server.semantic_diagnostic_cache),
        std::sync::Arc::clone(&server.workspace_diagnostic_uris),
        None,
    )
    .await;
}

pub(crate) async fn publish_workspace_diagnostics_parts(
    workspace: std::sync::Arc<al_workspace::Workspace>,
    client: tower_lsp::Client,
    semantic_cache: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<Url, CachedSemanticDiagnostics>>,
    >,
    published_uris: std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<Url>>>,
    force_clear_uri: Option<Url>,
) {
    // Compute from one immutable workspace generation. didOpen/didChange/
    // didClose and reindex all take the generation write lock while mutating
    // the document store, file index, and parse caches. Dropping this read lock
    // before `compute_workspace_push_diagnostics` used to let those mutations
    // interleave with `coherent_snapshot`, producing MissingCachedParse or
    // GenerationChanged errors during ordinary editor traffic.
    //
    // We release the lock before publishing over the client transport, then
    // retry if a newer generation won the race between computation and
    // publication. This keeps the snapshot atomic without holding edits behind
    // potentially slow client I/O.
    loop {
        let generation = workspace.generation_lock.read().await;
        let revision = workspace.generation_revision();

        let reports = match compute_workspace_push_diagnostics(
            std::sync::Arc::clone(&workspace),
            &semantic_cache,
        )
        .await
        {
            Ok(reports) => reports,
            Err(error) => {
                tracing::error!(%error, "project diagnostics generation failed");
                client
                    .show_message(
                        MessageType::ERROR,
                        format!("AL project diagnostics failed: {error}"),
                    )
                    .await;
                return;
            }
        };
        drop(generation);

        let generation = workspace.generation_lock.read().await;
        if workspace.generation_revision() != revision {
            drop(generation);
            tokio::task::yield_now().await;
            continue;
        }

        let mut current = std::collections::BTreeMap::new();
        for (uri, version, diagnostics) in reports {
            current.insert(uri, (version, diagnostics));
        }
        let mut published = published_uris.lock().await;
        let mut stale: Vec<Url> = published
            .difference(&current.keys().cloned().collect())
            .cloned()
            .collect();
        if let Some(uri) = force_clear_uri {
            if !current.contains_key(&uri) && !stale.contains(&uri) {
                stale.push(uri);
            }
        }
        stale.sort();

        for uri in stale {
            let version = workspace.documents.get_client_version(&uri);
            client.publish_diagnostics(uri, Vec::new(), version).await;
        }
        for (uri, (version, diagnostics)) in &current {
            client
                .publish_diagnostics(uri.clone(), diagnostics.clone(), *version)
                .await;
        }
        *published = current.into_keys().collect();
        drop(published);
        drop(generation);
        return;
    }
}

pub(crate) async fn publish_diagnostics(
    server: &AlServer,
    uri: &Url,
    text: Arc<String>,
    expected_client_version: i32,
) {
    // skip diagnostics for virtual symbol cache files — they are not
    // workspace files and Zed logs a warning for every publishDiagnostics on them.
    if is_cache_path(uri) {
        tracing::debug!(uri = %uri, "publish_diagnostics: skipping cache file");
        return;
    }
    if !document_snapshot_is_current(server, uri, &text, expected_client_version) {
        tracing::debug!(
            uri = %uri,
            expected_client_version,
            "publish_diagnostics: document version changed before analysis; skipping stale generation"
        );
        return;
    }

    tracing::debug!(uri = %uri, text_len = text.len(), "publish_diagnostics: entry");
    let mut diagnostics = Vec::new();

    // Reuse the parse tree already cached by update_workspace_index to avoid a
    // redundant parse on every did_open or did_change.
    {
        let parse_start = std::time::Instant::now();
        let config_guard = server.workspace.config.read().await;
        let project_root = server
            .workspace
            .project
            .read()
            .await
            .as_ref()
            .map(|project| project.root.clone());
        let syntax_diags = al_analysis::queries::diagnostics::syntax_diagnostics_at_root(
            &server.workspace,
            uri,
            &config_guard,
            project_root.as_deref(),
        );
        drop(config_guard);
        let parse_elapsed = parse_start.elapsed();
        let error_count = syntax_diags.len();
        tracing::debug!(uri = %uri, error_count, parse_us = parse_elapsed.as_micros() as u64, "publish_diagnostics: diagnostics from query");
        diagnostics.extend(syntax_diags.iter().map(syntax_diag_to_lsp));
    }

    let phase1_count = diagnostics.len();
    if !document_snapshot_is_current(server, uri, &text, expected_client_version) {
        tracing::debug!(
            uri = %uri,
            expected_client_version,
            "publish_diagnostics: document version changed during phase 1; skipping stale generation"
        );
        return;
    }
    let document_version = Some(expected_client_version);
    tracing::debug!(uri = %uri, phase1_count, "publish_diagnostics: publishing phase 1");
    server
        .client
        .publish_diagnostics(uri.clone(), diagnostics.clone(), document_version)
        .await;

    let semantic_diags = run_semantic_analysis(server, uri, &text).await;
    if !document_snapshot_is_current(server, uri, &text, expected_client_version) {
        tracing::debug!(
            uri = %uri,
            expected_client_version,
            "publish_diagnostics: document version changed during semantic analysis; skipping stale phase 2"
        );
        return;
    }
    server.semantic_diagnostic_cache.lock().await.insert(
        uri.clone(),
        CachedSemanticDiagnostics {
            text: Arc::clone(&text),
            client_version: expected_client_version,
            diagnostics: semantic_diags.clone(),
        },
    );
    if !semantic_diags.is_empty() {
        diagnostics.extend(semantic_diags);
        let total_count = diagnostics.len();
        tracing::debug!(uri = %uri, total_count, "publish_diagnostics: publishing phase 2");
        server
            .client
            .publish_diagnostics(uri.clone(), diagnostics, document_version)
            .await;
    }
}

fn document_snapshot_is_current(
    server: &AlServer,
    uri: &Url,
    expected_text: &Arc<String>,
    expected_client_version: i32,
) -> bool {
    server
        .workspace
        .documents
        .get_text_and_client_version(uri)
        .is_some_and(|(current_text, current_version)| {
            current_version == expected_client_version && Arc::ptr_eq(&current_text, expected_text)
        })
}

/// Run semantic analysis via .NET bridge if enabled. Returns diagnostics or empty vec.
///
/// Shared between `compute_diagnostics` (pull) and `publish_diagnostics` (push Phase 2).
async fn run_semantic_analysis(server: &AlServer, uri: &Url, text: &str) -> Vec<Diagnostic> {
    let (
        enable_analysis,
        bg_analysis,
        configured_analyzers,
        assembly_probing_paths,
        configured_package_cache,
    ) = {
        let cfg = server.workspace.config.read().await;
        (
            cfg.enable_code_analysis,
            cfg.background_code_analysis,
            cfg.code_analyzers.clone(),
            cfg.assembly_probing_paths.clone(),
            cfg.package_cache_path.clone(),
        )
    };
    if !enable_analysis || !bg_analysis {
        tracing::debug!(uri = %uri, enable_analysis, bg_analysis, "semantic analysis disabled by config");
        return vec![];
    }

    let file_path = match uri.to_file_path() {
        Ok(path) => path,
        Err(()) => {
            return vec![semantic_pipeline_diagnostic(format!(
                "Microsoft semantic analysis requires a local file URI, got {uri}"
            ))];
        }
    };
    let project = server.workspace.project.read().await.clone();
    let project_root = project
        .as_ref()
        .map(|project| project.root.clone())
        .or_else(|| file_path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let package_cache = configured_package_cache
        .or_else(|| project.as_ref().map(|project| project.packages_dir.clone()))
        .unwrap_or_else(|| project_root.join(".alpackages"));

    let analyzers = match tokio::task::spawn_blocking(move || {
        resolve_semantic_analyzer_entries(
            &configured_analyzers,
            &project_root,
            &assembly_probing_paths,
        )
    })
    .await
    {
        Ok(Ok(analyzers)) => analyzers,
        Ok(Err(error)) => return vec![semantic_pipeline_diagnostic(error)],
        Err(error) => {
            return vec![semantic_pipeline_diagnostic(format!(
                "analyzer discovery worker failed: {error}"
            ))];
        }
    };

    let guard = match server.get_or_init_bridge().await {
        Some(g) => g,
        None => return vec![],
    };
    let bridge = match guard.as_ref() {
        Some(b) => b,
        None => return vec![],
    };
    let bridge_generation = bridge.generation();

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
            let should_restart =
                is_persistent || matches!(&error, crate::semantic::SemanticError::HostInit(_));
            drop(guard);
            if should_restart {
                if let Err(restart_error) =
                    al_workspace::restart_bridge_if_current(&server.workspace, bridge_generation)
                        .await
                {
                    tracing::warn!(error = %restart_error, "semantic bridge restart failed");
                }
            }
            if is_persistent && server.should_report_semantic_failure() {
                if let Some(sink) = server.workspace.notify_sink.get() {
                    sink(&format!(
                        "AL semantic analysis failed: {error}. The bridge restart path has been invoked."
                    ));
                }
            }
            vec![semantic_pipeline_diagnostic(format!(
                "Microsoft semantic analysis failed: {error}"
            ))]
        }
    }
}

fn resolve_semantic_analyzer_entries(
    configured: &[String],
    project_root: &Path,
    assembly_probing_paths: &[PathBuf],
) -> Result<Vec<String>, String> {
    configured
        .iter()
        .map(|entry| {
            if al_project::analyzers::is_builtin_analyzer(entry) {
                return Ok(entry.clone());
            }
            al_project::analyzers::discover_custom_analyzer(
                entry,
                project_root,
                assembly_probing_paths,
            )
            .map_err(|error| error.to_string())?
            .map(|path| path.display().to_string())
            .ok_or_else(|| {
                format!(
                    "Requested analyzer '{entry}' could not be found in the project, probing paths, NuGet cache, or common editor extension locations"
                )
            })
        })
        .collect()
}

fn semantic_pipeline_diagnostic(message: String) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String("AL-SEMANTIC".to_string())),
        source: Some("al-analyzer".to_string()),
        message,
        ..Default::default()
    }
}

/// Convert a transport-agnostic `SyntaxDiagnostic` to an LSP `Diagnostic`.
///
/// `SyntaxDiagnostic.range` already carries UTF-16 code unit columns — the
/// `queries::diagnostics::ts_range_to_query_range` helper runs `byte_col_to_utf16_col`
/// at the query layer. This is a thin shim that maps the already-converted
/// `queries::Range` to LSP `Range` without re-walking the source.
pub(crate) fn syntax_diag_to_lsp(
    diag: &al_analysis::queries::diagnostics::SyntaxDiagnostic,
) -> Diagnostic {
    use al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity;

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
pub fn syntax_error_to_diagnostic(err: &al_syntax::SyntaxError, source: &[u8]) -> Diagnostic {
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
pub fn lint_to_diagnostic(lint: &al_syntax::LintDiagnostic, source: &[u8]) -> Diagnostic {
    let severity = match lint.severity {
        al_syntax::LintSeverity::Error => DiagnosticSeverity::ERROR,
        al_syntax::LintSeverity::Warning => DiagnosticSeverity::WARNING,
        al_syntax::LintSeverity::Info => DiagnosticSeverity::INFORMATION,
        al_syntax::LintSeverity::Hint => DiagnosticSeverity::HINT,
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

/// Convert a `TestDiagnostic` from the test runner to an LSP `Diagnostic`.
///
/// Lines in `TestDiagnostic` are 1-based; LSP positions are 0-based.
pub fn test_diag_to_lsp(td: &al_analysis::queries::test_diagnostics::TestDiagnostic) -> Diagnostic {
    use al_analysis::queries::test_diagnostics::DiagnosticSeverity as TDSev;

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
    diagnostics: &[al_analysis::queries::test_diagnostics::TestDiagnostic],
) {
    use al_analysis::queries::test_diagnostics::group_by_file;

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
    fn semantic_analyzer_resolution_preserves_builtins_and_discovers_custom_names() {
        let project = tempfile::tempdir().unwrap();
        let dll = project
            .path()
            .join(".netpackages/businesscentral.lintercop/1.0.0/BusinessCentral.LinterCop.dll");
        std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
        std::fs::write(&dll, b"analyzer").unwrap();

        let resolved = resolve_semantic_analyzer_entries(
            &[
                "CodeCop".to_string(),
                "BusinessCentral.LinterCop".to_string(),
            ],
            project.path(),
            &[],
        )
        .unwrap();
        assert_eq!(resolved[0], "CodeCop");
        assert_eq!(
            resolved[1],
            dll.canonicalize().unwrap().display().to_string()
        );
    }

    #[test]
    fn missing_requested_semantic_analyzer_is_explicit() {
        let project = tempfile::tempdir().unwrap();
        let error = resolve_semantic_analyzer_entries(
            &["Missing.Custom.Analyzer".to_string()],
            project.path(),
            &[],
        )
        .unwrap_err();
        assert!(error.contains("could not be found"));

        let diagnostic = semantic_pipeline_diagnostic(error);
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostic.code,
            Some(NumberOrString::String("AL-SEMANTIC".to_string()))
        );
        assert_eq!(diagnostic.source.as_deref(), Some("al-analyzer"));
    }

    #[test]
    fn test_syntax_error_to_diagnostic() {
        let src = "codeunit 50100 T { }";
        let err = al_syntax::SyntaxError {
            message: "Missing semicolon".to_string(),
            range: al_syntax::AlParser::parse_quick(src)
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
        let lint = al_syntax::LintDiagnostic {
            code: "AL-L001".to_string(),
            message: "Empty begin..end block".to_string(),
            range: al_syntax::AlParser::parse_quick(src)
                .tree
                .root_node()
                .range(),
            severity: al_syntax::LintSeverity::Warning,
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
        let lint = al_syntax::LintDiagnostic {
            code: "AL-L006".to_string(),
            message: "Empty trigger".to_string(),
            range: al_syntax::AlParser::parse_quick(src)
                .tree
                .root_node()
                .range(),
            severity: al_syntax::LintSeverity::Hint,
        };

        let diag = lint_to_diagnostic(&lint, src.as_bytes());
        assert_eq!(diag.severity, Some(DiagnosticSeverity::HINT));
    }

    #[test]
    fn test_lint_to_diagnostic_info() {
        let src = "codeunit 50100 T { }";
        let lint = al_syntax::LintDiagnostic {
            code: "AL-L007".to_string(),
            message: "TODO comment".to_string(),
            range: al_syntax::AlParser::parse_quick(src)
                .tree
                .root_node()
                .range(),
            severity: al_syntax::LintSeverity::Info,
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

    #[test]
    fn test_diag_fail_maps_to_error() {
        use al_analysis::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
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
        use al_analysis::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
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
        use al_analysis::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
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
        let cache_root = al_symbols::virtual_file::cache_dir();
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
        sev: al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity,
    ) -> al_analysis::queries::diagnostics::SyntaxDiagnostic {
        al_analysis::queries::diagnostics::SyntaxDiagnostic {
            message: "boom".to_string(),
            range: al_analysis::queries::Range {
                start: al_analysis::queries::Position {
                    line: 3,
                    character: 7,
                },
                end: al_analysis::queries::Position {
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
        use al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity;
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
        use al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity;
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

    #[test]
    fn test_diag_information_and_hint_severities() {
        use al_analysis::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
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
        use al_analysis::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
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
