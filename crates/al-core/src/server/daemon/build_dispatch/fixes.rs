//! Analysis/fix dispatchers — lint, format, fix, rules, parse, arch-lint,
//! and field-annotation fixers.

use super::super::{
    ensure_document, file_not_found, file_uri_from_params, invalid_params, lint_diag_to_json,
    require_document_text, require_project_root, rpc_error,
};
use super::build::write_al_file_and_refresh;
use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Response};

// ---------------------------------------------------------------------------
// Analysis dispatchers (lint, format, fix, rules, parse, source)
// ---------------------------------------------------------------------------

pub(in crate::server::daemon) fn dispatch_lint(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);

    if all {
        // Lint all workspace files using cached parse trees. Clean files are
        // included with an empty diagnostics array — previously only dirty
        // files were returned, so a fully-clean 62-file workspace reported
        // "0 diagnostics across 0 files", indistinguishable from "scanned
        // nothing" (audit 2026-06-12).
        let mut results: Vec<serde_json::Value> = Vec::new();
        for entry in workspace.file_index.files.iter() {
            let path = entry.key();
            let Some((content, tree)) = workspace.file_index.get_cached_parse(path) else {
                continue;
            };
            let diagnostics = crate::syntax::lint(&tree, &content);
            let diags: Vec<serde_json::Value> = diagnostics.iter().map(lint_diag_to_json).collect();
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
    let text = match require_document_text(workspace, &uri, id) {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    let result = crate::syntax::AlParser::parse_quick(&text);
    let mut diagnostics = crate::syntax::lint(&result.tree, &text);

    for err in &result.errors {
        diagnostics.push(crate::syntax::LintDiagnostic {
            code: "parse-error".to_string(),
            message: err.message.clone(),
            range: err.range,
            severity: crate::syntax::LintSeverity::Error,
        });
    }

    let diags: Vec<serde_json::Value> = diagnostics.iter().map(lint_diag_to_json).collect();
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

    // Accept direct content or file path
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

    // Load per-workspace formatting options from .alformat.json (falls back to defaults).
    let options = workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .map(|root| crate::queries::format::AlFormatConfig::load_options(&root))
        .unwrap_or_default();
    let formatted = crate::syntax::format_al(&content, &options);
    let changed = formatted != content;

    if check {
        Response {
            id,
            result: Some(serde_json::json!({ "changed": changed })),
            error: None,
            ..Default::default()
        }
    } else {
        // If a file was specified, write back. F-011: routed through
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

    let result = crate::syntax::AlParser::parse_quick(&text);
    let diagnostics = crate::syntax::lint(&result.tree, &text);
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

    // No custom lint rules are registered, so no fixes to generate.
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
    let rules = crate::syntax::lint_rules();
    let value: Vec<serde_json::Value> = rules
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
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
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
    let result = crate::syntax::AlParser::parse_quick(&text);
    let elapsed = start.elapsed();

    let node_count = crate::parsing::count_nodes(&result.tree);

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
// ---------------------------------------------------------------------------
// Bulk fix dispatchers (T1603-T1605)
// ---------------------------------------------------------------------------

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

    match crate::queries::bulk_fix::add_application_area(&project_root, value, dry_run) {
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

    // Build tooltip map: field_name -> tooltip from symbol data
    let table_name = params
        .get("fromTable")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tooltips: Vec<(String, String)> = if !table_name.is_empty() {
        workspace
            .symbols
            .get_by_name(table_name)
            .into_iter()
            .filter(|e| e.kind == crate::symbols::ObjectKind::Table)
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

    match crate::queries::bulk_fix::add_tooltips(&project_root, &tooltips, dry_run) {
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

    match crate::queries::bulk_fix::add_data_classification(&project_root, value, dry_run) {
        Ok(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, error_codes::INTERNAL_ERROR, &e),
    }
}
pub(in crate::server::daemon) fn dispatch_arch_lint(workspace: &Workspace, id: u64) -> Response {
    // Load .alarch.json from project root if present; fall back to defaults.
    // The synchronous fs::read_to_string is wrapped in block_in_place so the
    // async runtime hosting this dispatch can re-schedule the parked thread
    // for other work while the read is in flight (matches dispatch_format).
    let config = workspace
        .project
        .try_read()
        .ok()
        .and_then(|p| p.as_ref().map(|p| p.root.join(".alarch.json")))
        .and_then(|path| tokio::task::block_in_place(|| std::fs::read_to_string(path).ok()))
        .and_then(|json| crate::queries::arch_lint::ArchConfig::from_json(&json).ok())
        .unwrap_or_default();
    let violations = crate::queries::arch_lint::arch_lint(workspace, &config);
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
    use crate::workspace::Workspace;
    use al_protocol::jsonrpc::error_codes;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    // -----------------------------------------------------------------------
    // Analysis dispatchers: param-validation & happy-path branches.
    // These exercise the synchronous, in-process error/edge paths that need
    // neither a live BC server nor a spawned binary.
    // -----------------------------------------------------------------------

    /// Open a real `.al` file on disk and return its `file://` URI string,
    /// suitable for the `{ "file": ... }` param shape the dispatchers accept.
    /// The file must exist because `file_uri_from_params` canonicalises it.
    fn write_al(tmp: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = tmp.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.canonicalize().unwrap().to_string_lossy().to_string()
    }

    // --- dispatch_lint -------------------------------------------------------

    #[test]
    fn lint_missing_file_param_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_lint(&ws, 1, &serde_json::json!({}));
        let err = resp.error.expect("missing file/uri must be invalid params");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn lint_single_file_reports_parse_errors() {
        // Positive + behavior: a file with a syntax error must surface at
        // least one diagnostic (the parse-error branch appends them).
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // Deliberately malformed AL — unterminated object.
        let file = write_al(&tmp, "Bad.al", "codeunit 50100 \"Bad\" { procedure X( ");
        let resp = dispatch_lint(&ws, 2, &serde_json::json!({ "file": file }));
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
                .any(|d| d.get("code") == Some(&serde_json::json!("parse-error"))),
            "malformed AL must produce a parse-error diagnostic; got {diags:?}"
        );
    }

    #[test]
    fn lint_all_mode_returns_array_for_empty_workspace() {
        // `all` mode iterates the file index; an empty workspace yields [].
        let ws = empty_ws();
        let resp = dispatch_lint(&ws, 3, &serde_json::json!({ "all": true }));
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .and_then(|v| v.as_array().cloned())
            .expect("array");
        assert!(arr.is_empty(), "empty workspace → no per-file lint entries");
    }

    // --- dispatch_format -----------------------------------------------------

    #[test]
    fn format_content_returns_formatted_text() {
        // Positive: passing raw `content` avoids any file I/O and returns the
        // formatted source plus a `changed` flag.
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
        // In check mode the response carries ONLY `changed`, never `formatted`.
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

    // --- dispatch_fix --------------------------------------------------------

    #[test]
    fn fix_missing_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_fix(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn fix_reports_zero_fixes_for_clean_file() {
        // No custom lint rules are registered, so `fixes` is always 0 and the
        // dryRun flag round-trips. Exercises the happy path + filter branch.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(&tmp, "Ok.al", "codeunit 50100 \"Ok\"\n{\n}\n");
        let resp = dispatch_fix(&ws, 2, &serde_json::json!({ "file": file, "dryRun": true }));
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["fixes"], serde_json::json!(0));
        assert_eq!(r["dryRun"], serde_json::json!(true));
    }

    // --- dispatch_rules ------------------------------------------------------

    #[test]
    fn rules_returns_json_array_mirroring_registry() {
        // Native lint rules are intentionally empty (all diagnostics come from
        // the .NET bridge), so the dispatcher must return a JSON array whose
        // length matches the registry exactly — proving it maps the registry
        // rather than fabricating entries.
        let resp = dispatch_rules(1);
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .and_then(|v| v.as_array().cloned())
            .expect("array");
        assert_eq!(
            arr.len(),
            crate::syntax::lint_rules().len(),
            "dispatch_rules length must mirror the lint-rule registry"
        );
    }

    // --- dispatch_parse ------------------------------------------------------

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
