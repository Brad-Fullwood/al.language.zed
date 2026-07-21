//! Analysis/fix dispatchers — lint, format, fix, rules, parse, arch-lint,
//! and field-annotation fixers.

use super::super::{
    ensure_document, file_not_found, file_uri_from_params, invalid_params, require_document_text,
    require_project_root, rpc_error,
};
use super::build::write_al_file_and_refresh;
use al_protocol::jsonrpc::{error_codes, Response};
use al_workspace::Workspace;

pub(in crate::server::daemon) async fn dispatch_lint(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);

    if all {
        // Run the same syntax + file-local + symbol-aware workspace rules used
        // by editor diagnostics. Clean files remain present so "scanned all,
        // clean" is distinguishable from "scanned nothing".
        let mut results: Vec<serde_json::Value> = Vec::new();
        let config = workspace.config.read().await.clone();
        let project_root = workspace
            .project
            .read()
            .await
            .as_ref()
            .map(|project| project.root.clone());
        for (path, diagnostics) in
            al_analysis::queries::diagnostics::workspace_syntax_diagnostics_at_root(
                workspace,
                &config,
                project_root.as_deref(),
            )
        {
            let diags: Vec<serde_json::Value> =
                diagnostics.iter().map(syntax_diag_to_json).collect();
            results.push(serde_json::json!({
                "file": path.display().to_string(),
                "diagnostics": diags,
            }));
        }
        return Response {
            id,
            result: Some(serde_json::json!(results)),
            error: None,
            ..Default::default()
        };
    }

    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    let _text = match require_document_text(workspace, &uri, id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    let config = workspace.config.read().await.clone();
    let project_root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let diagnostics = al_analysis::queries::diagnostics::syntax_diagnostics_at_root(
        workspace,
        &uri,
        &config,
        project_root.as_deref(),
    );
    let diags: Vec<serde_json::Value> = diagnostics.iter().map(syntax_diag_to_json).collect();
    Response {
        id,
        result: Some(serde_json::json!(diags)),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_format(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let check = params
        .get("check")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let content = if let Some(text) = params.get("content").and_then(|v| v.as_str()) {
        text.to_string()
    } else if let Some(uri) = file_uri_from_params(params) {
        ensure_document(workspace, &uri);
        match workspace.documents.get_text(&uri) {
            Some(t) => t,
            None => {
                return file_not_found(id);
            }
        }
    } else {
        return invalid_params(id);
    };

    let options = workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .map(|root| al_analysis::queries::format::AlFormatConfig::load_options(&root))
        .unwrap_or_default();
    let formatted = al_syntax::format_al(&content, &options);
    let changed = formatted != content;

    if check {
        Response {
            id,
            result: Some(serde_json::json!({ "changed": changed })),
            error: None,
            ..Default::default()
        }
    } else {
        // If a file was specified, write it through
        // write_al_file_and_refresh so the document store, file index,
        // and insight graph all see the update.
        if let Some(uri) = file_uri_from_params(params) {
            if let Ok(path) = uri.to_file_path() {
                if changed {
                    if let Err(e) = tokio::task::block_in_place(|| {
                        write_al_file_and_refresh(workspace, &path, formatted.clone())
                    }) {
                        return rpc_error(
                            id,
                            -32000,
                            &format!("Failed to write formatted file: {e}"),
                        );
                    }
                }
            }
        }
        Response {
            id,
            result: Some(serde_json::json!({
                "formatted": formatted,
                "changed": changed,
            })),
            error: None,
            ..Default::default()
        }
    }
}
pub(in crate::server::daemon) fn dispatch_fix(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let rule_filter = params.get("rule").and_then(|v| v.as_str());

    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return file_not_found(id);
    };

    let result = al_syntax::AlParser::parse_quick(&text);
    let diagnostics = al_syntax::lint(&result.tree, &text);
    let filtered: Vec<_> = diagnostics
        .iter()
        .filter(|d| {
            if let Some(rule) = rule_filter {
                d.code.eq_ignore_ascii_case(rule)
            } else {
                true
            }
        })
        .collect();

    // Transaction/semantic findings intentionally have no automatic edits:
    // choosing the correct transaction boundary is a design decision.
    let edits: Vec<serde_json::Value> = Vec::new();

    let _ = dry_run;
    let _ = &text;

    Response {
        id,
        result: Some(serde_json::json!({
            "diagnostics": filtered.len(),
            "fixes": edits.len(),
            "dryRun": dry_run,
            "edits": edits,
        })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_rules(id: u64) -> Response {
    let mut value: Vec<serde_json::Value> = al_syntax::lint_rules()
        .iter()
        .map(|r| {
            serde_json::json!({
                "code": r.code,
                "name": r.name,
                "severity": r.severity.to_string(),
                "description": r.description,
            })
        })
        .collect();
    value.extend(
        al_analysis::queries::transaction_lint::transaction_lint_rules()
            .iter()
            .map(|rule| {
                serde_json::json!({
                    "code": rule.code,
                    "name": rule.name,
                    "severity": workspace_severity_label(rule.severity),
                    "description": rule.description,
                })
            }),
    );
    value.extend(
        al_analysis::queries::native_check::native_check_rules()
            .iter()
            .map(|rule| {
                serde_json::json!({
                    "code": rule.code,
                    "name": rule.name,
                    "severity": match rule.severity {
                        al_analysis::queries::native_check::NativeSeverity::Error => "error",
                        al_analysis::queries::native_check::NativeSeverity::Warning => "warning",
                    },
                    "description": rule.description,
                })
            }),
    );
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}

fn syntax_diag_to_json(
    diagnostic: &al_analysis::queries::diagnostics::SyntaxDiagnostic,
) -> serde_json::Value {
    let severity = match diagnostic.severity {
        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Error => "error",
        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Warning => "warning",
        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Info => "info",
        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Hint => "hint",
    };
    serde_json::json!({
        "code": diagnostic.code,
        "message": diagnostic.message,
        "severity": severity,
        "line": diagnostic.range.start.line + 1,
        "column": diagnostic.range.start.character + 1,
        "endLine": diagnostic.range.end.line + 1,
        "endColumn": diagnostic.range.end.character + 1,
    })
}

fn workspace_severity_label(
    severity: al_analysis::queries::transaction_lint::WorkspaceLintSeverity,
) -> &'static str {
    use al_analysis::queries::transaction_lint::WorkspaceLintSeverity;
    match severity {
        WorkspaceLintSeverity::Error => "error",
        WorkspaceLintSeverity::Warning => "warning",
        WorkspaceLintSeverity::Info => "info",
        WorkspaceLintSeverity::Hint => "hint",
    }
}
pub(in crate::server::daemon) fn dispatch_parse(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return file_not_found(id);
    };

    let start = std::time::Instant::now();
    let result = al_syntax::AlParser::parse_quick(&text);
    let elapsed = start.elapsed();

    let node_count = al_source::parsing::count_nodes(&result.tree);

    Response {
        id,
        result: Some(serde_json::json!({
            "errors": result.errors.len(),
            "nodeCount": node_count,
            "parseTimeMs": elapsed.as_secs_f64() * 1000.0,
            "parseErrors": result.errors.iter().map(|e| serde_json::json!({
                "message": e.message,
                "line": e.range.start_point.row + 1,
                "column": e.range.start_point.column + 1,
            })).collect::<Vec<_>>(),
        })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_fix_application_area(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project_root = match require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let value = params
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("All");
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match al_analysis::queries::bulk_fix::add_application_area(&project_root, value, dry_run) {
        Ok(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, error_codes::INTERNAL_ERROR, &e),
    }
}
pub(in crate::server::daemon) fn dispatch_fix_tooltips(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project_root = match require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let table_name = params
        .get("fromTable")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tooltips: Vec<(String, String)> = if !table_name.is_empty() {
        workspace
            .symbols
            .get_by_name(table_name)
            .into_iter()
            .filter(|e| e.kind == al_symbols::ObjectKind::Table)
            .flat_map(|e| {
                e.fields
                    .iter()
                    .filter_map(|f| {
                        let tooltip = f
                            .properties
                            .iter()
                            .find(|p| p.name.eq_ignore_ascii_case("ToolTip"))
                            .map(|p| p.value.clone())?;
                        Some((f.name.clone(), tooltip))
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    } else {
        Vec::new()
    };

    match al_analysis::queries::bulk_fix::add_tooltips(&project_root, &tooltips, dry_run) {
        Ok(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, error_codes::INTERNAL_ERROR, &e),
    }
}
pub(in crate::server::daemon) fn dispatch_fix_data_classification(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project_root = match require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let value = params
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("CustomerContent");
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match al_analysis::queries::bulk_fix::add_data_classification(&project_root, value, dry_run) {
        Ok(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, error_codes::INTERNAL_ERROR, &e),
    }
}
pub(in crate::server::daemon) async fn dispatch_arch_lint(
    workspace: &Workspace,
    id: u64,
) -> Response {
    let root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let config = if let Some(root) = root {
        let path = root.join(".alarch.json");
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => match al_analysis::queries::arch_lint::ArchConfig::from_json(&json) {
                Ok(config) => config,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("Invalid {}: {error}", path.display()),
                    )
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(error) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("Failed to read {}: {error}", path.display()),
                )
            }
        }
    } else {
        Default::default()
    };
    let violations = al_analysis::queries::arch_lint::arch_lint(workspace, &config);
    let value = serde_json::to_value(&violations).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_protocol::jsonrpc::error_codes;
    use al_workspace::Workspace;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    /// Open a real `.al` file on disk and return its `file://` URI string,
    /// suitable for the `{ "file": ... }` param shape the dispatchers accept.
    /// The file must exist because `file_uri_from_params` canonicalises it.
    fn write_al(tmp: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = tmp.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.canonicalize().unwrap().to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn lint_missing_file_param_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_lint(&ws, 1, &serde_json::json!({})).await;
        let err = resp.error.expect("missing file/uri must be invalid params");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn lint_single_file_reports_parse_errors() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // Deliberately malformed AL — unterminated object.
        let file = write_al(&tmp, "Bad.al", "codeunit 50100 \"Bad\" { procedure X( ");
        let resp = dispatch_lint(&ws, 2, &serde_json::json!({ "file": file })).await;
        assert!(
            resp.error.is_none(),
            "lint should succeed: {:?}",
            resp.error
        );
        let diags = resp
            .result
            .as_ref()
            .and_then(|v| v.as_array())
            .expect("diagnostics array");
        assert!(
            diags
                .iter()
                .any(|d| d.get("code") == Some(&serde_json::json!("syntax"))),
            "malformed AL must produce a syntax diagnostic; got {diags:?}"
        );
    }

    #[tokio::test]
    async fn lint_all_mode_returns_array_for_empty_workspace() {
        let ws = empty_ws();
        let resp = dispatch_lint(&ws, 3, &serde_json::json!({ "all": true })).await;
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .and_then(|v| v.as_array().cloned())
            .expect("array");
        assert!(arr.is_empty(), "empty workspace → no per-file lint entries");
    }

    #[tokio::test]
    async fn arch_lint_rejects_malformed_configuration() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".alarch.json"),
            r#"{"rules":[{"id":"CX","description":"complexity","kind":"maxComplexity","values":["not-a-number"]}]}"#,
        )
        .unwrap();
        let workspace = Workspace::new();
        *workspace.project.write().await = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: "test".to_string(),
                name: "Test".to_string(),
                publisher: "Test".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
        });

        let response = dispatch_arch_lint(&workspace, 4).await;
        let error = response
            .error
            .expect("malformed config must be an RPC error");
        assert_eq!(error.code, error_codes::INVALID_PARAMS);
        assert!(error.message.contains(".alarch.json"));
        assert!(error.message.contains("complexity threshold"));
    }

    #[test]
    fn format_content_returns_formatted_text() {
        let ws = empty_ws();
        let resp = dispatch_format(
            &ws,
            1,
            &serde_json::json!({ "content": "codeunit 50100 \"X\"\n{\n}" }),
        );
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        assert!(r.get("formatted").and_then(|v| v.as_str()).is_some());
        assert!(r.get("changed").and_then(|v| v.as_bool()).is_some());
    }

    #[test]
    fn format_check_mode_only_reports_changed_flag() {
        let ws = empty_ws();
        let resp = dispatch_format(
            &ws,
            2,
            &serde_json::json!({ "content": "codeunit 50100 X {}", "check": true }),
        );
        let r = resp.result.expect("result");
        assert!(r.get("changed").is_some(), "check mode must report changed");
        assert!(
            r.get("formatted").is_none(),
            "check mode must NOT include formatted text"
        );
    }

    #[test]
    fn format_missing_content_and_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_format(&ws, 3, &serde_json::json!({}));
        let err = resp.error.expect("no content and no file → invalid params");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn fix_missing_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_fix(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn fix_reports_zero_fixes_for_clean_file() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(&tmp, "Ok.al", "codeunit 50100 \"Ok\"\n{\n}\n");
        let resp = dispatch_fix(&ws, 2, &serde_json::json!({ "file": file, "dryRun": true }));
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["fixes"], serde_json::json!(0));
        assert_eq!(r["dryRun"], serde_json::json!(true));
    }

    #[test]
    fn rules_returns_json_array_mirroring_registry() {
        let resp = dispatch_rules(1);
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .and_then(|v| v.as_array().cloned())
            .expect("array");
        let expected = al_syntax::lint_rules().len()
            + al_analysis::queries::transaction_lint::transaction_lint_rules().len()
            + al_analysis::queries::native_check::native_check_rules().len();
        assert_eq!(
            arr.len(),
            expected,
            "dispatch_rules must list every registry"
        );
    }

    #[test]
    fn parse_missing_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_parse(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_reports_node_count_and_errors() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(&tmp, "P.al", "codeunit 50100 \"P\"\n{\n}\n");
        let resp = dispatch_parse(&ws, 2, &serde_json::json!({ "file": file }));
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        let nodes = r["nodeCount"].as_u64().expect("nodeCount");
        assert!(nodes > 0, "a non-empty file must parse to >0 nodes");
        assert!(r.get("parseErrors").is_some());
    }
}
