//! Insight engine dispatchers — trace, entrypoints, graph export, dead code, impact, suggest_event.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_syntax::IdentifierText;
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
    // A subscriber's body calls are outgoing edges of a procedure the lazy
    // graph may not have resolved yet.
    if let Err(error) = workspace.complete_workspace_call_edges() {
        return graph_build_error(id, "trace", error);
    }
    let (graph, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "trace", error),
    };
    // Supply the call graph so the trace can follow the events a subscriber's
    // body actually raises instead of fabricating hops from its object's
    // unrelated publishers.
    let steps = al_insight::search::trace_event(&graph, _cg_guard.as_ref(), event_name, max_depth);
    serialized_response(id, &steps, "trace")
}

pub(super) fn dispatch_entrypoints(workspace: &Workspace, id: u64) -> Response {
    // Callers are incoming edges, which only a fully resolved graph has.
    if let Err(error) = workspace.complete_workspace_call_edges() {
        return graph_build_error(id, "entrypoints", error);
    }
    let (graph, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "entrypoints", error),
    };
    // Call edges live in the CallGraph, not the InsightGraph — pass it, or the
    // filter has nothing to exclude and every procedure looks like an entry
    // point.
    let entry_points = al_insight::search::find_entry_points(&graph, _cg_guard.as_ref());
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
    let scope = match super::scope::scope_param(params) {
        Ok(scope) => scope,
        Err(message) => return super::rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let (graph, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => return graph_build_error(id, "graph export", error),
    };

    let keep = scope.map(|scope| scoped_graph_nodes(workspace, &graph, scope));
    let kept = |index: usize| keep.as_ref().is_none_or(|keep| keep.contains(&index));

    // Refuse to materialise an unbounded graph into one JSON-RPC response.
    // The whole exported document lives in memory twice (the String/Value
    // *and* the framed JSON-RPC body), so even a "moderately large"
    // workspace can OOM the daemon's tokio worker thread.
    let size = match &keep {
        None => graph.node_count() + graph.edge_count(),
        Some(keep) => al_insight::search::slice_size(&graph, keep),
    };
    if size > MAX_GRAPH_EXPORT_NODES_AND_EDGES {
        let narrower = if scope == Some(super::scope::Scope::Workspace) {
            "Use the trace or impact endpoints to narrow the query."
        } else {
            "Export the workspace part with scope 'workspace' (`--scope workspace`), or use \
             the trace or impact endpoints."
        };
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!(
                    "Graph too large to export in one response: {size} nodes+edges \
                     exceeds cap of {MAX_GRAPH_EXPORT_NODES_AND_EDGES}. {narrower}"
                ),
            }),
            ..Default::default()
        };
    }

    let mut result = match format {
        "dot" => {
            let dot = al_insight::search::export_dot_where(&graph, kept);
            serde_json::json!({ "format": "dot", "content": dot })
        }
        "json" => match serde_json::to_value(al_insight::search::export_json_where(&graph, kept)) {
            Ok(value) => value,
            Err(error) => {
                return super::rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("serializing graphExport: {error}"),
                );
            }
        },
        _ => unreachable!("format was validated above"),
    };
    if let (Some(scope), Some(keep), serde_json::Value::Object(object)) =
        (scope, &keep, &mut result)
    {
        object.insert("scope".into(), serde_json::json!(scope.label()));
        object.insert(
            "outOfScopeCount".into(),
            serde_json::json!(graph.node_count() - keep.len()),
        );
    }
    Response {
        id,
        result: Some(result),
        error: None,
        ..Default::default()
    }
}

/// The graph nodes a scope keeps, by index.
///
/// `workspace` keeps the nodes of the workspace's objects and the nodes one
/// edge away from them, so a call into Base Application shows where it goes
/// without the rest of Base Application. `packages` keeps everything else.
fn scoped_graph_nodes(
    workspace: &Workspace,
    graph: &al_insight::graph::InsightGraph,
    scope: super::scope::Scope,
) -> std::collections::HashSet<usize> {
    let names = super::scope::workspace_object_names(workspace);
    let own = |name: &str| names.contains(&name.to_lowercase());
    match scope {
        super::scope::Scope::All => al_insight::search::graph_slice(graph, |_| true, false),
        super::scope::Scope::Packages => {
            al_insight::search::graph_slice(graph, |name| !own(name), false)
        }
        super::scope::Scope::Workspace => al_insight::search::graph_slice(graph, own, true),
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
    // Consumers are found through the workspace-enriched index; without the
    // enrichment pass a workspace symbol looks like it has none.
    if let Err(error) = workspace.get_or_build_call_graph() {
        return graph_build_error(id, "impact", error);
    }
    match al_analysis::queries::impact::impact(workspace, symbol) {
        // An empty list reads the same whether the symbol is unused or
        // misspelled, so resolve the name before reporting zero.
        Ok(entries) if entries.is_empty() => match resolve_impact_symbol(workspace, symbol) {
            SymbolResolution::Found => Response {
                id,
                result: Some(serde_json::json!({ "symbol": symbol, "impacted": entries })),
                error: None,
                ..Default::default()
            },
            SymbolResolution::UnknownObject { name, candidates } => {
                not_found_error(id, "impact", "object", &name, &candidates)
            }
            SymbolResolution::UnknownMember {
                object,
                member,
                candidates,
            } => rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!(
                    "impact: object '{object}' has no member named '{member}'. {}",
                    if candidates.is_empty() {
                        format!("Call `source \"{object}\" --list-procedures` for its members.")
                    } else {
                        format!("Closest members: {}.", candidates.join(", "))
                    }
                ),
            ),
        },
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

    // The trace walks outgoing call edges, which the lazy graph leaves
    // unresolved for most workspace procedures until a query reaches them:
    // every run reported "still being analyzed" and stopped there.
    if let Err(error) = workspace.complete_workspace_call_edges() {
        return graph_build_error(id, "suggestEvent", error);
    }
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
    // Workspace objects reach `workspace.symbols` through the call-graph
    // enrichment pass. Without it this method saw package entries only and
    // answered `totalImpacts: 0` for a table that workspace pages use.
    if let Err(error) = workspace.get_or_build_call_graph() {
        return graph_build_error(id, "tableImpact", error);
    }
    let mut result = al_insight::analysis::table_impact(&workspace.symbols, table);
    al_insight::analysis::add_workspace_local_record_variables(
        &mut result,
        &workspace.file_index,
        table,
    );
    if result.total_impacts == 0 {
        if let SymbolResolution::UnknownObject { name, candidates } =
            resolve_impact_symbol(workspace, table)
        {
            return not_found_error(id, "tableImpact", "table", &name, &candidates);
        }
    }
    serialized_response(id, &result, "tableImpact")
}

/// What `impact`'s `symbol` argument turned out to name.
///
/// An agent cannot act on a bare empty list: it reads the same whether the
/// symbol is unused or misspelled. The survey's `impact "Sales-Post.PostSalesDoc"`
/// returned `{"impacted": []}` for a procedure whose real name is `RunWithCheck`.
enum SymbolResolution {
    Found,
    UnknownObject {
        name: String,
        candidates: Vec<String>,
    },
    UnknownMember {
        object: String,
        member: String,
        candidates: Vec<String>,
    },
}

fn resolve_impact_symbol(workspace: &Workspace, symbol: &str) -> SymbolResolution {
    let (object, member) = split_object_member(symbol);
    let entries = workspace.symbols.get_by_name(&object);
    // Every object of every file, not only each file's first.
    let in_workspace_files = workspace.file_index.object_infos.iter().any(|entry| {
        entry
            .value()
            .iter()
            .any(|info| info.name.eq_ignore_ascii_case(&object))
    });
    if entries.is_empty() && !in_workspace_files {
        let candidates = unknown_symbol_candidates(workspace, &object);
        return SymbolResolution::UnknownObject {
            name: object,
            candidates,
        };
    }
    let Some(member) = member else {
        return SymbolResolution::Found;
    };
    // A workspace object indexed only in the file index carries no members
    // here, so an unverifiable member is accepted rather than rejected.
    if entries.is_empty() {
        return SymbolResolution::Found;
    }
    let mut members: Vec<String> = Vec::new();
    for entry in &entries {
        members.extend(entry.methods.iter().map(|method| method.name.clone()));
        members.extend(entry.fields.iter().map(|field| field.name.clone()));
        members.extend(entry.controls.iter().map(|control| control.name.clone()));
    }
    if members
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&member))
    {
        return SymbolResolution::Found;
    }
    let member_lower = member.to_lowercase();
    let mut candidates: Vec<String> = members
        .iter()
        .filter(|candidate| {
            let lower = candidate.to_lowercase();
            lower.contains(&member_lower) || member_lower.contains(&lower)
        })
        .cloned()
        .collect();
    candidates.sort();
    candidates.dedup();
    candidates.truncate(8);
    SymbolResolution::UnknownMember {
        object,
        member,
        candidates,
    }
}

/// Split `Object."Member"` into its two halves, ignoring a dot inside quotes.
fn split_object_member(symbol: &str) -> (String, Option<String>) {
    let mut in_quotes = false;
    for (index, character) in symbol.char_indices() {
        match character {
            '"' => in_quotes = !in_quotes,
            '.' if !in_quotes => {
                return (
                    symbol[..index].unquote_identifier().into_owned(),
                    Some(symbol[index + 1..].unquote_identifier().into_owned()),
                );
            }
            _ => {}
        }
    }
    (symbol.unquote_identifier().into_owned(), None)
}

/// Close-enough object names for a name the index does not hold. Uses the same
/// fuzzy matcher `search` serves, over both packages and workspace source, so
/// the suggestion is a name the agent's next call can use verbatim.
fn unknown_symbol_candidates(workspace: &Workspace, name: &str) -> Vec<String> {
    let mut candidates: Vec<String> = workspace
        .symbols
        .search(name, 8)
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    // `workspace_search` matches substrings, so a misspelling in the last few
    // characters finds nothing. Shortening the query until it matches turns
    // "Sales Postr" into the prefix that does hit "Sales Poster".
    let mut prefix_length = name.len();
    while prefix_length >= 3 {
        let Some(prefix) = name.get(..prefix_length) else {
            prefix_length -= 1;
            continue;
        };
        let found = al_analysis::queries::search::workspace_search(workspace, prefix, 8);
        if !found.is_empty() {
            candidates.extend(found.into_iter().map(|result| result.info.name));
            break;
        }
        prefix_length -= 1;
    }
    candidates.sort();
    candidates.dedup();
    candidates.truncate(8);
    candidates
}

/// A not-found error naming the closest candidates, so the agent's next call
/// can be the right one instead of a full dump to find the real name.
fn not_found_error(
    id: u64,
    method: &str,
    noun: &str,
    name: &str,
    candidates: &[String],
) -> Response {
    let suggestion = if candidates.is_empty() {
        format!("Run `search {name}` to find the name that does exist.")
    } else {
        format!("Closest names in the index: {}.", candidates.join(", "))
    };
    rpc_error(
        id,
        error_codes::INVALID_PARAMS,
        &format!("{method}: no {noun} named '{name}' is loaded. {suggestion}"),
    )
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

    // A subscriber's body calls are outgoing edges of a procedure the lazy
    // graph may not have resolved yet.
    if let Err(error) = workspace.complete_workspace_call_edges() {
        return graph_build_error(id, "traceChain", error);
    }
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

    /// `impact` checks the name against workspace files before the symbol
    /// index holds workspace objects. A codeunit declared after a table in
    /// the same file was reported as an unknown object.
    #[test]
    fn impact_knows_the_second_object_of_a_workspace_file() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/ws/Posting.al"),
            "table 50200 \"Posting Buffer\" { }\n\ncodeunit 50100 \"Posting Mgt\" { procedure Post() begin end; }\n"
                .to_string(),
        );
        assert!(matches!(
            resolve_impact_symbol(&workspace, "Posting Mgt"),
            SymbolResolution::Found
        ));
    }

    /// A procedure called only from a low-fanout file had no resolved
    /// incoming edge, so `entrypoints` listed it as never called.
    #[test]
    fn entrypoints_leaves_out_procedures_called_from_their_own_object() {
        let workspace = Workspace::new();
        // A busier file puts the probe below the eager tier, whose threshold
        // is relative: a lone file is always in it.
        workspace.file_index.add_file(
            std::path::PathBuf::from("/ws/Busy.Codeunit.al"),
            "codeunit 50107 Busy\n{\n    procedure Run()\n    begin\n        Step(); Step(); Step(); Step(); Step(); Step();\n    end;\n\n    procedure Step()\n    begin\n    end;\n}\n"
                .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/ws/CallProbe.Codeunit.al"),
            "codeunit 50106 \"Call Probe\"\n{\n    trigger OnRun()\n    begin\n        FromTrigger();\n    end;\n\n    procedure Caller()\n    begin\n        FromProcedure();\n    end;\n\n    procedure FromTrigger()\n    begin\n    end;\n\n    procedure FromProcedure()\n    begin\n    end;\n}\n"
                .to_string(),
        );

        let response = dispatch_entrypoints(&workspace, 1);

        let names: Vec<String> = response
            .result
            .unwrap_or_else(|| panic!("{:?}", response.error))
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["name"].as_str().map(str::to_string))
            .collect();
        assert!(names.contains(&"Caller".to_string()), "{names:?}");
        assert!(!names.contains(&"FromTrigger".to_string()), "{names:?}");
        assert!(!names.contains(&"FromProcedure".to_string()), "{names:?}");
    }

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
    fn malformed_dependency_source_degrades_per_file_instead_of_failing() {
        // One unparsable embedded .al in a dependency package must not take
        // down whole-workspace insight: the bad file is skipped (with a
        // warning at index time) and the event map still builds from
        // everything that did parse.
        let (workspace, _package) = workspace_with_malformed_dependency_source();

        let response = dispatch_event_map(&workspace, 90);
        assert!(
            response.error.is_none(),
            "a skipped malformed dependency file must not fail the event map: {:?}",
            response.error
        );
        let result = response
            .result
            .expect("event map must be produced from the files that parsed");
        let events = result
            .get("events")
            .and_then(|v| v.as_array())
            .expect("event map result carries an events array");
        assert!(
            events.is_empty(),
            "the skipped file's contents must not fabricate events: {events:?}"
        );
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

    /// One unparsable file no longer takes the whole query down: it is skipped
    /// per file (see `al_analysis::workspace_sources`), and the remaining files
    /// still produce a report.
    #[test]
    fn dispatch_impact_degrades_per_file_on_a_malformed_workspace_source() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Broken.al"),
            "codeunit 50100 Broken { procedure Incomplete(".to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Uses.Codeunit.al"),
            "codeunit 50101 Uses\n{\n    procedure P()\n    var\n        C: Record Customer;\n    begin\n    end;\n}\n"
                .to_string(),
        );

        let resp = dispatch_impact(&ws, 4, &serde_json::json!({ "symbol": "Customer" }));
        assert!(
            resp.error.is_none(),
            "one broken file must not fail the whole query: {:?}",
            resp.error
        );
        let value = resp.result.expect("must carry a result");
        let impacted = value
            .get("impacted")
            .and_then(|value| value.as_array())
            .expect("impact result carries an 'impacted' array");
        assert!(
            impacted
                .iter()
                .any(|entry| entry.get("n").and_then(|v| v.as_str()) == Some("Uses")),
            "the parsable file must still be reported: {value}"
        );
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
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Customer.Table.al"),
            "table 18 Customer\n{\n}\n".to_string(),
        );
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

    /// `trace` timed out at 30 s twice in a row while `dead-code` answered in
    /// 47 ms on the same daemon. `dead-code` reads the workspace file index;
    /// `trace` waits for the dependency AL source index, which takes about a
    /// minute on Base Application and only starts when a query first needs it.
    /// Once that index exists the traversal itself is bounded by the event's
    /// own subscriber fan-out, not by the graph size, which is what this
    /// asserts: a graph with a thousand unrelated events costs the same as one
    /// with a handful.
    #[test]
    fn trace_cost_follows_the_event_not_the_graph_size() {
        fn workspace_with_events(unrelated: usize) -> Workspace {
            let ws = Workspace::new();
            ws.file_index.add_file(
                std::path::PathBuf::from("/proj/Target.Codeunit.al"),
                "codeunit 50000 \"Target Publisher\"\n{\n    [IntegrationEvent(false, false)]\n    procedure OnTargetEvent()\n    begin\n    end;\n}\n".to_string(),
            );
            for index in 0..unrelated {
                ws.file_index.add_file(
                    std::path::PathBuf::from(format!("/proj/Noise{index}.Codeunit.al")),
                    format!(
                        "codeunit {} \"Noise {index}\"\n{{\n    [IntegrationEvent(false, false)]\n    procedure OnNoise{index}()\n    begin\n    end;\n}}\n",
                        50_100 + index
                    ),
                );
            }
            // Pay the graph build before timing the query. The guard is
            // dropped at the end of this statement, which is what lets the
            // workspace be returned.
            drop(ws.get_or_build_call_graph().expect("graph builds"));
            ws
        }

        let small = workspace_with_events(4);
        let large = workspace_with_events(1_000);
        let params = serde_json::json!({ "event": "OnTargetEvent" });

        let time = |ws: &Workspace| {
            let start = std::time::Instant::now();
            let response = dispatch_trace(ws, 60, &params);
            assert!(response.error.is_none(), "{:?}", response.error);
            start.elapsed()
        };
        let small_elapsed = time(&small);
        let large_elapsed = time(&large);

        // Generous headroom: the point is that the cost does not scale with
        // the 250x larger graph, not a precise ratio on a shared runner.
        assert!(
            large_elapsed < small_elapsed + std::time::Duration::from_millis(250),
            "trace on a 1000-event graph took {large_elapsed:?} against {small_elapsed:?} on a \
             4-event graph; a per-call whole-graph walk would show up here"
        );
    }

    /// A cold call-graph build is 86 s on a project with Base Application
    /// loaded. Two `trace` calls arriving before it finishes must join the
    /// same build: running a second copy is the difference between one wait
    /// and two, and a client that retried after its deadline used to be the
    /// second caller.
    #[test]
    fn concurrent_cold_traces_share_one_call_graph_build() {
        let ws = std::sync::Arc::new(Workspace::new());
        for index in 0..40 {
            ws.file_index.add_file(
                std::path::PathBuf::from(format!("/proj/Object{index}.al")),
                format!(
                    "codeunit {} \"Object {index}\"\n{{\n    [IntegrationEvent(false, false)]\n    procedure OnThing{index}()\n    begin\n    end;\n}}\n",
                    50_100 + index
                ),
            );
        }
        assert_eq!(ws.call_graph_build_count(), 0, "nothing built yet");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let ws = std::sync::Arc::clone(&ws);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let response =
                        dispatch_trace(&ws, 70, &serde_json::json!({ "event": "OnThing0" }));
                    assert!(response.error.is_none(), "{:?}", response.error);
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("trace worker finished");
        }

        assert_eq!(
            ws.call_graph_build_count(),
            1,
            "four concurrent cold traces must share one build"
        );
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

    /// A project with Base Application could never export its graph, and
    /// `--scope` was ignored.
    #[test]
    fn dispatch_graph_export_scope_workspace_leaves_package_objects_out() {
        use al_symbols::{MethodSymbol, ObjectKind, SymbolEntry};
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Mine.al"),
            "codeunit 50100 Mine\n{\n    procedure Run()\n    begin\n    end;\n}\n".to_string(),
        );
        let mut package = SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 7,
            name: "Unrelated Base".to_string(),
            package: "Base Application".to_string(),
            ..Default::default()
        };
        package.methods = vec![MethodSymbol {
            name: "Elsewhere".to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        }];
        ws.symbols.add_entries(&[package]);

        let whole = dispatch_graph_export(&ws, 14, &serde_json::json!({ "format": "dot" }));
        let whole = whole.result.expect("result")["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(whole.contains("Unrelated Base"), "{whole}");

        let scoped = dispatch_graph_export(
            &ws,
            15,
            &serde_json::json!({ "format": "dot", "scope": "workspace" }),
        );
        let scoped = scoped.result.expect("result");
        let content = scoped["content"].as_str().unwrap();
        assert!(content.contains("Mine"), "{content}");
        assert!(!content.contains("Unrelated Base"), "{content}");
        assert_eq!(scoped["scope"], "workspace");
        assert!(scoped["outOfScopeCount"].as_u64().unwrap() > 0);

        let bad = dispatch_graph_export(&ws, 16, &serde_json::json!({ "scope": "mine" }));
        assert_invalid_params(&bad);
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
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/MyCodeunit.al"),
            "codeunit 50100 MyCodeunit\n{\n}\n".to_string(),
        );
        let resp = dispatch_impact(&ws, 15, &serde_json::json!({ "symbol": "MyCodeunit" }));
        assert!(resp.error.is_none(), "valid symbol must not error");
        let value = resp.result.expect("impact must carry a result");
        assert_eq!(
            value.get("symbol").and_then(|v| v.as_str()),
            Some("MyCodeunit")
        );
        assert!(value.get("impacted").is_some());
    }

    /// An empty `impacted` list and a misspelled name used to be the same
    /// answer, so an agent could not tell a real zero from a typo.
    #[test]
    fn dispatch_impact_on_an_unknown_object_says_not_found_with_candidates() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/SalesPost.al"),
            "codeunit 50100 \"Sales Poster\"\n{\n}\n".to_string(),
        );
        let resp = dispatch_impact(&ws, 50, &serde_json::json!({ "symbol": "Sales Postr" }));
        assert_invalid_params(&resp);
        let message = &resp.error.as_ref().expect("error").message;
        assert!(
            message.contains("no object named 'Sales Postr'"),
            "got: {message}"
        );
        assert!(
            message.contains("Sales Poster"),
            "the close name must be offered: {message}"
        );
    }

    #[test]
    fn dispatch_impact_on_an_unknown_member_lists_the_real_members() {
        use al_symbols::{MethodSymbol, ObjectKind, SymbolEntry};
        let ws = Workspace::new();
        let mut entry = SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            package: "Base Application".to_string(),
            ..Default::default()
        };
        entry.methods = vec![MethodSymbol {
            name: "RunWithCheck".to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        }];
        ws.symbols.add_entries(&[entry]);

        let resp = dispatch_impact(
            &ws,
            51,
            &serde_json::json!({ "symbol": "Sales-Post.PostSalesDoc" }),
        );
        assert_invalid_params(&resp);
        let message = &resp.error.as_ref().expect("error").message;
        assert!(
            message.contains("has no member named 'PostSalesDoc'"),
            "got: {message}"
        );
    }

    /// A workspace page bound to the table through `SourceTable` used to be
    /// invisible here, so a table plainly in use reported `totalImpacts: 0`.
    #[test]
    fn dispatch_table_impact_counts_a_workspace_page_source_table() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Staging.Table.al"),
            "table 50130 \"Work Order Staging\"\n{\n    fields { field(1; \"No.\"; Code[20]) { } }\n}\n"
                .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/StagingList.Page.al"),
            "page 50130 \"Work Order List\"\n{\n    PageType = List;\n    SourceTable = \"Work Order Staging\";\n}\n"
                .to_string(),
        );

        let resp = dispatch_table_impact(
            &ws,
            52,
            &serde_json::json!({ "table": "Work Order Staging" }),
        );
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let value = resp.result.expect("result");
        assert!(
            value
                .get("totalImpacts")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                > 0,
            "the page must count as an impact: {value}"
        );
    }

    #[test]
    fn dispatch_table_impact_on_an_unknown_table_says_not_found() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Staging.Table.al"),
            "table 50130 \"Work Order Staging\"\n{\n}\n".to_string(),
        );
        let resp = dispatch_table_impact(
            &ws,
            53,
            &serde_json::json!({ "table": "Work Order Stagin" }),
        );
        assert_invalid_params(&resp);
        assert!(resp
            .error
            .as_ref()
            .expect("error")
            .message
            .contains("Work Order Staging"));
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
