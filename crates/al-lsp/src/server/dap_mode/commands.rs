//! `workspace/executeCommand` implementations — one function per `al.*`
//! command (F-OPEN-264).
//!
//! `AlServer::execute_command` in `lsp.rs` is a thin dispatch table over
//! these. Each command is independently readable and testable instead of
//! living in one ~350-line match expression.

use std::sync::Arc;

use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticSeverity, FormattingOptions, MessageType, NumberOrString, Position,
    Range, Url, WorkspaceEdit,
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
    // SILENT: .ok() on from_value — invalid argument from client is not user-affecting
    let Some(uri) = arguments
        .first()
        .and_then(|v| serde_json::from_value::<Url>(v.clone()).ok())
    else {
        return;
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
    // SILENT: .ok() on from_value — invalid argument from client is not user-affecting
    if let Some(uri) = arguments
        .first()
        .and_then(|v| serde_json::from_value::<Url>(v.clone()).ok())
    {
        if let Some(text) = server.workspace.documents.get_text(&uri) {
            diagnostics::publish_diagnostics(server, &uri, &text).await;
        }
    }
}

/// `al.getStatus` — health/inventory snapshot for the status command.
pub(super) async fn get_status(server: &AlServer) -> serde_json::Value {
    let has_bridge = server.workspace.semantic.read().await.is_some();
    let has_toolchain = server.workspace.toolchain.read().await.is_some();
    let indexed_symbols = server.workspace.symbols.len();
    let workspace_files = server.workspace.file_index.len();
    let workspace_objects = server.workspace.file_index.object_count();
    let builtins = server
        .workspace
        .builtins
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .len(); // SILENT: recover from RwLock poison

    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "semanticBridge": has_bridge,
        "toolchain": has_toolchain,
        "indexedSymbols": indexed_symbols,
        "workspaceFiles": workspace_files,
        "workspaceObjects": workspace_objects,
        "builtinTypes": builtins,
    })
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
        // Reindex path: pass throwaway ready/notify pair so the
        // call satisfies the signature; reindex doesn't need to
        // gate request handlers since the workspace is already
        // serving traffic.
        let throwaway_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let throwaway_notify = Arc::new(tokio::sync::Notify::new());
        let handle = tokio::spawn(async move {
            workspace::initialize_workspace(
                ws,
                client.clone(),
                Some(uri_cloned),
                throwaway_flag,
                throwaway_notify,
            )
            .await;
            client
                .show_message(MessageType::INFO, "Workspace reindex complete")
                .await;
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
    let toolchain_guard = server.workspace.toolchain.read().await;
    let project_guard = server.workspace.project.read().await;
    let config_guard = server.workspace.config.read().await;
    let toolchain = toolchain_guard.clone();
    let project_root = project_guard.as_ref().map(|p| p.root.clone());
    let use_official_compiler = config_guard.use_official_compiler;
    drop(toolchain_guard);
    drop(project_guard);
    drop(config_guard);

    let Some(root) = project_root else {
        server
            .client
            .show_message(
                MessageType::WARNING,
                "No AL project loaded — open an AL workspace first",
            )
            .await;
        return;
    };

    if use_official_compiler {
        let Some(tc) = toolchain else {
            server
                .client
                .show_message(
                    MessageType::WARNING,
                    "No AL toolchain configured — run 'al setup' first",
                )
                .await;
            return;
        };
        match al_compile::compile_project(&tc, &root, None).await {
            Ok(result) => publish_compile_result(server, &root, &result).await,
            Err(e) => {
                server
                    .client
                    .show_message(MessageType::ERROR, format!("Compilation error: {e}"))
                    .await;
            }
        }
        return;
    }

    let result = al_compile::native_compile(&root);
    publish_compile_result(server, &root, &result).await;
}

/// Convert compiler diagnostics into per-file LSP diagnostics, resolving
/// relative paths against `root`. Pure (no server / I/O) so the severity
/// mapping, the 1-based→0-based position conversion, the end-of-range (start of
/// next line per LSP, not the old u32::MAX sentinel — T067), and the relative→
/// absolute path resolution (F-019) are all unit-testable.
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
                // Compiler diagnostics only carry a start position today; the
                // LSP-spec way to express "to end of line" is the start of the
                // next line. The previous u32::MAX sentinel was tolerated by
                // Zed/VS Code but is undefined by the LSP spec and breaks
                // stricter clients (T067).
                end: Position {
                    line: start_line.saturating_add(1),
                    character: 0,
                },
            },
            severity: Some(severity),
            code: Some(NumberOrString::String(d.code.clone())),
            source: Some("al-compiler".to_string()),
            message: d.message.clone(),
            ..Default::default()
        };
        // F-019: compilers may emit relative paths (`src/Foo.al`) when run from
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
/// that were affected last compile but are clean now (F-008).
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

    // F-008: clear compiler diagnostics for files that were
    // affected last compile but are clean now. We re-publish
    // syntax/lint diagnostics if the file is open (so
    // existing squiggles stay), or an empty list otherwise.
    let last = server.workspace.last_compile_affected.lock().await;
    let stale: Vec<String> = last.difference(&current_affected).cloned().collect();
    drop(last);
    for file in &stale {
        if let Ok(uri) = Url::from_file_path(file) {
            if let Some(text) = server.workspace.documents.get_text(&uri) {
                diagnostics::publish_diagnostics(server, &uri, &text).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use al_compile::{CompileDiagnostic, DiagnosticSeverity as S};

    fn diag(file: &str, line: u32, column: u32, sev: S, code: &str) -> CompileDiagnostic {
        CompileDiagnostic {
            file: file.to_string(),
            line,
            column,
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
        // 1-based (5,3) → 0-based (4,2); end is the start of the next line (T067).
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
        assert!(
            by_file
                .get("/proj/A.al")
                .unwrap()
                .iter()
                .any(|d| d.severity == Some(DiagnosticSeverity::INFORMATION))
        );
    }
}
