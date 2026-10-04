//! Two-phase diagnostics — instant syntax + async analyzer.
//!
//! Phase 1 (instant): parse with tree-sitter and collect syntax, file-local,
//!                     project-semantic, and resolved call/event-stack native
//!                     diagnostics.
//! Phase 2 (async):   send to .NET SemanticBridge for CodeAnalysis diagnostics.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tower_lsp::lsp_types::*;

use super::lsp::LspSessionState;
use super::AlServer;

/// The latest Microsoft (compiler and analyzer) diagnostics of one file. They
/// are kept until a newer pass for the file replaces them, so every publish
/// for the file can include them.
pub(crate) struct CachedSemanticDiagnostics {
    pub(crate) diagnostics: Vec<Diagnostic>,
}

#[derive(Clone)]
pub(crate) struct DiagnosticPublicationState {
    pub(crate) semantic_cache: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<Url, CachedSemanticDiagnostics>>,
    >,
    pub(crate) published_uris: std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<Url>>>,
    pub(crate) session: LspSessionState,
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
    let mut diagnostics: Vec<Diagnostic> = syntax_diagnostics(server, uri)
        .await
        .iter()
        .map(syntax_diag_to_lsp)
        .collect();

    diagnostics.extend(
        run_semantic_analysis(server, uri, text)
            .await
            .unwrap_or_default(),
    );

    diagnostics
}

/// Native rules and the analyzer whose rule reports the same thing: AL-NL007
/// (control without a tooltip) and CodeCop's AA0218, AL-NL005 (a read
/// without SetLoadFields) and ALCops PlatformCop's PC0030.
const NATIVE_RULES_COVERED_BY_ANALYZERS: &[(&str, &str)] =
    &[("AL-NL007", "codecop"), ("AL-NL005", "alcops.platformcop")];

/// The native rules a configured analyzer already covers. An entry is
/// matched by its bare name, whatever the spelling: `CodeCop`, `${CodeCop}`,
/// `${analyzerFolder}ALCops.PlatformCop.dll`, or a path to the DLL.
fn native_rules_covered_by(configured_analyzers: &[String]) -> Vec<&'static str> {
    let names: Vec<String> = configured_analyzers
        .iter()
        .map(|entry| {
            let entry = entry.trim();
            let entry = entry.strip_prefix("${analyzerFolder}").unwrap_or(entry);
            let file = entry.rsplit(['/', '\\']).next().unwrap_or(entry);
            al_project::analyzers::analyzer_name(file).to_ascii_lowercase()
        })
        .collect();
    NATIVE_RULES_COVERED_BY_ANALYZERS
        .iter()
        .filter(|(_, analyzer)| names.iter().any(|name| name == analyzer))
        .map(|(rule, _)| *rule)
        .collect()
}

/// The configuration native lint runs with. A native rule that a configured
/// analyzer covers is off while the compiler pass can run (code analysis on,
/// a toolchain found), so a finding is not listed twice. A rule turned on
/// explicitly in `al.nativeLintRules` stays on.
pub(crate) async fn native_lint_config(
    workspace: &al_workspace::Workspace,
    mut config: al_project::config::AlConfig,
) -> al_project::config::AlConfig {
    let compiler_pass_runs = config.enable_code_analysis
        && config.background_code_analysis
        && workspace.toolchain.read().await.is_some();
    if compiler_pass_runs {
        for rule in native_rules_covered_by(&config.code_analyzers) {
            config
                .native_lint_rules
                .entry(rule.to_string())
                .or_insert(false);
        }
    }
    config
}

/// Remove native findings of rules `config` turns off.
fn drop_silenced_native_rules(
    diagnostics: &mut Vec<Diagnostic>,
    config: &al_project::config::AlConfig,
) {
    diagnostics.retain(|diagnostic| match &diagnostic.code {
        Some(NumberOrString::String(code)) => config.native_lint_rules.get(code) != Some(&false),
        _ => true,
    });
}

/// Phase 1 syntax and lint diagnostics for `uri`.
///
/// The project root is read and released before the config guard is taken, so
/// no guard is held across an await. `did_change_configuration` holds the
/// project write guard while it waits for the config write guard. A config
/// read guard held across `project.read()` here waited on that writer while
/// the writer waited on it, and the generation write guard the writer also
/// holds then stalled every other request.
async fn syntax_diagnostics(
    server: &AlServer,
    uri: &Url,
) -> Vec<al_analysis::queries::diagnostics::SyntaxDiagnostic> {
    let project_root = server
        .workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let config = server.workspace.config.read().await.clone();
    let config = native_lint_config(&server.workspace, config).await;
    // Native lint walks the project for its cross-file rules, which takes
    // hundreds of milliseconds on a real project. On the blocking pool it
    // does not hold up the requests (semantic tokens, inlay hints) that the
    // editor sends right after an open.
    let workspace = Arc::clone(&server.workspace);
    let uri = uri.clone();
    match tokio::task::spawn_blocking(move || {
        al_analysis::queries::diagnostics::syntax_diagnostics_at_root(
            &workspace,
            &uri,
            &config,
            project_root.as_deref(),
        )
    })
    .await
    {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            tracing::error!(%error, "native diagnostics worker failed");
            Vec::new()
        }
    }
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

/// What the project pass read for one URI while staging its report or its
/// clear: the file index entry and, when the document is open, its text and
/// client version.
///
/// Every write to either one stores a new `Arc`, and this input holds the
/// staged `Arc`s, so pointer equality tells whether either was replaced.
struct StagedInput {
    /// The file index key of `uri`, `None` when `uri` is not a file URI.
    path: Option<std::path::PathBuf>,
    source: Option<Arc<(String, tree_sitter::Tree)>>,
    document: Option<(Arc<String>, i32)>,
}

impl StagedInput {
    fn read(
        workspace: &al_workspace::Workspace,
        path: Option<std::path::PathBuf>,
        uri: &Url,
    ) -> Self {
        Self {
            source: path
                .as_deref()
                .and_then(|path| workspace.file_index.cached_parse_entry(path)),
            document: workspace.documents.get_text_and_client_version(uri),
            path,
        }
    }

    fn client_version(&self) -> Option<i32> {
        self.document.as_ref().map(|(_, version)| *version)
    }

    /// Whether the file index entry and the document text and version for
    /// `uri` are still the ones this input was read from.
    fn is_current(&self, workspace: &al_workspace::Workspace, uri: &Url) -> bool {
        let source = self
            .path
            .as_deref()
            .and_then(|path| workspace.file_index.cached_parse_entry(path));
        let same_source = match (&self.source, &source) {
            (Some(staged), Some(current)) => Arc::ptr_eq(staged, current),
            (None, None) => true,
            _ => false,
        };
        let document = workspace.documents.get_text_and_client_version(uri);
        let same_document = match (&self.document, &document) {
            (Some((staged, staged_version)), Some((current, current_version))) => {
                staged_version == current_version && Arc::ptr_eq(staged, current)
            }
            (None, None) => true,
            _ => false,
        };
        same_source && same_document
    }
}

/// One staging attempt of the project pass.
struct StagedPass {
    /// Every URI with diagnostics, with the input they were computed from.
    reports: Vec<(Url, StagedInput, Vec<Diagnostic>)>,
    /// The input of every URI the publish step may clear: each one in the
    /// published set, or the forced clear, that has no report.
    clears: std::collections::HashMap<Url, StagedInput>,
}

/// Compute the bridge-free project diagnostic generation used by push
/// diagnostics. Cached semantic diagnostics are merged only when they belong
/// to the exact current open-document `Arc` and client version.
async fn compute_workspace_push_diagnostics(
    workspace: std::sync::Arc<al_workspace::Workspace>,
    semantic_cache: &tokio::sync::Mutex<std::collections::HashMap<Url, CachedSemanticDiagnostics>>,
    published_uris: &tokio::sync::Mutex<std::collections::HashSet<Url>>,
    force_clear_uri: Option<&Url>,
) -> Result<StagedPass, WorkspaceDiagnosticError> {
    let config = workspace.config.read().await.clone();
    let config = native_lint_config(&workspace, config).await;
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

    // Native diagnostics are staged here. Microsoft diagnostics are added when
    // the pass publishes (`with_semantic`), so results that arrive while this
    // pass computes are not overwritten by a native-only set. A URI is staged
    // when it has either kind.
    let semantic_uris: std::collections::HashSet<Url> =
        semantic_cache.lock().await.keys().cloned().collect();
    let mut reports = Vec::new();
    for (path, diagnostics) in native {
        let Ok(uri) = Url::from_file_path(&path) else {
            return Err(WorkspaceDiagnosticError::InvalidFilePath(path));
        };
        if is_cache_path(&uri) {
            continue;
        }
        let diagnostics: Vec<Diagnostic> = diagnostics.iter().map(syntax_diag_to_lsp).collect();
        if diagnostics.is_empty() && !semantic_uris.contains(&uri) {
            continue;
        }
        let input = StagedInput::read(&workspace, Some(path), &uri);
        reports.push((uri, input, diagnostics));
    }

    let reported: std::collections::HashSet<&Url> = reports.iter().map(|(uri, _, _)| uri).collect();
    let candidates: Vec<Url> = published_uris
        .lock()
        .await
        .iter()
        .chain(force_clear_uri)
        .filter(|uri| !reported.contains(uri))
        .cloned()
        .collect();
    let clears = candidates
        .into_iter()
        .map(|uri| {
            let input = StagedInput::read(&workspace, uri.to_file_path().ok(), &uri);
            (uri, input)
        })
        .collect();
    Ok(StagedPass { reports, clears })
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
        &server.session,
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
    session: &LspSessionState,
) -> bool {
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
    //
    // The retry is bounded. `on_document_change` bumps the revision on every
    // keystroke, so on a project where the pass takes longer than the typing
    // gaps an unbounded loop recomputed forever and published nothing: with
    // `diagnosticsScope: "project"` the user saw no diagnostics at all while
    // typing. After MAX_STAGING_ATTEMPTS the newest computed result is
    // published for every URI whose input is unchanged since staging. A URI
    // that changed is left to whatever changed it: the document's own
    // publish, the pass `did_close` runs, or the debounced pass that the last
    // keystroke or file change on disk armed.
    const MAX_STAGING_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_STAGING_ATTEMPTS {
        if session.is_cancelled() {
            return false;
        }
        let generation = workspace.generation_lock.read().await;
        let revision = workspace.generation_revision();

        let staged = match compute_workspace_push_diagnostics(
            std::sync::Arc::clone(&workspace),
            &semantic_cache,
            &published_uris,
            force_clear_uri.as_ref(),
        )
        .await
        {
            Ok(staged) => staged,
            Err(error) => {
                if session.is_cancelled() {
                    return false;
                }
                tracing::error!(%error, "project diagnostics generation failed");
                client
                    .show_message(
                        MessageType::ERROR,
                        format!("AL project diagnostics failed: {error}"),
                    )
                    .await;
                return false;
            }
        };
        drop(generation);

        if session.is_cancelled() {
            return false;
        }
        let generation = workspace.generation_lock.read().await;
        if workspace.generation_revision() != revision && attempt < MAX_STAGING_ATTEMPTS {
            drop(generation);
            tokio::task::yield_now().await;
            continue;
        }

        let mut current = std::collections::BTreeMap::new();
        {
            let semantic = semantic_cache.lock().await;
            for (uri, input, mut diagnostics) in staged.reports {
                if let Some(cached) = semantic.get(&uri) {
                    diagnostics.extend(cached.diagnostics.iter().cloned());
                }
                if !diagnostics.is_empty() {
                    current.insert(uri, (input, diagnostics));
                }
            }
        }
        let mut published = published_uris.lock().await;
        let mut stale: Vec<Url> = published
            .difference(&current.keys().cloned().collect())
            .cloned()
            .collect();
        if let Some(uri) = &force_clear_uri {
            if !current.contains_key(uri) && !stale.contains(uri) {
                stale.push(uri.clone());
            }
        }
        stale.sort();

        let mut kept = Vec::new();
        for uri in stale {
            if session.is_cancelled() {
                return false;
            }
            // A clear is sent only while the URI's input is the one this pass
            // staged. An edit since staging may already have published the
            // document's new errors, which the clear would remove. A URI with
            // no staged input was added to `published` by another publish
            // after staging. A skipped URI stays in `published`, so a later
            // pass clears it if it has no diagnostics by then.
            if !staged
                .clears
                .get(&uri)
                .is_some_and(|input| input.is_current(&workspace, &uri))
            {
                tracing::debug!(uri = %uri, "project diagnostics: input changed after staging, clear skipped");
                kept.push(uri);
                continue;
            }
            let version = workspace.documents.get_client_version(&uri);
            client.publish_diagnostics(uri, Vec::new(), version).await;
        }
        for (uri, (input, diagnostics)) in &current {
            if session.is_cancelled() {
                return false;
            }
            // On the last attempt the revision may have moved since staging.
            // A URI whose document or file index entry changed in between
            // gets no report from this pass: `did_close` may already have
            // sent its clear, and a report staged before the close would land
            // after it. The URI stays in `published`, so a later pass clears
            // it if it has no diagnostics by then.
            if !input.is_current(&workspace, uri) {
                tracing::debug!(uri = %uri, "project diagnostics: input changed after staging, report skipped");
                continue;
            }
            client
                .publish_diagnostics(uri.clone(), diagnostics.clone(), input.client_version())
                .await;
        }
        *published = current.into_keys().chain(kept).collect();
        drop(published);
        drop(generation);
        return true;
    }
    false
}

pub(crate) async fn publish_diagnostics(
    server: &AlServer,
    uri: &Url,
    text: Arc<String>,
    expected_client_version: i32,
) {
    if server.session.is_cancelled() {
        return;
    }
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
        let syntax_diags = syntax_diagnostics(server, uri).await;
        let parse_elapsed = parse_start.elapsed();
        let error_count = syntax_diags.len();
        tracing::debug!(uri = %uri, error_count, parse_us = parse_elapsed.as_micros() as u64, "publish_diagnostics: diagnostics from query");
        diagnostics.extend(syntax_diags.iter().map(syntax_diag_to_lsp));
    }

    // Phase 1 carries the file's last Microsoft diagnostics until phase 2
    // replaces them. Publishing native diagnostics alone would make every
    // compiler and analyzer finding vanish for the length of the semantic
    // pass, then come back.
    let previous_semantic = server
        .semantic_diagnostic_cache
        .lock()
        .await
        .get(uri)
        .map(|cached| cached.diagnostics.clone())
        .unwrap_or_default();
    let mut phase1 = diagnostics.clone();
    phase1.extend(previous_semantic);
    let phase1_count = phase1.len();
    if server.session.is_cancelled() {
        return;
    }
    tracing::debug!(uri = %uri, phase1_count, "publish_diagnostics: publishing phase 1");
    record_project_publication(server, uri, !phase1.is_empty()).await;
    if !publish_if_current(
        &server.workspace,
        &server.client,
        uri,
        &text,
        expected_client_version,
        phase1,
    )
    .await
    {
        tracing::debug!(
            uri = %uri,
            expected_client_version,
            "publish_diagnostics: document version changed during phase 1; skipping stale generation"
        );
        return;
    }

    let semantic_diags = run_semantic_analysis(server, uri, &text).await;
    if server.session.is_cancelled() {
        return;
    }
    if !document_snapshot_is_current(server, uri, &text, expected_client_version) {
        tracing::debug!(
            uri = %uri,
            expected_client_version,
            "publish_diagnostics: document version changed during semantic analysis; skipping stale phase 2"
        );
        return;
    }
    // No pass ran (no toolchain, or the bridge is not up): phase 1 already
    // shows the last results, which stay.
    let Some(semantic_diags) = semantic_diags else {
        return;
    };
    server.semantic_diagnostic_cache.lock().await.insert(
        uri.clone(),
        CachedSemanticDiagnostics {
            diagnostics: semantic_diags.clone(),
        },
    );
    // The native pass can run before the toolchain is found, when no native
    // rule is silenced yet. By phase 2 the compiler pass has run, so the rules
    // its analyzers cover are dropped here.
    let config = server.workspace.config.read().await.clone();
    let config = native_lint_config(&server.workspace, config).await;
    drop_silenced_native_rules(&mut diagnostics, &config);
    // Always published, also when the semantic pass found nothing: phase 1
    // may hold findings this pass no longer reports.
    {
        diagnostics.extend(semantic_diags);
        let total_count = diagnostics.len();
        tracing::debug!(uri = %uri, total_count, "publish_diagnostics: publishing phase 2");
        record_project_publication(server, uri, !diagnostics.is_empty()).await;
        if !publish_if_current(
            &server.workspace,
            &server.client,
            uri,
            &text,
            expected_client_version,
            diagnostics,
        )
        .await
        {
            tracing::debug!(
                uri = %uri,
                expected_client_version,
                "publish_diagnostics: document changed before phase 2 was published; skipping"
            );
        }
    }
}

/// Keep the project pass's record of published URIs in step with a per-file
/// publish. The project pass clears only URIs it recorded, so a file that
/// gained diagnostics here must be recorded and one that lost them dropped.
async fn record_project_publication(server: &AlServer, uri: &Url, has_diagnostics: bool) {
    if server.workspace.config.read().await.diagnostics_scope
        != al_project::config::DiagnosticsScope::Project
    {
        return;
    }
    let mut published = server.workspace_diagnostic_uris.lock().await;
    if has_diagnostics {
        published.insert(uri.clone());
    } else {
        published.remove(uri);
    }
}

/// Publish `diagnostics` for `uri` only while the document still holds `text`
/// at `client_version`.
///
/// The generation read lock spans the check and the publish. `did_close`
/// closes the document and sends its clearing publish under the write lock,
/// so a publish that passes the check here reaches the client before that
/// clear and can never land after it as a ghost on a closed document. The
/// check alone left a window in which a close on another worker thread ran
/// between the check and the send.
pub(crate) async fn publish_if_current(
    workspace: &al_workspace::Workspace,
    client: &tower_lsp::Client,
    uri: &Url,
    text: &Arc<String>,
    client_version: i32,
    diagnostics: Vec<Diagnostic>,
) -> bool {
    let generation = workspace.generation_lock.read().await;
    if !snapshot_is_current(workspace, uri, text, client_version) {
        return false;
    }
    client
        .publish_diagnostics(uri.clone(), diagnostics, Some(client_version))
        .await;
    drop(generation);
    true
}

fn document_snapshot_is_current(
    server: &AlServer,
    uri: &Url,
    expected_text: &Arc<String>,
    expected_client_version: i32,
) -> bool {
    snapshot_is_current(
        &server.workspace,
        uri,
        expected_text,
        expected_client_version,
    )
}

/// Whether the open document at `uri` is still `expected_text` at
/// `expected_client_version`.
pub(crate) fn snapshot_is_current(
    workspace: &al_workspace::Workspace,
    uri: &Url,
    expected_text: &Arc<String>,
    expected_client_version: i32,
) -> bool {
    workspace
        .documents
        .get_text_and_client_version(uri)
        .is_some_and(|(current_text, current_version)| {
            current_version == expected_client_version && Arc::ptr_eq(&current_text, expected_text)
        })
}

/// Run semantic analysis via .NET bridge if enabled. Returns diagnostics or empty vec.
///
/// Shared between `compute_diagnostics` (pull) and `publish_diagnostics` (push Phase 2).
async fn run_semantic_analysis(
    server: &AlServer,
    uri: &Url,
    text: &str,
) -> Option<Vec<Diagnostic>> {
    // The analyzers below are loaded into this process, so they come from the
    // trust decision as it stands now.
    server.refresh_trust().await;
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
        return Some(Vec::new());
    }
    if let Err(error) = server.await_semantic_workspace().await {
        if server.session.is_cancelled() {
            return None;
        }
        return Some(vec![semantic_pipeline_diagnostic(format!(
            "Microsoft semantic analysis could not start because workspace initialization failed: {error}"
        ))]);
    }

    let file_path = match uri.to_file_path() {
        Ok(path) => path,
        Err(()) => {
            return Some(vec![semantic_pipeline_diagnostic(format!(
                "Microsoft semantic analysis requires a local file URI, got {uri}"
            ))]);
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

    // The bridge starts from this toolchain, so without one there is no pass.
    let toolchain = server.workspace.toolchain.read().await.clone()?;
    let analyzers = match tokio::task::spawn_blocking(move || {
        resolve_semantic_analyzer_entries(
            &configured_analyzers,
            &project_root,
            &assembly_probing_paths,
            &toolchain.analyzers,
        )
    })
    .await
    {
        Ok(Ok(analyzers)) => analyzers,
        Ok(Err(error)) => return Some(vec![semantic_pipeline_diagnostic(error)]),
        Err(error) => {
            return Some(vec![semantic_pipeline_diagnostic(format!(
                "analyzer discovery worker failed: {error}"
            ))]);
        }
    };

    let guard = match server.get_or_init_bridge().await {
        Some(g) => g,
        None => return None,
    };
    let bridge = match guard.as_ref() {
        Some(b) => b,
        None => return None,
    };
    let bridge_generation = bridge.generation();

    // Inside its project, the file is compiled with the project's other files
    // and dependencies. A file outside it (a rendered symbol file, for one) is
    // compiled alone.
    let project_context =
        al_workspace::semantic_project_context(&server.workspace, &file_path).await;
    let (project_root, open_documents) = match project_context {
        Some(context) => (Some(context.root), context.open_documents),
        None => (None, Vec::new()),
    };

    let req = crate::semantic::AnalyzeRequest {
        file: file_path,
        source: text.to_string(),
        analyzers,
        package_cache,
        project_root,
        open_documents,
    };

    let semantic_start = std::time::Instant::now();
    let outcome = bridge.analyze(req).await;
    // Release the bridge read guard before anything below takes the bridge
    // lock again. `ensure_error_codes_loaded` goes back through
    // `get_or_init_bridge`, and tokio's RwLock is fair: a second read from this
    // task queues behind a restart or shutdown writer, which in turn waits for
    // this task's first read to end.
    drop(guard);
    match outcome {
        Ok(results) => {
            let semantic_elapsed = semantic_start.elapsed();
            tracing::debug!(uri = %uri, count = results.len(), elapsed_us = semantic_elapsed.as_micros() as u64, "semantic analysis complete");
            server.ensure_error_codes_loaded().await;

            results
                .iter()
                .map(|entry| semantic_entry_to_lsp(server, entry))
                .collect::<Vec<_>>()
                .into()
        }
        Err(error) => {
            if server.session.is_cancelled() {
                return None;
            }
            // Waiting behind another call, or the short pause after a
            // timeout: the bridge is fine, and the last results stay.
            if matches!(
                &error,
                crate::semantic::SemanticError::Busy(_)
                    | crate::semantic::SemanticError::Cooldown(_)
            ) {
                tracing::debug!(uri = %uri, %error, "semantic analysis deferred");
                return None;
            }
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
            Some(vec![semantic_pipeline_diagnostic(format!(
                "Microsoft semantic analysis failed: {error}"
            ))])
        }
    }
}

/// A bridge diagnostic as an LSP diagnostic, its message extended with the
/// error code's description from the compiler's catalog.
fn semantic_entry_to_lsp(
    server: &AlServer,
    entry: &crate::semantic::DiagnosticEntry,
) -> Diagnostic {
    let mut diag = semantic_to_diagnostic(entry);
    if let Some(desc) = server.error_code_description(&entry.code) {
        if !diag.message.contains(&desc) {
            diag.message = format!("{} — {}", diag.message, desc);
        }
    }
    diag
}

/// Compile and analyze the whole project in the background and store each
/// file's compiler and analyzer findings, then republish the project.
///
/// The per-file pass covers open files only. This pass gives every other file
/// the findings a build reports, as the Problems list of VS Code shows them.
/// It runs only with `diagnosticsScope: "project"`. An open file whose text
/// changed after the pass read it keeps its own newer findings.
pub(crate) async fn run_project_semantic_analysis(server: &AlServer) {
    server.refresh_trust().await;
    let (scope, enabled, configured_analyzers, assembly_probing_paths, configured_package_cache) = {
        let cfg = server.workspace.config.read().await;
        (
            cfg.diagnostics_scope,
            cfg.enable_code_analysis && cfg.background_code_analysis,
            cfg.code_analyzers.clone(),
            cfg.assembly_probing_paths.clone(),
            cfg.package_cache_path.clone(),
        )
    };
    if scope != al_project::config::DiagnosticsScope::Project || !enabled {
        return;
    }
    if server.await_semantic_workspace().await.is_err() || server.session.is_cancelled() {
        return;
    }
    let Some(project) = server.workspace.project.read().await.clone() else {
        return;
    };
    let Some(toolchain) = server.workspace.toolchain.read().await.clone() else {
        return;
    };
    let root = project.root.clone();
    let package_cache = configured_package_cache.unwrap_or_else(|| project.packages_dir.clone());
    let analyzer_root = root.clone();
    let analyzers = match tokio::task::spawn_blocking(move || {
        resolve_semantic_analyzer_entries(
            &configured_analyzers,
            &analyzer_root,
            &assembly_probing_paths,
            &toolchain.analyzers,
        )
    })
    .await
    {
        Ok(Ok(analyzers)) => analyzers,
        // The per-file pass reports a broken analyzer setting on the file.
        _ => return,
    };

    // Open buffers go with the request. Their versions decide afterwards which
    // results are still about the text on screen.
    let documents = &server.workspace.documents;
    let mut versions = std::collections::HashMap::new();
    let mut open = Vec::new();
    for uri in documents.open_uris() {
        let Some((text, version)) = documents.get_text_and_client_version(&uri) else {
            continue;
        };
        let Ok(path) = uri.to_file_path() else {
            continue;
        };
        versions.insert(uri, version);
        open.push((path, text));
    }
    let open_documents = al_workspace::project_open_documents(open, &root, Path::new(""));

    let started = std::time::Instant::now();
    let outcome = {
        let Some(guard) = server.get_or_init_bridge().await else {
            return;
        };
        let Some(bridge) = guard.as_ref() else {
            return;
        };
        bridge
            .analyze_project(crate::semantic::AnalyzeProjectRequest {
                project_root: root.clone(),
                package_cache,
                analyzers,
                open_documents,
            })
            .await
    };
    let entries = match outcome {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(%error, "project semantic analysis failed");
            return;
        }
    };
    server.ensure_error_codes_loaded().await;
    tracing::info!(
        findings = entries.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "project semantic analysis complete"
    );

    let mut by_file: std::collections::HashMap<Url, Vec<Diagnostic>> =
        std::collections::HashMap::new();
    for entry in &entries {
        if let Ok(uri) = Url::from_file_path(&entry.file) {
            by_file
                .entry(uri)
                .or_default()
                .push(semantic_entry_to_lsp(server, entry));
        }
    }
    // Every project file gets an entry, an empty one when it is clean, so a
    // finding fixed on disk leaves the list.
    let project_files: Vec<Url> = server
        .workspace
        .file_index
        .files
        .iter()
        .map(|entry| entry.key().clone())
        .filter(|path| path.starts_with(&root))
        .filter_map(|path| Url::from_file_path(path).ok())
        .collect();
    {
        let mut cache = server.semantic_diagnostic_cache.lock().await;
        for uri in project_files.into_iter().chain(by_file.keys().cloned()) {
            if is_cache_path(&uri) {
                continue;
            }
            let current = documents.get_client_version(&uri);
            if current.is_some() && current != versions.get(&uri).copied() {
                continue;
            }
            let diagnostics = by_file.get(&uri).cloned().unwrap_or_default();
            cache.insert(uri, CachedSemanticDiagnostics { diagnostics });
        }
    }
    publish_workspace_diagnostics(server).await;
}

fn resolve_semantic_analyzer_entries(
    configured: &[String],
    project_root: &Path,
    assembly_probing_paths: &[PathBuf],
    toolchain: &al_project::toolchain::AnalyzerPaths,
) -> Result<Vec<String>, String> {
    let search =
        al_project::analyzers::CustomAnalyzerSearch::new(project_root, assembly_probing_paths);
    configured
        .iter()
        .map(|entry| {
            // The bridge gets absolute paths only. A built-in name is the
            // toolchain's file and is never looked up anywhere else.
            if let Some(path) = al_project::analyzers::builtin_analyzer_path(toolchain, entry) {
                return path
                    .canonicalize()
                    .ok()
                    .filter(|path| path.is_file())
                    .map(|path| path.display().to_string())
                    .ok_or_else(|| {
                        format!(
                            "Requested built-in analyzer '{entry}' is not installed at {}",
                            path.display()
                        )
                    });
            }
            search
                .resolve(entry)
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
///
/// `None` for a diagnostic with no line: the run results named a test the
/// static discovery did not see, so there is nowhere to put the squiggle.
/// Anchoring it at line 0 drew a red mark on the `codeunit` header, which
/// reads as a fault in the declaration.
pub fn test_diag_to_lsp(
    td: &al_analysis::queries::test_diagnostics::TestDiagnostic,
) -> Option<Diagnostic> {
    use al_analysis::queries::test_diagnostics::DiagnosticSeverity as TDSev;

    let severity = match td.severity {
        TDSev::Error => DiagnosticSeverity::ERROR,
        TDSev::Warning => DiagnosticSeverity::WARNING,
        TDSev::Information => DiagnosticSeverity::INFORMATION,
        TDSev::Hint => DiagnosticSeverity::HINT,
    };

    let line = td.line?.saturating_sub(1); // 1-based → 0-based
    Some(Diagnostic {
        range: Range {
            start: Position { line, character: 0 },
            end: Position { line, character: 0 },
        },
        severity: Some(severity),
        code: Some(NumberOrString::String("AL-TEST".to_string())),
        source: Some("al-test-runner".to_string()),
        message: format!("[{}] {}", td.test_name, td.message),
        ..Default::default()
    })
}

/// Files the previous [`publish_test_diagnostics`] call published to.
///
/// LSP clears diagnostics by publishing an empty list to a URI, so a caller
/// that only wants to clear has no file list to work from. Remembering the
/// previous set is what makes "pass an empty slice to clear" actually clear;
/// without it an empty slice grouped to an empty map and the publish loop
/// never ran.
static LAST_TEST_DIAGNOSTIC_FILES: std::sync::Mutex<Vec<Url>> = std::sync::Mutex::new(Vec::new());

/// Publish test-result diagnostics for all affected files.
///
/// Groups the flat list by file and calls `publishDiagnostics` once per file.
/// A file that had diagnostics on the previous call and has none now is
/// published an empty list, so passing an empty `diagnostics` slice clears
/// every file the last call touched.
pub async fn publish_test_diagnostics(
    client: &tower_lsp::Client,
    diagnostics: &[al_analysis::queries::test_diagnostics::TestDiagnostic],
) {
    use al_analysis::queries::test_diagnostics::group_by_file;

    let grouped = group_by_file(diagnostics.to_vec());

    let mut published: Vec<Url> = Vec::new();
    for (file, tds) in grouped {
        let uri = match Url::from_file_path(&file) {
            Ok(u) => u,
            Err(_) => {
                tracing::warn!(file = %file, "publish_test_diagnostics: cannot convert path to URI");
                continue;
            }
        };
        let lsp_diags: Vec<Diagnostic> = tds.iter().filter_map(test_diag_to_lsp).collect();
        client
            .publish_diagnostics(uri.clone(), lsp_diags, None)
            .await;
        published.push(uri);
    }

    let stale: Vec<Url> = match LAST_TEST_DIAGNOSTIC_FILES.lock() {
        Ok(mut last) => {
            let stale = last
                .iter()
                .filter(|uri| !published.contains(uri))
                .cloned()
                .collect();
            *last = published;
            stale
        }
        Err(_) => Vec::new(),
    };
    for uri in stale {
        client.publish_diagnostics(uri, Vec::new(), None).await;
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
    fn native_findings_of_a_silenced_rule_are_dropped() {
        let finding = |code: &str| Diagnostic {
            code: Some(NumberOrString::String(code.to_string())),
            ..Diagnostic::default()
        };
        let mut config = al_project::config::AlConfig::default();
        config
            .native_lint_rules
            .insert("AL-NL007".to_string(), false);
        config
            .native_lint_rules
            .insert("AL-NL005".to_string(), true);
        let mut diagnostics = vec![finding("AL-NL007"), finding("AL-NL005"), finding("AA0218")];

        drop_silenced_native_rules(&mut diagnostics, &config);

        let codes: Vec<_> = diagnostics.iter().map(|d| d.code.clone()).collect();
        assert_eq!(
            codes,
            vec![
                Some(NumberOrString::String("AL-NL005".to_string())),
                Some(NumberOrString::String("AA0218".to_string())),
            ]
        );
    }

    /// Native rules that repeat a configured analyzer's rule were reported
    /// twice: every page field without a tooltip got AL-NL007 and CodeCop's
    /// AA0218 on the same line.
    #[test]
    fn a_native_rule_another_analyzer_covers_is_silenced_while_that_analyzer_runs() {
        let configured = [
            "${CodeCop}".to_string(),
            "${analyzerFolder}ALCops.PlatformCop.dll".to_string(),
        ];
        let mut covered = native_rules_covered_by(&configured);
        covered.sort_unstable();
        assert_eq!(covered, vec!["AL-NL005", "AL-NL007"]);

        assert!(native_rules_covered_by(&["UICop".to_string()]).is_empty());
        assert_eq!(
            native_rules_covered_by(&["codecop".to_string()]),
            vec!["AL-NL007"]
        );
    }

    /// Analyzer paths for a toolchain in `dir`, with CodeCop installed and the
    /// other cops missing.
    fn toolchain_analyzers(dir: &Path) -> al_project::toolchain::AnalyzerPaths {
        let code_cop = dir.join("Microsoft.Dynamics.Nav.CodeCop.dll");
        std::fs::write(&code_cop, b"toolchain CodeCop").unwrap();
        al_project::toolchain::AnalyzerPaths {
            code_cop,
            app_source_cop: dir.join("Microsoft.Dynamics.Nav.AppSourceCop.dll"),
            ui_cop: dir.join("Microsoft.Dynamics.Nav.UICop.dll"),
            per_tenant_cop: dir.join("Microsoft.Dynamics.Nav.PerTenantExtensionCop.dll"),
            common: dir.join("Microsoft.Dynamics.Nav.Analyzers.Common.dll"),
            custom: Vec::new(),
        }
    }

    /// The bridge loads the assembly at the path it is given into the language
    /// server. A built-in name has to reach it as the toolchain's own file:
    /// passed on by name, the bridge found a file called `CodeCop` or
    /// `CodeCop.dll` in the working directory, which is the project, before it
    /// looked in the toolchain.
    #[test]
    fn a_builtin_analyzer_name_resolves_to_the_toolchain_and_never_to_a_project_file() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("CodeCop.dll"), b"repository assembly").unwrap();
        std::fs::write(project.path().join("CodeCop"), b"repository assembly").unwrap();
        let toolchain = tempfile::tempdir().unwrap();
        let analyzers = toolchain_analyzers(toolchain.path());
        let toolchain_root = toolchain.path().canonicalize().unwrap();
        let project_root = project.path().canonicalize().unwrap();

        for entry in ["CodeCop", "CodeCop.dll", "${CodeCop}", "codecop.DLL"] {
            let resolved = resolve_semantic_analyzer_entries(
                &[entry.to_string()],
                project.path(),
                &[],
                &analyzers,
            )
            .unwrap_or_else(|error| panic!("{entry:?}: {error}"));
            let path = PathBuf::from(&resolved[0]);
            assert!(path.is_absolute(), "{entry:?} resolved to {path:?}");
            assert!(
                path.starts_with(&toolchain_root),
                "{entry:?} resolved to {path:?}, outside the toolchain"
            );
            assert!(
                !path.starts_with(&project_root),
                "{entry:?} resolved to {path:?}"
            );
        }
    }

    /// A built-in the toolchain does not ship is an error, and the entry is
    /// not looked up anywhere else.
    #[test]
    fn a_builtin_analyzer_missing_from_the_toolchain_is_an_error() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("UICop.dll"), b"repository assembly").unwrap();
        let toolchain = tempfile::tempdir().unwrap();
        let error = resolve_semantic_analyzer_entries(
            &["${UICop}".to_string()],
            project.path(),
            &[],
            &toolchain_analyzers(toolchain.path()),
        )
        .unwrap_err();
        assert!(error.contains("not installed"), "{error}");
    }

    /// The semantic bridge loads the resolved assemblies into the language
    /// server itself, so a name the user wrote must not resolve to a DLL an
    /// untrusted clone ships in `.netpackages` until the project is trusted.
    #[test]
    #[serial_test::serial]
    fn semantic_analyzer_resolution_preserves_builtins_and_needs_trust_for_project_copies() {
        let config = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", config.path());

        let project = tempfile::tempdir().unwrap();
        let dll = project
            .path()
            .join(".netpackages/businesscentral.lintercop/1.0.0/BusinessCentral.LinterCop.dll");
        std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
        std::fs::write(&dll, b"analyzer").unwrap();
        // The project's settings name the analyzer, so the trust record lists
        // the copy. A copy the record does not list is refused.
        std::fs::create_dir_all(project.path().join(".vscode")).unwrap();
        std::fs::write(
            project.path().join(".vscode/settings.json"),
            r#"{"al.codeAnalyzers": ["BusinessCentral.LinterCop"]}"#,
        )
        .unwrap();
        let requested = [
            "CodeCop".to_string(),
            "BusinessCentral.LinterCop".to_string(),
        ];

        let toolchain = tempfile::tempdir().unwrap();
        let analyzers = toolchain_analyzers(toolchain.path());

        let untrusted =
            resolve_semantic_analyzer_entries(&requested, project.path(), &[], &analyzers);
        let granted = al_project::trust::grant(project.path()).map(|_| ());
        let trusted =
            resolve_semantic_analyzer_entries(&requested, project.path(), &[], &analyzers);
        match previous {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }

        let error = untrusted.expect_err("the clone's copy must not be loaded");
        assert!(error.contains("not trusted"), "{error}");
        granted.unwrap();
        let resolved = trusted.unwrap();
        assert_eq!(
            resolved[0],
            analyzers
                .code_cop
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );
        assert_eq!(
            resolved[1],
            dll.canonicalize().unwrap().display().to_string()
        );
    }

    #[test]
    fn missing_requested_semantic_analyzer_is_explicit() {
        let project = tempfile::tempdir().unwrap();
        let toolchain = tempfile::tempdir().unwrap();
        let error = resolve_semantic_analyzer_entries(
            &["Missing.Custom.Analyzer".to_string()],
            project.path(),
            &[],
            &toolchain_analyzers(toolchain.path()),
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
            line: Some(10),
            severity: TDSev::Error,
            message: "Assert.AreEqual failed".to_string(),
            test_name: "TestSomething".to_string(),
            codeunit: "MyTests".to_string(),
        };
        let diag = test_diag_to_lsp(&td).expect("a located diagnostic");
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
            line: Some(5),
            severity: TDSev::Warning,
            message: "Test was skipped".to_string(),
            test_name: "TestSkipped".to_string(),
            codeunit: "MyTests".to_string(),
        };
        let diag = test_diag_to_lsp(&td).expect("a located diagnostic");
        assert_eq!(diag.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(diag.range.start.line, 4);
    }

    /// A diagnostic with no line is not published. Line 0 used to render as
    /// a red squiggle on the `codeunit` header, which reads as a fault in the
    /// declaration rather than a test whose source could not be located.
    #[test]
    fn a_diagnostic_without_a_line_is_not_published() {
        use al_analysis::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
        let td = TestDiagnostic {
            file: "/src/Tests.al".to_string(),
            line: None,
            severity: TDSev::Error,
            message: "fail".to_string(),
            test_name: "T".to_string(),
            codeunit: "CU".to_string(),
        };
        assert!(test_diag_to_lsp(&td).is_none());
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
            line: Some(3),
            severity: sev,
            message: "m".to_string(),
            test_name: "T".to_string(),
            codeunit: "CU".to_string(),
        };
        assert_eq!(
            test_diag_to_lsp(&mk(TDSev::Information))
                .expect("a located diagnostic")
                .severity,
            Some(DiagnosticSeverity::INFORMATION)
        );
        assert_eq!(
            test_diag_to_lsp(&mk(TDSev::Hint))
                .expect("a located diagnostic")
                .severity,
            Some(DiagnosticSeverity::HINT)
        );
    }

    #[test]
    fn test_diag_message_and_code_format() {
        use al_analysis::queries::test_diagnostics::{DiagnosticSeverity as TDSev, TestDiagnostic};
        let td = TestDiagnostic {
            file: "/src/Tests.al".to_string(),
            line: Some(7),
            severity: TDSev::Error,
            message: "expected 1 got 2".to_string(),
            test_name: "MyTest".to_string(),
            codeunit: "MyTests".to_string(),
        };
        let diag = test_diag_to_lsp(&td).expect("a located diagnostic");
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

    #[tokio::test]
    async fn cancelled_session_never_publishes_a_workspace_generation() {
        let (service, _socket) = tower_lsp::LspService::new(super::super::AlLsp::new);
        let server = service.inner();
        let session = server.session.clone();
        session.cancel();

        let published = publish_workspace_diagnostics_parts(
            std::sync::Arc::clone(&server.workspace),
            server.client.clone(),
            std::sync::Arc::clone(&server.semantic_diagnostic_cache),
            std::sync::Arc::clone(&server.workspace_diagnostic_uris),
            None,
            &session,
        )
        .await;

        assert!(!published);
        assert!(server.workspace_diagnostic_uris.lock().await.is_empty());
    }
}
