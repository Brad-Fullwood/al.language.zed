//! Insight engine dispatchers — trace, entrypoints, graph export, dead code, impact, suggest_event.

use al_core::workspace::Workspace;
use al_daemon_client::jsonrpc::{error_codes, Response, RpcError};

use super::invalid_params;

pub(super) fn dispatch_trace(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(event_name) = params.get("event").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let max_depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let max_depth = max_depth.min(50);

    let graph = workspace.get_or_build_insight_graph();
    let steps = al_core::insight::search::trace_event(&graph, event_name, max_depth);
    // SILENT: serialization of valid Vec<TraceStep> should not fail
    let value = serde_json::to_value(&steps).unwrap_or(serde_json::Value::Null);
    Response { id, result: Some(value), error: None }
}

pub(super) fn dispatch_entrypoints(workspace: &Workspace, id: u64) -> Response {
    let graph = workspace.get_or_build_insight_graph();
    let entry_points = al_core::insight::search::find_entry_points(&graph);
    // SILENT: serialization of valid Vec<&InsightNode> should not fail
    let value = serde_json::to_value(&entry_points).unwrap_or(serde_json::Value::Null);
    Response { id, result: Some(value), error: None }
}

pub(super) fn dispatch_graph_export(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("json");
    let graph = workspace.get_or_build_insight_graph();

    match format {
        "dot" => {
            let dot = al_core::insight::search::export_dot(&graph);
            Response {
                id,
                result: Some(serde_json::json!({ "format": "dot", "content": dot })),
                error: None,
            }
        }
        _ => {
            let json = al_core::insight::search::export_json(&graph);
            // SILENT: serialization of valid GraphJson should not fail
            let value = serde_json::to_value(&json).unwrap_or(serde_json::Value::Null);
            Response { id, result: Some(value), error: None }
        }
    }
}

pub(super) fn dispatch_insight_stats(workspace: &Workspace, id: u64) -> Response {
    let graph = workspace.get_or_build_insight_graph();
    Response {
        id,
        result: Some(serde_json::json!({
            "nodes": graph.node_count(),
            "edges": graph.edge_count(),
        })),
        error: None,
    }
}

pub(super) fn dispatch_dead_code(workspace: &Workspace, id: u64) -> Response {
    let unused = al_core::queries::dead_code::dead_code(workspace);
    Response {
        id,
        result: Some(serde_json::to_value(&unused).unwrap_or_default()),
        error: None,
    }
}

pub(super) fn dispatch_impact(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
    if symbol.is_empty() {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "Missing 'symbol' parameter".to_string(),
            }),
        };
    }
    let entries = al_core::queries::impact::impact(workspace, symbol);
    Response {
        id,
        result: Some(serde_json::json!({ "symbol": symbol, "impacted": entries })),
        error: None,
    }
}

pub(super) fn dispatch_suggest_event(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    // Accept either the new structured `query` field or fall back to the legacy
    // `description` field for backwards compatibility during the transition.
    let query_value = params.get("query").cloned().unwrap_or_else(|| params.clone());

    let query: al_core::queries::suggest_event::EventQuery = match serde_json::from_value(query_value) {
        Ok(q) => q,
        Err(e) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: format!("Invalid suggestEvent query: {e}"),
                }),
            };
        }
    };

    let result = al_core::queries::suggest_event::suggest_event(workspace, &query);
    Response {
        id,
        result: Some(serde_json::to_value(&result).unwrap_or_default()),
        error: None,
    }
}
