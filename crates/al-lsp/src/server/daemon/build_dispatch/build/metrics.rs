// Metrics: cyclomatic/cognitive complexity per procedure

use al_protocol::jsonrpc::Response;
use al_workspace::Workspace;

use crate::server::daemon::{
    ensure_document, file_not_found, file_uri_from_params, invalid_params,
};

pub(in crate::server::daemon) fn dispatch_metrics(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let threshold_cyclomatic = params
        .get("thresholdCyclomatic")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(10);
    let threshold_cognitive = params
        .get("thresholdCognitive")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(15);

    if all {
        let mut all_results: Vec<serde_json::Value> = Vec::new();

        for entry in workspace.file_index.files.iter() {
            let path = entry.key().to_string_lossy().to_string();
            let text = entry.value();
            let parsed = al_syntax::AlParser::parse_quick(text);
            let metrics = al_syntax::complexity::compute_complexity(&parsed.tree, text);
            if !metrics.is_empty() {
                let hotspots: Vec<serde_json::Value> = metrics
                    .iter()
                    .filter(|m| {
                        m.cyclomatic >= threshold_cyclomatic || m.cognitive >= threshold_cognitive
                    })
                    .map(procedure_complexity_to_json)
                    .collect();
                all_results.push(serde_json::json!({
                    "file": path,
                    "procedures": metrics.iter().map(procedure_complexity_to_json).collect::<Vec<_>>(),
                    "hotspots": hotspots,
                }));
            }
        }

        return Response {
            id,
            result: Some(serde_json::json!(all_results)),
            error: None,
            ..Default::default()
        };
    }

    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return file_not_found(id);
    };

    let parsed = al_syntax::AlParser::parse_quick(&text);
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
        "cyclomatic": m.cyclomatic,
        "cognitive": m.cognitive,
    })
}

pub(in crate::server::daemon) fn dispatch_profiler_hints(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let hotspots = params
        .get("hotspots")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let hints = al_analysis::queries::profiler_hints::profiler_hints(workspace, &hotspots);
    let value = serde_json::to_value(&hints).unwrap_or(serde_json::Value::Null);
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
    fn metrics_out_of_range_threshold_falls_back_to_default() {
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
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(
            r["thresholdCyclomatic"],
            serde_json::json!(10),
            "an out-of-range threshold must not wrap into a tiny value via `as u32`"
        );
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
