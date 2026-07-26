// Metrics: cyclomatic/cognitive complexity per procedure

use al_protocol::jsonrpc::{error_codes, Response};
use al_workspace::Workspace;

use crate::server::daemon::{
    ensure_document, file_not_found, file_uri_from_params, invalid_params, optional_bool_param,
    optional_bounded_usize_param, rpc_error,
};

pub(in crate::server::daemon) fn dispatch_metrics(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let all = match optional_bool_param(params, "all", false) {
        Ok(all) => all,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let threshold_cyclomatic =
        match optional_bounded_usize_param(params, "thresholdCyclomatic", 10, u32::MAX as usize) {
            Ok(value) => value as u32,
            Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
        };
    let threshold_cognitive =
        match optional_bounded_usize_param(params, "thresholdCognitive", 15, u32::MAX as usize) {
            Ok(value) => value as u32,
            Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
        };

    if all {
        return match al_analysis::queries::complexity::workspace_complexity(
            workspace,
            threshold_cyclomatic,
            threshold_cognitive,
        ) {
            Ok(report) => match serde_json::to_value(&report) {
                Ok(value) => Response {
                    id,
                    result: Some(value),
                    error: None,
                    ..Default::default()
                },
                Err(error) => rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("serialize complexity report failed: {error}"),
                ),
            },
            Err(error) => rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("workspace complexity analysis failed: {error}"),
            ),
        };
    }

    let uri = match file_uri_from_params(params) {
        Ok(Some(uri)) => uri,
        Ok(None) => return invalid_params(id),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if let Err(response) = ensure_document(workspace, &uri, id) {
        return response;
    }

    let Some(text) = workspace.documents.get_text(&uri) else {
        return file_not_found(id);
    };

    let parsed = al_syntax::AlParser::parse_quick(&text);
    if parsed.tree.root_node().has_error() {
        return rpc_error(
            id,
            error_codes::CODE_ANALYSIS_ERROR,
            "complexity analysis requires syntactically valid AL source",
        );
    }
    let metrics = al_syntax::complexity::compute_complexity(&parsed.tree, &text);

    let hotspots: Vec<serde_json::Value> = metrics
        .iter()
        .filter(|m| m.cyclomatic >= threshold_cyclomatic || m.cognitive >= threshold_cognitive)
        .map(procedure_complexity_to_json)
        .collect();

    Response {
        id,
        result: Some(serde_json::json!({
            "procedures": metrics.iter().map(procedure_complexity_to_json).collect::<Vec<_>>(),
            "hotspots": hotspots,
            "thresholdCyclomatic": threshold_cyclomatic,
            "thresholdCognitive": threshold_cognitive,
        })),
        error: None,
        ..Default::default()
    }
}
fn procedure_complexity_to_json(
    m: &al_syntax::complexity::ProcedureComplexity,
) -> serde_json::Value {
    serde_json::json!({
        "name": m.name,
        "line": m.line,
        "nestingDepth": m.nesting_depth,
        "cyclomatic": m.cyclomatic,
        "cognitive": m.cognitive,
    })
}

pub(in crate::server::daemon) fn dispatch_profiler_hints(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let hotspots = match params.get("hotspots") {
        None => Vec::new(),
        Some(value) => match value.as_array() {
            Some(hotspots) => hotspots.clone(),
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'hotspots' must be an array",
                );
            }
        },
    };
    let hints = match al_analysis::queries::profiler_hints::profiler_hints(workspace, &hotspots) {
        Ok(hints) => hints,
        Err(al_analysis::queries::profiler_hints::ProfilerHintError::InvalidHotspot { reason }) => {
            return rpc_error(id, error_codes::INVALID_PARAMS, &reason);
        }
        Err(error) => {
            return rpc_error(id, error_codes::INTERNAL_ERROR, &error.to_string());
        }
    };
    match serde_json::to_value(&hints) {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        },
        Err(error) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("serialize profiler hints failed: {error}"),
        ),
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

    fn write_al(tmp: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = tmp.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.canonicalize().unwrap().to_string_lossy().to_string()
    }

    #[test]
    fn metrics_missing_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_metrics(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn metrics_single_file_returns_thresholds_and_procedures() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(
            &tmp,
            "M.al",
            "codeunit 50100 \"M\"\n{\n  procedure Do()\n  begin\n  end;\n}\n",
        );
        let resp = dispatch_metrics(&ws, 2, &serde_json::json!({ "file": file }));
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["thresholdCyclomatic"], serde_json::json!(10));
        assert_eq!(r["thresholdCognitive"], serde_json::json!(15));
        assert!(r.get("procedures").and_then(|v| v.as_array()).is_some());
    }

    #[test]
    fn metrics_rejects_out_of_range_or_wrong_typed_options() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(
            &tmp,
            "M.al",
            "codeunit 50100 \"M\"\n{\n  procedure Do()\n  begin\n  end;\n}\n",
        );
        let resp = dispatch_metrics(
            &ws,
            2,
            &serde_json::json!({ "file": file, "thresholdCyclomatic": u64::MAX }),
        );
        assert_eq!(
            resp.error.expect("out-of-range threshold must fail").code,
            error_codes::INVALID_PARAMS
        );

        for params in [
            serde_json::json!({ "all": "yes" }),
            serde_json::json!({ "thresholdCyclomatic": "ten" }),
            serde_json::json!({ "thresholdCognitive": -1 }),
        ] {
            assert_eq!(
                dispatch_metrics(&ws, 3, &params)
                    .error
                    .expect("wrong option type must fail")
                    .code,
                error_codes::INVALID_PARAMS
            );
        }
    }

    #[test]
    fn metrics_rejects_malformed_source_in_single_and_workspace_modes() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(
            &tmp,
            "Broken.al",
            "codeunit 50100 Broken { procedure Incomplete(",
        );
        let single = dispatch_metrics(&ws, 4, &serde_json::json!({ "file": file }));
        assert_eq!(
            single.error.expect("single malformed source").code,
            error_codes::CODE_ANALYSIS_ERROR
        );

        let ws = empty_ws();
        ws.file_index.files.insert(
            std::path::PathBuf::from("/project/MissingCache.al"),
            "codeunit 50100 MissingCache { }".to_string(),
        );
        let all = dispatch_metrics(&ws, 5, &serde_json::json!({ "all": true }));
        assert_eq!(
            all.error.expect("incomplete workspace").code,
            error_codes::INTERNAL_ERROR
        );
    }

    #[test]
    fn metrics_output_includes_procedure_nesting_depth() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(
            &tmp,
            "Nested.al",
            "codeunit 50100 M\n{\n  procedure Outer()\n  begin\n  end;\n  procedure Inner()\n  begin\n  end;\n}\n",
        );
        let resp = dispatch_metrics(&ws, 2, &serde_json::json!({ "file": file }));
        let procedures = resp.result.expect("result")["procedures"]
            .as_array()
            .cloned()
            .expect("procedures array");
        assert_eq!(procedures.len(), 2);
        assert_eq!(procedures[0]["nestingDepth"], serde_json::json!(0));
        assert_eq!(procedures[1]["nestingDepth"], serde_json::json!(0));
    }

    #[test]
    fn profiler_hints_absent_hotspots_returns_array() {
        let ws = empty_ws();
        let resp = dispatch_profiler_hints(&ws, 1, &serde_json::json!({}));
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert!(
            r.is_array(),
            "profiler hints must serialize to a JSON array, got {r:?}"
        );
    }
}
