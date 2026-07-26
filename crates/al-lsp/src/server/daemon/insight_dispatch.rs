//! Insight engine dispatchers — trace, entrypoints, graph export, dead code, impact, suggest_event.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;

use super::{invalid_params, optional_bounded_usize_param, rpc_error, serialized_response};

/// Upper bound on `node_count + edge_count` for full graph export.
/// A 100k-symbol workspace can yield 200 MB+ of DOT/JSON; we refuse
/// to materialise that into a single JSON-RPC response. Clients that
/// hit the cap should narrow the query (impact / trace) or paginate.
const MAX_GRAPH_EXPORT_NODES_AND_EDGES: usize = 50_000;

fn graph_build_error(
    id: u64,
    operation: &str,
    error: al_workspace::CallGraphBuildError,
) -> Response {
    Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INTERNAL_ERROR,
            message: format!("{operation} could not build a complete call graph: {error}"),
        }),
        ..Default::default()
    }
}

pub(super) fn dispatch_trace(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(event_name) = params.get("event").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let max_depth = match optional_bounded_usize_param(params, "depth", 10, 50) {
        Ok(depth) => depth,
        Err(error) => return rpc_error(id, error_codes::INVALID_PARAMS, &error),
    };

    // serve the WORKSPACE-ENRICHED graph (packages + workspace
    // objects/procedures/calls), not the package-only one. The enriched build
    // is cached; the returned call-graph read guard is held only while serving.
    let (graph, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "trace", error),
    };
    let steps = al_insight::search::trace_event(&graph, event_name, max_depth);
    serialized_response(id, &steps, "trace")
}

pub(super) fn dispatch_entrypoints(workspace: &Workspace, id: u64) -> Response {
    let (graph, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "entrypoints", error),
    };
    let entry_points = al_insight::search::find_entry_points(&graph);
    serialized_response(id, &entry_points, "entrypoints")
}

pub(super) fn dispatch_graph_export(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let format = match params.get("format") {
        None => "json",
        Some(value) => match value.as_str() {
            Some(format @ ("json" | "dot")) => format,
            _ => {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INVALID_PARAMS,
                        message: "'format' must be 'json' or 'dot'".to_string(),
                    }),
                    ..Default::default()
                };
            }
        },
    };
    let (graph, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "graph export", error),
    };

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
            let dot = al_insight::search::export_dot(&graph);
            Response {
                id,
                result: Some(serde_json::json!({ "format": "dot", "content": dot })),
                error: None,
                ..Default::default()
            }
        }
        "json" => {
            let json = al_insight::search::export_json(&graph);
            serialized_response(id, &json, "graphExport")
        }
        _ => unreachable!("format was validated above"),
    }
}

pub(super) fn dispatch_insight_stats(workspace: &Workspace, id: u64) -> Response {
    let (graph, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "insight stats", error),
    };
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
    match al_analysis::queries::dead_code::dead_code(workspace) {
        Ok(unused) => serialized_response(id, &unused, "deadCode"),
        Err(error) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: error.to_string(),
            }),
            ..Default::default()
        },
    }
}

/// Native semantic workspace checks duplicate object ids, ids outside
/// the declared `app.json` idRanges, and duplicate object names. Additive and
/// entirely separate from the `diagnostics`/`lint` paths — emits `AL-NC*` codes.
pub(super) async fn dispatch_native_check(workspace: &Workspace, id: u64) -> Response {
    let root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let findings =
        al_analysis::queries::native_check::native_semantic_checks(workspace, root.as_deref());
    serialized_response(id, &findings, "nativeCheck")
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
    match al_analysis::queries::impact::impact(workspace, symbol) {
        Ok(entries) => Response {
            id,
            result: Some(serde_json::json!({ "symbol": symbol, "impacted": entries })),
            error: None,
            ..Default::default()
        },
        Err(error) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: error.to_string(),
            }),
            ..Default::default()
        },
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

    let query: al_analysis::queries::suggest_event::EventQuery =
        match serde_json::from_value(query_value) {
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

    match al_analysis::queries::suggest_event::suggest_event(workspace, &query) {
        Ok(result) => serialized_response(id, &result, "suggestEvent"),
        Err(al_analysis::queries::suggest_event::SuggestEventError::Graph(error)) => {
            graph_build_error(id, "suggestEvent", error)
        }
        Err(error) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: error.to_string(),
            }),
            ..Default::default()
        },
    }
}

/// Returns table usage grouped by object and operation.
pub(super) fn dispatch_table_impact(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(table) = params.get("table").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let result = al_insight::analysis::table_impact(&workspace.symbols, table);
    serialized_response(id, &result, "tableImpact")
}

/// Returns the event propagation tree, including cycles.
pub(super) fn dispatch_trace_chain(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(event_name) = params.get("event").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let max_depth = match optional_bounded_usize_param(params, "depth", 10, 50) {
        Ok(depth) => depth,
        Err(error) => return rpc_error(id, error_codes::INVALID_PARAMS, &error),
    };

    let (insight, cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "traceChain", error),
    };
    let Some(call_graph) = cg_guard.as_ref() else {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            "traceChain call-graph cache invariant was broken after a successful graph build",
        );
    };
    let chain = al_insight::search::trace_event_chain(&insight, call_graph, event_name, max_depth);
    serialized_response(id, &chain, "traceChain")
}

/// Returns all published events, subscribers, and orphan subscribers.
pub(super) fn dispatch_event_map(workspace: &Workspace, id: u64) -> Response {
    let (insight, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "eventMap", error),
    };
    let result = al_insight::discovery::discover_events(&insight);
    serialized_response(id, &result, "eventMap")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_with_malformed_dependency_source() -> (Workspace, tempfile::NamedTempFile) {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let mut app = Vec::from(&b"NAVX"[..]);
        app.resize(40, 0);
        let mut archive = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut archive));
            let options = SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(
                br#"<?xml version="1.0"?><Package><App Id="00000000-0000-0000-0000-000000000001" Name="Broken RPC Source" Publisher="Test" Version="1.0.0.0" /></Package>"#,
            )
            .unwrap();
            zip.start_file("SymbolReference.json", options).unwrap();
            zip.write_all(
                br#"{"Codeunits":[{"Id":50190,"Name":"Broken RPC Source","Methods":[]}]}"#,
            )
            .unwrap();
            zip.start_file("src/Cod50190.Broken.al", options).unwrap();
            zip.write_all(b"codeunit 50190 Broken { procedure Incomplete(")
                .unwrap();
            zip.finish().unwrap();
        }
        app.extend_from_slice(&archive);

        let mut file = tempfile::Builder::new().suffix(".app").tempfile().unwrap();
        file.write_all(&app).unwrap();
        file.flush().unwrap();

        let workspace = Workspace::new();
        workspace
            .symbols
            .load_packages(std::slice::from_ref(&file.path().to_path_buf()))
            .unwrap();
        (workspace, file)
    }

    fn assert_invalid_params(resp: &Response) {
        assert!(
            resp.result.is_none(),
            "error response must not carry a result"
        );
        let err = resp.error.as_ref().expect("expected an RpcError");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn malformed_dependency_source_is_an_internal_error_not_an_empty_event_map() {
        let (workspace, _package) = workspace_with_malformed_dependency_source();

        let response = dispatch_event_map(&workspace, 90);
        assert!(
            response.result.is_none(),
            "an incomplete graph must never be serialized as an empty result"
        );
        let error = response.error.expect("graph failure must be explicit");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert!(error
            .message
            .contains("could not build a complete call graph"));
        assert!(error.message.contains("did not parse cleanly"));
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
    fn dispatch_impact_rejects_malformed_workspace_instead_of_returning_partial_results() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Broken.al"),
            "codeunit 50100 Broken { procedure Incomplete(".to_string(),
        );

        let resp = dispatch_impact(&ws, 4, &serde_json::json!({ "symbol": "Customer" }));
        assert!(resp.result.is_none());
        let error = resp.error.expect("incomplete impact input must fail");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert!(error.message.contains("incomplete workspace snapshot"));
    }

    #[test]
    fn dispatch_table_impact_missing_table_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_table_impact(&ws, 40, &serde_json::json!({}));
        assert_invalid_params(&resp);
    }

    #[test]
    fn dispatch_table_impact_returns_table_impact_result_shape() {
        let ws = Workspace::new();
        let resp = dispatch_table_impact(&ws, 41, &serde_json::json!({ "table": "Customer" }));
        let value = resp.result.expect("must carry a result");
        assert_eq!(
            value.get("tableName").and_then(|v| v.as_str()),
            Some("Customer")
        );
        assert!(value.get("objects").and_then(|v| v.as_array()).is_some());
        assert!(value.get("totalImpacts").is_some());
    }

    #[test]
    fn dispatch_trace_chain_missing_event_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_trace_chain(&ws, 42, &serde_json::json!({}));
        assert_invalid_params(&resp);
    }

    #[test]
    fn dispatch_trace_chain_returns_event_chain_shape() {
        let ws = Workspace::new();
        let resp = dispatch_trace_chain(&ws, 43, &serde_json::json!({ "event": "OnAfterPost" }));
        let value = resp.result.expect("must carry a result");
        assert_eq!(
            value.get("eventName").and_then(|v| v.as_str()),
            Some("OnAfterPost")
        );
        assert!(value.get("chains").and_then(|v| v.as_array()).is_some());
        assert!(value.get("nodesVisited").is_some());
    }

    #[test]
    fn dispatch_event_map_returns_discovery_result_shape() {
        let ws = Workspace::new();
        let resp = dispatch_event_map(&ws, 44);
        let value = resp.result.expect("must carry a result");
        assert!(value.get("events").and_then(|v| v.as_array()).is_some());
        assert!(value
            .get("orphanSubscribers")
            .and_then(|v| v.as_array())
            .is_some());
        assert_eq!(value.get("totalEvents").and_then(|v| v.as_u64()), Some(0));
    }

    #[test]
    fn dispatch_suggest_event_malformed_query_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_suggest_event(&ws, 4, &serde_json::json!({ "query": "not-an-object" }));
        assert_invalid_params(&resp);
    }

    #[test]
    fn dispatch_trace_valid_event_returns_result() {
        let ws = Workspace::new();
        let resp = dispatch_trace(&ws, 5, &serde_json::json!({ "event": "OnAfterPost" }));
        assert!(resp.error.is_none(), "valid event must not error");
        assert!(resp.result.is_some());
        assert_eq!(resp.id, 5);
    }

    #[test]
    fn dispatch_trace_non_string_event_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_trace(&ws, 6, &serde_json::json!({ "event": 42 }));
        assert_invalid_params(&resp);
    }

    #[test]
    fn dispatch_trace_rejects_invalid_depth() {
        let ws = Workspace::new();
        for params in [
            serde_json::json!({ "event": "OnAfterPost", "depth": 10_000 }),
            serde_json::json!({ "event": "OnAfterPost", "depth": "10" }),
        ] {
            let resp = dispatch_trace(&ws, 7, &params);
            assert_invalid_params(&resp);
        }
    }

    #[test]
    fn dispatch_trace_chain_rejects_invalid_depth() {
        let ws = Workspace::new();
        let resp = dispatch_trace_chain(
            &ws,
            43,
            &serde_json::json!({ "event": "OnAfterPost", "depth": false }),
        );
        assert_invalid_params(&resp);
    }

    #[test]
    fn dispatch_entrypoints_returns_array_result() {
        let ws = Workspace::new();
        let resp = dispatch_entrypoints(&ws, 8);
        assert!(resp.error.is_none(), "entrypoints must not error");
        let value = resp.result.expect("entrypoints must carry a result");
        assert!(value.is_array(), "expected a JSON array of entry points");
        assert_eq!(resp.id, 8);
    }

    #[test]
    fn dispatch_insight_stats_reports_node_and_edge_counts() {
        let ws = Workspace::new();
        let resp = dispatch_insight_stats(&ws, 9);
        assert!(resp.error.is_none());
        let value = resp.result.expect("stats must carry a result");
        assert_eq!(value.get("nodes").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(value.get("edges").and_then(|v| v.as_u64()), Some(0));
    }

    #[test]
    fn dispatch_insight_stats_includes_workspace_objects() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/Logic.al"),
            r#"codeunit 50100 "Hello World"
{
    procedure Greet()
    begin
    end;
}
"#
            .to_string(),
        );
        let resp = dispatch_insight_stats(&ws, 9);
        assert!(resp.error.is_none());
        let value = resp.result.expect("stats must carry a result");
        let nodes = value.get("nodes").and_then(|v| v.as_u64()).unwrap_or(0);
        let edges = value.get("edges").and_then(|v| v.as_u64()).unwrap_or(0);
        assert!(
            nodes >= 2,
            "workspace object + procedure must appear as insight nodes; got {nodes}"
        );
        assert!(
            edges >= 1,
            "the Contains edge (object -> procedure) must exist; got {edges}"
        );
    }

    #[test]
    fn dispatch_entrypoints_includes_workspace_procedures() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/Logic.al"),
            r#"codeunit 50100 "Hello World"
{
    procedure Greet()
    begin
    end;
}
"#
            .to_string(),
        );
        let resp = dispatch_entrypoints(&ws, 8);
        assert!(resp.error.is_none());
        let value = resp.result.expect("result");
        let text = value.to_string();
        assert!(
            text.contains("Greet"),
            "workspace procedure with no callers must be an entry point; got: {text}"
        );
    }

    #[test]
    fn dispatch_dead_code_returns_array_result() {
        let ws = Workspace::new();
        let resp = dispatch_dead_code(&ws, 10);
        assert!(resp.error.is_none());
        let value = resp.result.expect("dead_code must carry a result");
        assert!(
            value.is_array(),
            "expected a JSON array of dead-code entries"
        );
    }

    #[tokio::test]
    async fn dispatch_native_check_returns_array_result() {
        let ws = Workspace::new();
        let resp = dispatch_native_check(&ws, 14).await;
        assert!(resp.error.is_none());
        let value = resp.result.expect("native_check must carry a result");
        assert!(
            value.is_array(),
            "expected a JSON array of native-check findings"
        );
        // Empty workspace → no findings.
        assert_eq!(value.as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn dispatch_native_check_reports_duplicate_id() {
        use std::path::PathBuf;
        let ws = Workspace::new();
        // Two codeunits sharing id 50100 in the workspace file index.
        ws.file_index.add_file(
            PathBuf::from("/virtual/nc/Foo.al"),
            "codeunit 50100 \"Foo\" { }".to_string(),
        );
        ws.file_index.add_file(
            PathBuf::from("/virtual/nc/Bar.al"),
            "codeunit 50100 \"Bar\" { }".to_string(),
        );
        let resp = dispatch_native_check(&ws, 15).await;
        let value = resp.result.expect("result");
        let text = value.to_string();
        assert!(
            text.contains("AL-NC001"),
            "duplicate id must surface AL-NC001; got: {text}"
        );
    }

    #[test]
    fn dispatch_graph_export_defaults_to_json() {
        let ws = Workspace::new();
        let resp = dispatch_graph_export(&ws, 11, &serde_json::json!({}));
        assert!(resp.error.is_none());
        let value = resp.result.expect("json export must carry a result");
        assert!(value.is_object());
        assert!(
            value.get("content").is_none(),
            "json export must not be wrapped in a dot envelope"
        );
    }

    #[test]
    fn dispatch_graph_export_dot_format_wraps_content() {
        let ws = Workspace::new();
        let resp = dispatch_graph_export(&ws, 12, &serde_json::json!({ "format": "dot" }));
        assert!(resp.error.is_none());
        let value = resp.result.expect("dot export must carry a result");
        assert_eq!(value.get("format").and_then(|v| v.as_str()), Some("dot"));
        assert!(value.get("content").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn dispatch_graph_export_unknown_format_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_graph_export(&ws, 13, &serde_json::json!({ "format": "xml" }));
        assert_invalid_params(&resp);
        assert!(resp.result.is_none());
    }

    #[test]
    fn dispatch_graph_export_under_cap_succeeds() {
        let ws = Workspace::new();
        let graph = ws.get_or_build_insight_graph().unwrap();
        assert!(
            graph.node_count() + graph.edge_count() <= MAX_GRAPH_EXPORT_NODES_AND_EDGES,
            "empty workspace must be under the export cap"
        );
        let resp = dispatch_graph_export(&ws, 14, &serde_json::json!({ "format": "json" }));
        assert!(
            resp.error.is_none(),
            "graphs under the cap must not be rejected"
        );
    }

    #[test]
    fn dispatch_impact_valid_symbol_returns_result() {
        let ws = Workspace::new();
        let resp = dispatch_impact(&ws, 15, &serde_json::json!({ "symbol": "MyCodeunit" }));
        assert!(resp.error.is_none(), "valid symbol must not error");
        let value = resp.result.expect("impact must carry a result");
        assert_eq!(
            value.get("symbol").and_then(|v| v.as_str()),
            Some("MyCodeunit")
        );
        assert!(value.get("impacted").is_some());
    }

    #[test]
    fn dispatch_impact_non_string_symbol_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_impact(&ws, 16, &serde_json::json!({ "symbol": 123 }));
        assert_invalid_params(&resp);
    }

    #[test]
    fn dispatch_suggest_event_structured_query_succeeds() {
        let ws = Workspace::new();
        let resp = dispatch_suggest_event(
            &ws,
            17,
            &serde_json::json!({
                "query": {
                    "source": { "type": "table", "table": "Customer" }
                }
            }),
        );
        assert!(
            resp.error.is_none(),
            "valid structured query must not error"
        );
        let value = resp.result.expect("suggest_event must carry a result");
        assert!(value.get("integrationPoints").is_some());
        assert!(value.get("partial").is_some());
    }

    #[test]
    fn dispatch_suggest_event_unknown_object_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_suggest_event(
            &ws,
            171,
            &serde_json::json!({
                "query": {
                    "source": {
                        "type": "procedure",
                        "object": "Does Not Exist"
                    }
                }
            }),
        );
        assert_invalid_params(&resp);
        assert!(resp
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("was not found")));
    }

    #[test]
    fn dispatch_suggest_event_legacy_flat_query_succeeds() {
        let ws = Workspace::new();
        let resp = dispatch_suggest_event(
            &ws,
            18,
            &serde_json::json!({
                "source": { "type": "table", "table": "Item" }
            }),
        );
        assert!(resp.error.is_none(), "valid flat query must not error");
        assert!(resp.result.is_some());
    }

    #[test]
    fn dispatch_suggest_event_missing_source_is_invalid_params() {
        let ws = Workspace::new();
        let resp = dispatch_suggest_event(&ws, 19, &serde_json::json!({ "unrelated": true }));
        assert_invalid_params(&resp);
    }
}
