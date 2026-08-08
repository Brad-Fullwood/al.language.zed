//! `workspace/executeCommand` implementations — one function per `al.*`
//! command.
//!
//! `AlServer::execute_command` in `lsp.rs` is a thin dispatch table over
//! these. Each command is independently readable and testable instead of
//! living in one ~350-line match expression.

use std::sync::Arc;

use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticSeverity, FormattingOptions, Location, MessageType, NumberOrString,
    Position, Range, Url, WorkspaceEdit,
};

use super::lsp::{AlServer, LspSessionState};
use super::{diagnostics, formatting, workspace};

/// `al.clearSymbolCache` — remove the on-disk virtual-file cache.
pub(super) async fn clear_symbol_cache(server: &AlServer) {
    let cache_dir = al_symbols::virtual_file::cache_dir();
    match tokio::fs::remove_dir_all(&cache_dir).await {
        Ok(()) => {
            tracing::info!(path = ?cache_dir, "Cleared symbol cache");
            server
                .client
                .show_message(MessageType::INFO, "Symbol cache cleared")
                .await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to clear symbol cache");
            server
                .client
                .show_message(MessageType::WARNING, format!("Failed to clear cache: {e}"))
                .await;
        }
    }
}

/// `al.formatFile` — format a document and apply the edit via the client.
///
/// Formatting is normally a CodeAction with a WorkspaceEdit (handlers.rs);
/// this command is kept for backward compatibility and direct calls.
pub(super) async fn format_file(server: &AlServer, arguments: &[serde_json::Value]) {
    let uri = match arguments.first().cloned().map(serde_json::from_value) {
        Some(Ok(uri)) => uri,
        Some(Err(error)) => {
            tracing::warn!(%error, "al.formatFile received an invalid URI");
            server
                .client
                .show_message(
                    MessageType::WARNING,
                    format!("Format failed: invalid URI: {error}"),
                )
                .await;
            return;
        }
        None => {
            server
                .client
                .show_message(
                    MessageType::WARNING,
                    "Format failed: no document URI supplied",
                )
                .await;
            return;
        }
    };
    let Some(edits) = formatting::handle_formatting(
        server,
        &uri,
        &FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
    ) else {
        return;
    };
    let mut changes = std::collections::HashMap::new();
    changes.insert(uri.clone(), edits);
    match server
        .client
        .apply_edit(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        })
        .await
    {
        Ok(resp) if resp.applied => {}
        Ok(resp) => {
            let reason = resp
                .failure_reason
                .unwrap_or_else(|| "edit rejected by editor".to_string());
            tracing::warn!(uri = %uri, reason = %reason, "al.formatFile: apply_edit not applied");
            server
                .client
                .show_message(MessageType::WARNING, format!("Format failed: {reason}"))
                .await;
        }
        Err(e) => {
            tracing::warn!(uri = %uri, error = %e, "al.formatFile: apply_edit transport error");
            server
                .client
                .show_message(MessageType::WARNING, format!("Format failed: {e}"))
                .await;
        }
    }
}

/// `al.lintFile` — re-publish diagnostics for an open document.
pub(super) async fn lint_file(server: &AlServer, arguments: &[serde_json::Value]) {
    let uri = match arguments.first().cloned().map(serde_json::from_value) {
        Some(Ok(uri)) => uri,
        Some(Err(error)) => {
            tracing::warn!(%error, "al.lintFile received an invalid URI");
            server
                .client
                .show_message(
                    MessageType::WARNING,
                    format!("Lint failed: invalid URI: {error}"),
                )
                .await;
            return;
        }
        None => {
            server
                .client
                .show_message(
                    MessageType::WARNING,
                    "Lint failed: no document URI supplied",
                )
                .await;
            return;
        }
    };
    if let Some((text, version)) = server.workspace.documents.get_text_and_client_version(&uri) {
        diagnostics::publish_diagnostics(server, &uri, text, version).await;
    } else {
        tracing::warn!(%uri, "al.lintFile document is not open");
        server
            .client
            .show_message(
                MessageType::WARNING,
                format!("Lint failed: document is not open: {uri}"),
            )
            .await;
    }
}

/// `al.getStatus` — health/inventory snapshot for the status command.
pub(super) async fn get_status(server: &AlServer) -> Result<serde_json::Value, String> {
    let has_bridge = server.workspace.semantic.read().await.is_some();
    let has_toolchain = server.workspace.toolchain.read().await.is_some();
    let indexed_symbols = server.workspace.symbols.len();
    let workspace_files = server.workspace.file_index.len();
    let workspace_objects = server.workspace.file_index.object_count();
    let builtins = server
        .workspace
        .builtins
        .read()
        .map_err(|_| "workspace builtin type state is poisoned".to_string())?
        .len();
    let (workspace_state, workspace_error) = match server.workspace_init_state.borrow().clone() {
        super::WorkspaceInitState::Initializing => ("initializing", None),
        super::WorkspaceInitState::Ready => ("ready", None),
        super::WorkspaceInitState::Failed(error) => ("failed", Some(error)),
    };

    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "workspaceState": workspace_state,
        "workspaceError": workspace_error,
        "semanticBridge": has_bridge,
        "toolchain": has_toolchain,
        "indexedSymbols": indexed_symbols,
        "workspaceFiles": workspace_files,
        "workspaceObjects": workspace_objects,
        "builtinTypes": builtins,
    }))
}

/// `al.reindex` — re-run workspace initialization in the background,
/// aborting any in-flight previous reindex.
pub(super) async fn reindex(server: &AlServer) {
    let root_uri = server.root_uri.read().await.clone();
    if let Some(uri) = &root_uri {
        tracing::info!(root = %uri, "Reindexing workspace (background)");
        let ws = Arc::clone(&server.workspace);
        let client = server.client.clone();
        let uri_cloned = uri.clone();
        let diagnostic_state = server.diagnostic_publication_state();
        let session = diagnostic_state.session.clone();
        let handle = tokio::spawn(async move {
            match workspace::initialize_workspace(
                ws,
                client.clone(),
                Some(uri_cloned),
                None,
                Some(diagnostic_state),
            )
            .await
            {
                Ok(()) => {
                    if !session.is_cancelled() {
                        client
                            .show_message(MessageType::INFO, "Workspace reindex complete")
                            .await;
                    }
                }
                Err(error) => {
                    if !session.is_cancelled() {
                        client
                            .show_message(
                                MessageType::ERROR,
                                format!("Workspace reindex failed: {error}"),
                            )
                            .await;
                    }
                }
            }
        });
        // Cancel any previous in-flight reindex so rapid clicks
        // don't run two full scans concurrently.
        let mut slot = server.reindex_task.lock().await;
        if let Some(prev) = slot.replace(handle) {
            prev.abort();
        }
    } else {
        server
            .client
            .show_message(MessageType::WARNING, "No workspace root — cannot reindex")
            .await;
    }
}

/// `al.compile` — build the project and publish per-file diagnostics as
/// publishDiagnostics when the selected compiler path produces them.
///
/// The default path is the pure-Rust native `.app` emitter. Setting
/// `al.useOfficialCompiler=true` opts into Microsoft's `dotnet alc`
/// subprocess for compiler diagnostics and authoritative validation.
pub(super) async fn compile(server: &AlServer) {
    let project = server.workspace.project.read().await;
    let Some((root, package_cache, dependency_packages)) = project.as_ref().map(|project| {
        (
            project.root.clone(),
            project.packages_dir.clone(),
            project.packages.clone(),
        )
    }) else {
        drop(project);
        server
            .client
            .show_message(
                MessageType::WARNING,
                "No AL project loaded — open an AL workspace first",
            )
            .await;
        return;
    };
    drop(project);

    let config = server.workspace.config.read().await.clone();
    let backend =
        al_compile::BuildBackend::from_use_official_compiler(config.use_official_compiler);
    let toolchain = if backend == al_compile::BuildBackend::Alc {
        let toolchain = server.workspace.toolchain.read().await.clone();
        let Some(toolchain) = toolchain else {
            server
                .client
                .show_message(
                    MessageType::WARNING,
                    "No AL toolchain configured — run 'al setup' first",
                )
                .await;
            return;
        };
        Some(toolchain)
    } else {
        None
    };

    let mut workspace_diagnostics = if backend == al_compile::BuildBackend::Native {
        native_workspace_compile_diagnostics(server, &config, &root)
    } else {
        Vec::new()
    };
    let has_errors = workspace_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == al_compile::DiagnosticSeverity::Error);
    let mut result = if has_errors {
        al_compile::CompileResult {
            success: false,
            app_path: None,
            diagnostics: Vec::new(),
            output: "native workspace semantic validation failed; .app was not emitted".to_string(),
            timings: None,
        }
    } else {
        match al_compile::build(al_compile::BuildRequest {
            project_root: &root,
            backend,
            toolchain: toolchain.as_ref(),
            dependency_packages: Some(dependency_packages.as_slice()),
            package_cache: Some(&package_cache),
            analyzers: if config.code_analyzers.is_empty() {
                None
            } else {
                Some(config.code_analyzers.as_slice())
            },
            config: al_compile::CompilationConfigOptions::from(&config),
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                server
                    .client
                    .show_message(
                        MessageType::ERROR,
                        format!("Compilation infrastructure error: {error}"),
                    )
                    .await;
                return;
            }
        }
    };
    result.diagnostics.append(&mut workspace_diagnostics);
    publish_compile_result(server, &root, &result).await;
}

fn native_workspace_compile_diagnostics(
    server: &AlServer,
    config: &al_project::config::AlConfig,
    project_root: &std::path::Path,
) -> Vec<al_compile::CompileDiagnostic> {
    al_analysis::queries::diagnostics::native_workspace_diagnostics_at_root(
        &server.workspace,
        config,
        Some(project_root),
    )
    .into_iter()
    .map(|(path, diagnostic)| al_compile::CompileDiagnostic {
        file: path.display().to_string(),
        line: diagnostic.range.start.line + 1,
        column: diagnostic.range.start.character + 1,
        end_line: Some(diagnostic.range.end.line + 1),
        end_column: Some(diagnostic.range.end.character + 1),
        severity: match diagnostic.severity {
            al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Error => {
                al_compile::DiagnosticSeverity::Error
            }
            al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Warning => {
                al_compile::DiagnosticSeverity::Warning
            }
            al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Info
            | al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Hint => {
                al_compile::DiagnosticSeverity::Info
            }
        },
        code: diagnostic.code,
        message: diagnostic.message,
    })
    .collect()
}

/// Convert compiler diagnostics into per-file LSP diagnostics, resolving
/// relative paths against `root`. Pure (no server / I/O) so the severity
/// mapping, the 1-based→0-based position conversion, the end-of-range (start of
/// next line per LSP, not the old u32::MAX sentinel), and the relative-to-
/// absolute path resolution are all unit-testable.
fn group_compile_diagnostics(
    diagnostics: &[al_compile::CompileDiagnostic],
    root: &std::path::Path,
) -> std::collections::HashMap<String, Vec<Diagnostic>> {
    let mut by_file: std::collections::HashMap<String, Vec<Diagnostic>> =
        std::collections::HashMap::new();
    for d in diagnostics {
        let severity = match d.severity {
            al_compile::DiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
            al_compile::DiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
            al_compile::DiagnosticSeverity::Info => DiagnosticSeverity::INFORMATION,
        };
        let start_line = d.line.saturating_sub(1);
        let start_char = d.column.saturating_sub(1);
        let lsp_diag = Diagnostic {
            range: Range {
                start: Position {
                    line: start_line,
                    character: start_char,
                },
                end: match (d.end_line, d.end_column) {
                    (Some(line), Some(character)) => Position {
                        line: line.saturating_sub(1),
                        character: character.saturating_sub(1),
                    },
                    // `alc` text diagnostics carry only a start. The LSP-spec
                    // representation for the rest of that line is the start
                    // of the next line.
                    _ => Position {
                        line: start_line.saturating_add(1),
                        character: 0,
                    },
                },
            },
            severity: Some(severity),
            code: Some(NumberOrString::String(d.code.clone())),
            source: Some("al-compiler".to_string()),
            message: d.message.clone(),
            ..Default::default()
        };
        // compilers may emit relative paths (`src/Foo.al`) when run from
        // project_root. Url::from_file_path requires an absolute path, so
        // resolve relative entries against the root before grouping; otherwise
        // the per-file URI conversion silently drops the diagnostic.
        let abs_path = {
            let p = std::path::Path::new(&d.file);
            if p.is_absolute() {
                d.file.clone()
            } else {
                root.join(p).to_string_lossy().into_owned()
            }
        };
        by_file.entry(abs_path).or_default().push(lsp_diag);
    }
    by_file
}

/// Publish compiler diagnostics per file and clear squiggles for files
/// that were affected last compile but are clean now.
async fn publish_compile_result(
    server: &AlServer,
    root: &std::path::Path,
    result: &al_compile::CompileResult,
) {
    let by_file = group_compile_diagnostics(&result.diagnostics, root);
    let current_affected: std::collections::HashSet<String> = by_file.keys().cloned().collect();
    for (file, diags) in by_file {
        if let Ok(uri) = Url::from_file_path(&file) {
            server.client.publish_diagnostics(uri, diags, None).await;
        }
    }

    // clear compiler diagnostics for files that were
    // affected last compile but are clean now. We re-publish
    // syntax/lint diagnostics if the file is open (so
    // existing squiggles stay), or an empty list otherwise.
    let last = server.workspace.last_compile_affected.lock().await;
    let stale: Vec<String> = last.difference(&current_affected).cloned().collect();
    drop(last);
    for file in &stale {
        if let Ok(uri) = Url::from_file_path(file) {
            if let Some((text, version)) =
                server.workspace.documents.get_text_and_client_version(&uri)
            {
                diagnostics::publish_diagnostics(server, &uri, text, version).await;
            } else {
                server
                    .client
                    .publish_diagnostics(uri, Vec::new(), None)
                    .await;
            }
        }
    }
    *server.workspace.last_compile_affected.lock().await = current_affected;

    if result.success {
        server
            .client
            .show_message(MessageType::INFO, "Compilation succeeded")
            .await;
    } else {
        let mut message = "Compilation failed".to_string();
        let output = result.output.trim();
        if !output.is_empty() {
            let first_line = output.lines().next().unwrap_or(output);
            message.push_str(": ");
            message.push_str(first_line);
        }
        server
            .client
            .show_message(MessageType::ERROR, message)
            .await;
    }
}

/// `al.applyRecommendedSettings` — write the recommended Zed settings.
pub(super) async fn apply_recommended_settings(server: &AlServer) {
    let result = tokio::task::spawn_blocking(crate::server::workspace::apply_recommended_settings)
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    match result {
        Ok(()) => {
            server
                .client
                .show_message(
                    MessageType::INFO,
                    "Applied recommended AL settings. Reload Zed to activate.",
                )
                .await;
        }
        Err(e) => {
            server
                .client
                .show_message(
                    MessageType::WARNING,
                    format!("Failed to apply settings: {e}"),
                )
                .await;
        }
    }
}

/// `al.findReferences` — resolve the references for the symbol the CodeLens
/// sits on and return them as LSP `Location[]`.
///
/// The lens passes `{ "uri", "position" }`; we run the same workspace-wide
/// reference search as `textDocument/references`. Always returns a JSON array
/// (empty when nothing is found) so the client can present the results — and so
/// the click is never the silent no-op the catch-all dispatch arm produced.
pub(super) async fn find_references(
    server: &AlServer,
    arguments: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let arg = arguments.first();
    let uri = arg
        .and_then(|v| v.get("uri"))
        .and_then(|v| serde_json::from_value::<Url>(v.clone()).ok());
    let position = arg
        .and_then(|v| v.get("position"))
        .and_then(|v| serde_json::from_value::<Position>(v.clone()).ok());
    let (Some(uri), Some(position)) = (uri, position) else {
        return Err("al.findReferences requires { uri, position } arguments".to_string());
    };
    // Same workspace-wide walk as `textDocument/references`, so it gets the
    // same treatment: run it on the blocking pool instead of stalling the async
    // executor that drives every other request.
    let workspace = std::sync::Arc::clone(&server.workspace);
    let locations = tokio::task::spawn_blocking(move || {
        al_analysis::queries::references::references(&workspace, &uri, position.into(), false)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("al.findReferences worker failed: {error}"))??;
    let lsp_locations: Vec<Location> = locations.into_iter().map(Into::into).collect();
    serde_json::to_value(lsp_locations)
        .map_err(|error| format!("serializing al.findReferences result failed: {error}"))
}

/// `al.showProfiler` — return the active `.alcpuprofile` session's hotspots so
/// the client can surface them. When no profile is loaded the result
/// is `{ "active": false, "hints": [] }` rather than a silent no-op. The
/// profiler lens is only emitted while a session is active, so the inactive
/// branch is defensive.
pub(super) fn show_profiler(
    server: &AlServer,
    _arguments: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let guard = server
        .workspace
        .profiler_session
        .read()
        .map_err(|_| "profiler session state is poisoned".to_string())?;
    Ok(match guard.as_ref() {
        Some(session) if session.is_active() => serde_json::json!({
            "active": true,
            "profilePath": session.profile_path,
            "hints": session.hints,
        }),
        _ => serde_json::json!({ "active": false, "hints": [] }),
    })
}

/// `al.runTest` — run the `[Test]` procedure the lens targets against the
/// configured BC server.
///
/// The lens passes a [`al_analysis::queries::code_lens::TestTarget`] (codeunit id +
/// method). Running BC tests requires a launch configuration (`.zed/debug.json`
/// or `.vscode/launch.json`); when one is present we kick off the run in the
/// background (mirroring the daemon's `tests.run` flow) and report pass/fail via
/// a client message, persisting the result so the lens refreshes. When no server
/// is configured we tell the user how to set one up instead of doing nothing.
///
/// Returns a small status object so callers/automation can observe the routing:
/// `{ "status": "started" | "noServer" | "invalidArgs", "target"?: {..} }`.
pub(super) async fn run_test(
    server: &AlServer,
    arguments: &[serde_json::Value],
) -> serde_json::Value {
    use al_analysis::queries::code_lens::TestTarget;

    let Some(target) = arguments
        .first()
        .and_then(|v| serde_json::from_value::<TestTarget>(v.clone()).ok())
    else {
        server
            .client
            .show_message(MessageType::WARNING, "Run test: no test target supplied")
            .await;
        return serde_json::json!({ "status": "invalidArgs" });
    };

    let project_root = server
        .workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|p| p.root.clone());
    let config = match project_root.as_deref() {
        Some(root) => match al_bc::launch::find_launch_config(root) {
            Ok(file) => file.and_then(|df| df.configs.into_iter().next()),
            Err(error) => {
                server
                    .client
                    .show_message(MessageType::ERROR, format!("Cannot run test: {error}"))
                    .await;
                return serde_json::json!({
                    "status": "configurationError",
                    "target": target,
                    "error": error.to_string(),
                });
            }
        },
        None => None,
    };

    let Some(config) = config else {
        server
            .client
            .show_message(
                MessageType::WARNING,
                format!(
                    "Cannot run test '{}' (codeunit {}): no BC server configured in \
                     .zed/debug.json or .vscode/launch.json.",
                    target.method_name, target.codeunit_id
                ),
            )
            .await;
        return serde_json::json!({ "status": "noServer", "target": target });
    };

    server
        .client
        .show_message(
            MessageType::INFO,
            format!(
                "Running test '{}' (codeunit {})…",
                target.method_name, target.codeunit_id
            ),
        )
        .await;

    // Run off the request path: the BC round-trip can take several seconds.
    let workspace = Arc::clone(&server.workspace);
    let client = server.client.clone();
    let session = server.session.clone();
    let target_for_task = target.clone();
    tokio::spawn(async move {
        run_test_background(workspace, client, session, config, target_for_task).await;
    });

    serde_json::json!({ "status": "started", "target": target })
}

/// Background worker for [`run_test`]: executes the codeunit/method against the
/// BC dev test API, reports the outcome, and persists per-method records so the
/// CodeLens status updates on the next refresh.
async fn run_test_background(
    workspace: Arc<al_workspace::Workspace>,
    client: tower_lsp::Client,
    session: LspSessionState,
    config: al_bc::launch::BcServerConfig,
    target: al_analysis::queries::code_lens::TestTarget,
) {
    if session.is_cancelled() {
        return;
    }
    let codeunit_name = format!("Codeunit {}", target.codeunit_id);
    let runner = match al_test::test_runner::TestRunnerClient::new(&config) {
        Ok(runner) => runner,
        Err(error) => {
            if !session.is_cancelled() {
                client
                    .show_message(
                        MessageType::ERROR,
                        format!("Could not construct the Business Central test client: {error}"),
                    )
                    .await;
            }
            return;
        }
    };
    let result = runner
        .run_codeunit(
            target.codeunit_id,
            &codeunit_name,
            Some(&target.method_name),
        )
        .await;
    if session.is_cancelled() {
        return;
    }

    let result = match result {
        Ok(r) => r,
        Err(e) => {
            if !session.is_cancelled() {
                client
                    .show_message(
                        MessageType::ERROR,
                        format!("Test '{}' failed to run: {e}", target.method_name),
                    )
                    .await;
            }
            return;
        }
    };

    // Persist so CodeLens / tests.last_results / the TUI agree, but only if a
    // results store is already initialised — we don't create one here.
    if let Some(store) = workspace.test_results.read().ok().and_then(|g| g.clone()) {
        let timestamp = match al_test::persistence::now_secs() {
            Ok(timestamp) => timestamp,
            Err(error) => {
                if !session.is_cancelled() {
                    client
                        .show_message(
                            MessageType::ERROR,
                            format!("Could not timestamp test results: {error}"),
                        )
                        .await;
                }
                return;
            }
        };
        for m in &result.methods {
            let rec = al_test::persistence::TestRunRecord {
                timestamp,
                codeunit_id: result.id,
                codeunit_name: result.name.clone(),
                method_name: m.name.clone(),
                status: m.status.clone(),
                duration_ms: m.duration_ms,
                error: m.error.clone(),
            };
            if let Err(e) = store.append(rec).await {
                tracing::warn!(error = %e, "failed to persist test result");
            }
        }
    }

    if session.is_cancelled() {
        return;
    }
    let level = if result.failed > 0 {
        MessageType::ERROR
    } else {
        MessageType::INFO
    };
    client
        .show_message(
            level,
            format!(
                "Test run complete: {} passed, {} failed, {} skipped",
                result.passed, result.failed, result.skipped
            ),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_compile::{CompileDiagnostic, DiagnosticSeverity as S};

    fn diag(file: &str, line: u32, column: u32, sev: S, code: &str) -> CompileDiagnostic {
        CompileDiagnostic {
            file: file.to_string(),
            line,
            column,
            end_line: None,
            end_column: None,
            severity: sev,
            code: code.to_string(),
            message: format!("{code} message"),
        }
    }

    #[test]
    fn maps_severity_and_converts_1based_position_to_0based_lsp_range() {
        let root = std::path::Path::new("/proj");
        let by_file =
            group_compile_diagnostics(&[diag("/proj/src/A.al", 5, 3, S::Error, "AL0118")], root);
        let v = by_file
            .get("/proj/src/A.al")
            .expect("absolute file grouped");
        assert_eq!(v.len(), 1);
        let d = &v[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        // 1-based (5,3) → 0-based (4,2); end is the start of the next line.
        assert_eq!(
            d.range.start,
            Position {
                line: 4,
                character: 2
            }
        );
        assert_eq!(
            d.range.end,
            Position {
                line: 5,
                character: 0
            }
        );
        assert_eq!(d.source.as_deref(), Some("al-compiler"));
        assert_eq!(d.code, Some(NumberOrString::String("AL0118".to_string())));
        assert_eq!(d.message, "AL0118 message");
    }

    #[test]
    fn resolves_relative_paths_against_root_and_saturates_zero_positions() {
        let root = std::path::Path::new("/proj");
        // line/column 1 must convert to 0 (saturating_sub), not underflow.
        let by_file =
            group_compile_diagnostics(&[diag("src/Rel.al", 1, 1, S::Warning, "AL0001")], root);
        let v = by_file
            .get("/proj/src/Rel.al")
            .expect("relative path resolved against root");
        assert_eq!(
            v[0].range.start,
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(v[0].severity, Some(DiagnosticSeverity::WARNING));
    }

    #[test]
    fn preserves_exact_native_end_position() {
        let root = std::path::Path::new("/proj");
        let mut diagnostic = diag("/proj/src/A.al", 5, 3, S::Error, "ALN0001");
        diagnostic.end_line = Some(7);
        diagnostic.end_column = Some(11);
        let by_file = group_compile_diagnostics(&[diagnostic], root);
        assert_eq!(
            by_file["/proj/src/A.al"][0].range.end,
            Position {
                line: 6,
                character: 10
            }
        );
    }

    #[test]
    fn buckets_multiple_diagnostics_per_file_and_maps_info() {
        let root = std::path::Path::new("/proj");
        let by_file = group_compile_diagnostics(
            &[
                diag("/proj/A.al", 1, 1, S::Error, "E1"),
                diag("/proj/A.al", 2, 1, S::Info, "I1"),
                diag("/proj/B.al", 1, 1, S::Error, "E2"),
            ],
            root,
        );
        assert_eq!(by_file.get("/proj/A.al").map(Vec::len), Some(2));
        assert_eq!(by_file.get("/proj/B.al").map(Vec::len), Some(1));
        assert!(by_file
            .get("/proj/A.al")
            .unwrap()
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::INFORMATION)));
    }
}
