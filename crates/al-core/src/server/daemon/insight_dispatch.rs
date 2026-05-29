//! Insight engine dispatchers — trace, entrypoints, graph export, dead code, impact, suggest_event.

use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Response, RpcError};

use super::invalid_params;

/// Upper bound on `node_count + edge_count` for full graph export.
/// A 100k-symbol workspace can yield 200 MB+ of DOT/JSON; we refuse
/// to materialise that into a single JSON-RPC response. Clients that
/// hit the cap should narrow the query (impact / trace) or paginate.
const MAX_GRAPH_EXPORT_NODES_AND_EDGES: usize = 50_000;

pub(super) fn dispatch_trace(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(event_name) = params.get("event").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let max_depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let max_depth = max_depth.min(50);

    let graph = workspace.get_or_build_insight_graph();
    let steps = crate::insight::search::trace_event(&graph, event_name, max_depth);
    // SILENT: serialization of valid Vec<TraceStep> should not fail
    let value = serde_json::to_value(&steps).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_entrypoints(workspace: &Workspace, id: u64) -> Response {
    let graph = workspace.get_or_build_insight_graph();
    let entry_points = crate::insight::search::find_entry_points(&graph);
    // SILENT: serialization of valid Vec<&InsightNode> should not fail
    let value = serde_json::to_value(&entry_points).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_graph_export(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");
    let graph = workspace.get_or_build_insight_graph();

    // Refuse to materialise an unbounded graph into one JSON-RPC response.
    // The whole exported document lives in memory twice (the String/Value
    // *and* the framed JSON-RPC body), so even a "moderately large"
    // workspace can OOM the daemon's tokio worker thread.
    let size = graph.node_count() + graph.edge_count();
    if size > MAX_GRAPH_EXPORT_NODES_AND_EDGES {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!(
                    "Graph too large to export in one response: {size} nodes+edges \
                     exceeds cap of {MAX_GRAPH_EXPORT_NODES_AND_EDGES}. \
                     Use the trace or impact endpoints to narrow the query."
                ),
            }),
            ..Default::default()
        };
    }

    match format {
        "dot" => {
            let dot = crate::insight::search::export_dot(&graph);
            Response {
                id,
                result: Some(serde_json::json!({ "format": "dot", "content": dot })),
                error: None,
                ..Default::default()
            }
        }
        _ => {
            let json = crate::insight::search::export_json(&graph);
            // SILENT: serialization of valid GraphJson should not fail
            let value = serde_json::to_value(&json).unwrap_or(serde_json::Value::Null);
            Response {
                id,
                result: Some(value),
                error: None,
                ..Default::default()
            }
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
        ..Default::default()
    }
}

pub(super) fn dispatch_dead_code(workspace: &Workspace, id: u64) -> Response {
    let unused = crate::queries::dead_code::dead_code(workspace);
    // SILENT: serialization of valid Vec<DeadCodeEntry> should not fail
    let value = serde_json::to_value(&unused).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_impact(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
    if symbol.is_empty() {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "Missing 'symbol' parameter".to_string(),
            }),
            ..Default::default()
        };
    }
    let entries = crate::queries::impact::impact(workspace, symbol);
    Response {
        id,
        result: Some(serde_json::json!({ "symbol": symbol, "impacted": entries })),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_suggest_event(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    // Accept either the new structured `query` field or fall back to the legacy
    // `description` field for backwards compatibility during the transition.
    let query_value = params
        .get("query")
        .cloned()
        .unwrap_or_else(|| params.clone());

    let query: crate::queries::suggest_event::EventQuery = match serde_json::from_value(query_value)
    {
        Ok(q) => q,
        Err(e) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: format!("Invalid suggestEvent query: {e}"),
                }),
                ..Default::default()
            };
        }
    };

    let result = crate::queries::suggest_event::suggest_event(workspace, &query);
    // SILENT: serialization of valid SuggestEventResult should not fail
    let value = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
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

    fn assert_invalid_params(resp: &Response) {
        assert!(
            resp.result.is_none(),
            "error response must not carry a result"
        );
        let err = resp.error.as_ref().expect("expected an RpcError");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn dispatch_trace_missing_event_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_trace(&ws, 1, &serde_json::json!({}));
        assert_invalid_params(&resp);
        assert_eq!(resp.id, 1);
    }

    #[test]
    fn dispatch_impact_missing_symbol_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_impact(&ws, 2, &serde_json::json!({}));
        assert_invalid_params(&resp);
        assert_eq!(resp.id, 2);
    }

    #[test]
    fn dispatch_impact_empty_symbol_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_impact(&ws, 3, &serde_json::json!({ "symbol": "" }));
        assert_invalid_params(&resp);
    }

    #[test]
    fn dispatch_suggest_event_malformed_query_is_invalid_params() {
        let ws = Workspace::new();
        // `query` is present but the wrong shape (a string, not an object),
        // so deserialization into EventQuery must fail and surface as an error.
        let resp = dispatch_suggest_event(&ws, 4, &serde_json::json!({ "query": "not-an-object" }));
        assert_invalid_params(&resp);
    }

    #[test]
    fn dispatch_trace_valid_event_returns_result() {
        let ws = Workspace::new();
        // Empty workspace: the trace is empty, but the call must still succeed
        // (a well-formed result, not an error).
        let resp = dispatch_trace(&ws, 5, &serde_json::json!({ "event": "OnAfterPost" }));
        assert!(resp.error.is_none(), "valid event must not error");
        assert!(resp.result.is_some());
    }
}
