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

use super::lsp::AlServer;
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
        // A successful reindex sets the state to Ready, which also ends a
        // Failed state from the first initialization.
        let init_state = server.workspace_init_state.clone();
        let handle = tokio::spawn(async move {
            match workspace::initialize_workspace(
                ws,
                client.clone(),
                Some(uri_cloned),
                Some(init_state),
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
/// The default is Microsoft's `dotnet alc` when the toolchain is found and the
/// pure-Rust native `.app` emitter otherwise. `al.useOfficialCompiler` forces
/// either one.
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
    let found_toolchain = server.workspace.toolchain.read().await.clone();
    let backend = al_compile::BuildBackend::from_use_official_compiler(
        config.use_official_compiler,
        found_toolchain.is_some(),
    );
    if al_compile::BuildBackend::is_auto_fallback(
        config.use_official_compiler,
        found_toolchain.is_some(),
    ) {
        tracing::info!("{}", al_compile::BuildBackend::NATIVE_FALLBACK_NOTE);
    }
    let toolchain = if backend == al_compile::BuildBackend::Alc {
        let toolchain = found_toolchain;
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
        let analyzers = config.effective_code_analyzers(&root);
        match al_compile::build(al_compile::BuildRequest {
            project_root: &root,
            backend,
            toolchain: toolchain.as_ref(),
            dependency_packages: Some(dependency_packages.as_slice()),
            package_cache: Some(&package_cache),
            analyzers: Some(analyzers.as_slice()),
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
        Ok(backup) => {
            server
                .client
                .show_message(
                    MessageType::INFO,
                    crate::server::workspace::applied_settings_message(backup.as_deref()),
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
    go_to(server, "reference", &lsp_locations).await;
    serde_json::to_value(lsp_locations)
        .map_err(|error| format!("serializing al.findReferences result failed: {error}"))
}

/// The `{ uri, position }` argument a lens passes to its command.
fn lens_target(arguments: &[serde_json::Value], command: &str) -> Result<(Url, Position), String> {
    let arg = arguments.first();
    let uri = arg
        .and_then(|v| v.get("uri"))
        .and_then(|v| serde_json::from_value::<Url>(v.clone()).ok());
    let position = arg
        .and_then(|v| v.get("position"))
        .and_then(|v| serde_json::from_value::<Position>(v.clone()).ok());
    match (uri, position) {
        (Some(uri), Some(position)) => Ok((uri, position)),
        _ => Err(format!("{command} requires {{ uri, position }} arguments")),
    }
}

/// `al.showSubscribers` — go to the subscribers of the event declared at the
/// lens: straight there for one, a list to choose from for several.
pub(super) async fn show_subscribers(
    server: &AlServer,
    arguments: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let (uri, position) = lens_target(arguments, "al.showSubscribers")?;
    let workspace = std::sync::Arc::clone(&server.workspace);
    let locations = tokio::task::spawn_blocking(move || {
        let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, &uri)
            .ok_or_else(|| format!("{uri} is not open"))?;
        match al_analysis::queries::events::event_role(&text, &tree, position.into()) {
            Some(al_analysis::queries::events::EventRole::Publisher { object, event }) => Ok(
                al_analysis::queries::events::subscriber_locations(&workspace, &object, &event),
            ),
            _ => Err("the lens is not on an event publisher".to_string()),
        }
    })
    .await
    .map_err(|error| format!("al.showSubscribers worker failed: {error}"))??;
    let lsp_locations: Vec<Location> = locations.into_iter().map(Into::into).collect();
    go_to(server, "subscriber", &lsp_locations).await;
    serde_json::to_value(lsp_locations)
        .map_err(|error| format!("serializing al.showSubscribers result failed: {error}"))
}

/// `al.showEventSource` — go to the declaration of the event the subscriber
/// at the lens handles, in the workspace or a package's extracted source.
pub(super) async fn show_event_source(
    server: &AlServer,
    arguments: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let (uri, position) = lens_target(arguments, "al.showEventSource")?;
    let workspace = std::sync::Arc::clone(&server.workspace);
    let location = tokio::task::spawn_blocking(move || {
        al_analysis::queries::events::publisher_location(&workspace, &uri, position.into())
    })
    .await
    .map_err(|error| format!("al.showEventSource worker failed: {error}"))?;
    let lsp_locations: Vec<Location> = location.into_iter().map(Into::into).collect();
    go_to(server, "event declaration", &lsp_locations).await;
    serde_json::to_value(lsp_locations)
        .map_err(|error| format!("serializing al.showEventSource result failed: {error}"))
}

/// How long a report may run before it is abandoned.
const REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The `al-explorer` arguments for a report on `object` declared in `file`.
fn report_arguments(report: &str, object: &str, file: &std::path::Path) -> Option<Vec<String>> {
    let args = match report {
        "composed" => vec!["composed".to_string(), object.to_string()],
        "impact" => vec!["impact".to_string(), object.to_string()],
        "suggest-event" => vec![
            "suggest-event".to_string(),
            "--object".to_string(),
            object.to_string(),
        ],
        "metrics" => vec!["metrics".to_string(), file.display().to_string()],
        _ => return None,
    };
    Some(args)
}

/// A report file name for `object`: letters and digits kept, the rest `_`.
fn report_file_name(report: &str, object: &str) -> String {
    let object: String = object
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{report}-{object}.txt")
}

/// `al.showReport` — run the `al-explorer` report the old editor task ran,
/// for the object the code action was offered on, save the output under the
/// cache folder and open it in the editor.
/// The `al-explorer` report process. It is killed when the future that waits
/// for it is dropped, which is what the timeout in [`show_report`] does.
fn report_command(
    explorer: &std::path::Path,
    args: &[String],
    cwd: &std::path::Path,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(explorer);
    command.args(args).current_dir(cwd).kill_on_drop(true);
    command
}

pub(super) async fn show_report(
    server: &AlServer,
    arguments: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let arg = arguments
        .first()
        .ok_or("al.showReport requires { report, object, uri }")?;
    let report = arg
        .get("report")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let object = arg
        .get("object")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let uri = arg
        .get("uri")
        .and_then(|v| serde_json::from_value::<Url>(v.clone()).ok())
        .ok_or("al.showReport requires a file uri")?;
    let file = uri
        .to_file_path()
        .map_err(|()| format!("al.showReport needs a local file, got {uri}"))?;
    let args = report_arguments(report, object, &file)
        .ok_or_else(|| format!("unknown report '{report}'"))?;

    let explorer = super::lsp::resolve_explorer_binary()?;
    let cwd = match server.workspace.project.read().await.as_ref() {
        Some(project) => project.root.clone(),
        None => file
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default(),
    };
    let run = report_command(&explorer, &args, &cwd).output();
    let output = tokio::time::timeout(REPORT_TIMEOUT, run)
        .await
        .map_err(|_| {
            format!(
                "al-explorer {} did not finish in {REPORT_TIMEOUT:?}",
                args.join(" ")
            )
        })?
        .map_err(|error| format!("cannot run {}: {error}", explorer.display()))?;
    let mut text = format!("al-explorer {}\n\n", args.join(" "));
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    let dir = al_types::cache_root()
        .unwrap_or_else(|| std::env::temp_dir().join("al-lsp"))
        .join("reports");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let path = dir.join(report_file_name(report, object));
    std::fs::write(&path, text)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    let report_uri =
        Url::from_file_path(&path).map_err(|()| format!("cannot address {}", path.display()))?;
    show_location(
        server,
        &Location {
            uri: report_uri,
            range: Range::default(),
        },
    )
    .await;
    Ok(serde_json::json!({ "path": path }))
}

/// Most entries a choice offers. A longer list does not fit the prompt.
const MAX_CHOICES: usize = 12;

/// Take the editor to `locations`: straight there for one, a list to choose
/// from for several, and a short message for none. A command's own result is
/// not shown by Zed, so this is how a lens click becomes visible.
async fn go_to(server: &AlServer, noun: &str, locations: &[Location]) {
    match locations {
        [] => {
            server
                .client
                .show_message(MessageType::INFO, format!("No {noun}s found."))
                .await;
        }
        [only] => show_location(server, only).await,
        many => {
            let labels: Vec<String> = many
                .iter()
                .take(MAX_CHOICES)
                .enumerate()
                .map(|(index, location)| location_label(index, location))
                .collect();
            let more = many.len().saturating_sub(MAX_CHOICES);
            let mut message = format!("{} {noun}s. Choose one to open.", many.len());
            if more > 0 {
                message.push_str(&format!(
                    " The first {MAX_CHOICES} are listed. Find All References lists every one."
                ));
            }
            let actions = labels
                .iter()
                .map(|title| tower_lsp::lsp_types::MessageActionItem {
                    title: title.clone(),
                    properties: Default::default(),
                })
                .collect();
            let choice = server
                .client
                .show_message_request(MessageType::INFO, message, Some(actions))
                .await;
            if let Ok(Some(choice)) = choice {
                if let Some(index) = labels.iter().position(|label| *label == choice.title) {
                    show_location(server, &many[index]).await;
                }
            }
        }
    }
}

/// "3. PalletManagement.Codeunit.al:42": numbered, so two entries in the same
/// file never share a label.
fn location_label(index: usize, location: &Location) -> String {
    let file = location
        .uri
        .to_file_path()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| location.uri.to_string());
    format!("{}. {file}:{}", index + 1, location.range.start.line + 1)
}

async fn show_location(server: &AlServer, location: &Location) {
    let shown = server
        .client
        .show_document(tower_lsp::lsp_types::ShowDocumentParams {
            uri: location.uri.clone(),
            external: Some(false),
            take_focus: Some(true),
            selection: Some(location.range),
        })
        .await;
    if let Err(error) = shown {
        tracing::warn!(%error, uri = %location.uri, "the editor did not open the location");
    }
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

/// `al.runTest`: run one `[Test]` method from its CodeLens.
///
/// The run goes through the daemon's `tests.run_batch` path, the one
/// `tests.run_auto` uses: a test the router classifies as local runs in the
/// interpreter, and a test that needs live Business Central is reported as
/// skipped with [`al_test::test_runner::LIVE_BC_SKIP_MESSAGE`]. That path
/// stores the results, so the lens shows the outcome on its next refresh. No
/// launch configuration is read, because no test runs against a server yet.
///
/// Returns `{ "status": "completed" | "failed" | "invalidArgs", "target"?, "totals"? }`.
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

    let params = serde_json::json!({
        "codeunitIds": [target.codeunit_id],
        "methodNames": [target.method_name],
    });
    let response =
        crate::server::daemon::dispatch_tests_run_batch(&server.workspace, 0, &params).await;
    if let Some(error) = response.error {
        server
            .client
            .show_message(
                MessageType::ERROR,
                format!(
                    "Test '{}' failed to run: {}",
                    target.method_name, error.message
                ),
            )
            .await;
        return serde_json::json!({
            "status": "failed",
            "target": target,
            "error": error.message,
        });
    }
    let result = response.result.unwrap_or_default();
    let totals = result.get("totals").cloned().unwrap_or_default();
    let (level, message) = run_test_message(&target.method_name, &totals);
    if !server.session.is_cancelled() {
        server.client.show_message(level, message).await;
    }
    serde_json::json!({ "status": "completed", "target": target, "totals": totals })
}

/// The message for one lens run, from the `totals` of `tests.run_batch`.
fn run_test_message(method: &str, totals: &serde_json::Value) -> (MessageType, String) {
    let count = |key: &str| {
        totals
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let (passed, failed, skipped) = (count("passed"), count("failed"), count("skipped"));
    if failed > 0 {
        (MessageType::ERROR, format!("Test '{method}' failed"))
    } else if passed > 0 {
        (MessageType::INFO, format!("Test '{method}' passed"))
    } else if skipped > 0 {
        (
            MessageType::WARNING,
            format!(
                "Test '{method}': {}",
                al_test::test_runner::LIVE_BC_SKIP_MESSAGE
            ),
        )
    } else {
        (
            MessageType::WARNING,
            format!("Test '{method}' was not found"),
        )
    }
}

#[cfg(test)]
#[path = "commands_review_tests.rs"]
mod commands_review_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Each object action runs the command its editor task ran.
    #[test]
    fn a_report_runs_the_command_its_task_ran() {
        let file = std::path::Path::new("/p/Pallet.Codeunit.al");
        assert_eq!(
            report_arguments("composed", "Item", file).unwrap(),
            vec!["composed", "Item"]
        );
        assert_eq!(
            report_arguments("suggest-event", "Item", file).unwrap(),
            vec!["suggest-event", "--object", "Item"]
        );
        assert_eq!(
            report_arguments("metrics", "Item", file).unwrap(),
            vec!["metrics", "/p/Pallet.Codeunit.al"]
        );
        assert!(report_arguments("rm -rf", "Item", file).is_none());
        assert_eq!(
            report_file_name("impact", "AUK Pallet/Management"),
            "impact-AUK_Pallet_Management.txt"
        );
    }
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
