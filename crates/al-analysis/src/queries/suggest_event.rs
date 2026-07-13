//! Structured integration point discovery.
//!
//! Given an object+procedure, table, or event, traces the call/event graph
//! to find all reachable integration points (events that fire along the
//! execution path). Supports filtering results to events that expose a
//! specific table as a `var` parameter.

use std::collections::HashSet;
use std::sync::Arc;

use al_symbols::{ObjectKind, SymbolIndex};
use serde::{Deserialize, Serialize};

use al_insight::graph::{EventNodeType, InsightGraph, InsightNode, NodeKey};
use al_insight::index::{CallGraph, EdgeResolutionState, NodeId};
use al_symbols::ParameterSymbol;
use al_workspace::Workspace;

/// Resolve an object's `ObjectKind` from the symbol index, defaulting to
/// `Codeunit` when the object is not found.
fn resolve_object_kind(workspace: &Workspace, object_name: &str) -> ObjectKind {
    workspace
        .symbols
        .get_by_name(object_name)
        .into_iter()
        .next()
        .map(|e| e.kind)
        .unwrap_or(ObjectKind::Codeunit)
}

fn map_parameters_to_param_info(parameters: &[ParameterSymbol]) -> Vec<ParamInfo> {
    parameters
        .iter()
        .map(|p| ParamInfo {
            name: p.name.clone(),
            type_name: p.type_name.clone(),
            is_var: p.is_var,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventQuery {
    pub source: QuerySource,
    #[serde(default)]
    pub filter_table: Option<String>,
    #[serde(default)]
    pub filter_field: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum QuerySource {
    /// Start from an object (and optionally a specific procedure).
    Procedure {
        object: String,
        #[serde(default)]
        procedure: Option<String>,
    },
    /// Start from a table — find all events where this table is a var param.
    Table { table: String },
    /// Start from a specific event publisher.
    Event { object: String, event: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestEventResult {
    pub integration_points: Vec<IntegrationPoint>,
    /// True when any procedure in the trace had unresolved call edges
    /// (i.e. source not yet parsed), so results may be incomplete.
    pub partial: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationPoint {
    pub event: String,
    pub object: String,
    /// "integration" | "business" | "trigger"
    pub event_type: String,
    pub params: Vec<ParamInfo>,
    /// Breadcrumb trace from the query source to this event.
    pub path: Vec<TraceHop>,
    /// Ready-to-paste [EventSubscriber] attribute.
    pub example: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamInfo {
    pub name: String,
    pub type_name: String,
    pub is_var: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceHop {
    pub object: String,
    pub procedure: String,
    pub edge_kind: String,
}

pub fn suggest_event(workspace: &Workspace, query: &EventQuery) -> SuggestEventResult {
    match &query.source {
        QuerySource::Procedure { object, procedure } => query_procedure(
            workspace,
            object,
            procedure.as_deref(),
            query.filter_table.as_deref(),
            query.filter_field.as_deref(),
        ),
        QuerySource::Table { table } => {
            query_table(workspace, table, query.filter_field.as_deref())
        }
        QuerySource::Event { object, event } => query_event(
            workspace,
            object,
            event,
            query.filter_table.as_deref(),
            query.filter_field.as_deref(),
        ),
    }
}

fn query_procedure(
    workspace: &Workspace,
    object_name: &str,
    procedure_name: Option<&str>,
    filter_table: Option<&str>,
    filter_field: Option<&str>,
) -> SuggestEventResult {
    let (insight, cg_guard) = workspace.get_or_build_call_graph();
    let cg_opt = cg_guard.as_ref();

    let object_kind = resolve_object_kind(workspace, object_name);

    let mut points: Vec<IntegrationPoint> = Vec::new();
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut partial = false;

    if let Some(proc_name) = procedure_name {
        let proc_key = NodeKey::Procedure(
            object_kind,
            object_name.to_lowercase(),
            proc_name.to_lowercase(),
        );
        if let Some(node_id) = cg_opt.and_then(|_cg| CallGraph::node_id_for(&insight, &proc_key)) {
            let hop = TraceHop {
                object: object_name.to_string(),
                procedure: proc_name.to_string(),
                edge_kind: "start".to_string(),
            };
            if let Some(cg) = cg_opt {
                if cg.resolution_state(node_id) == EdgeResolutionState::Unresolved {
                    partial = true;
                }
                trace_from_node(
                    node_id,
                    &insight,
                    cg,
                    &workspace.symbols,
                    &mut points,
                    &mut visited,
                    &mut partial,
                    vec![hop],
                    0,
                    10,
                );
            }
        }
    } else {
        let obj_lower = object_name.to_lowercase();
        for key in insight.index.keys() {
            let proc_id = match key {
                NodeKey::Procedure(kind, obj, _name)
                    if *kind == object_kind && obj == &obj_lower =>
                {
                    CallGraph::node_id_for(&insight, key)
                }
                _ => None,
            };
            if let Some(node_id) = proc_id {
                if visited.contains(&node_id) {
                    continue;
                }
                let proc_name = match &insight.graph[petgraph::graph::NodeIndex::new(node_id.0)] {
                    InsightNode::Procedure { name, .. } => name.clone(),
                    _ => continue,
                };
                let hop = TraceHop {
                    object: object_name.to_string(),
                    procedure: proc_name,
                    edge_kind: "start".to_string(),
                };
                if let Some(cg) = cg_opt {
                    if cg.resolution_state(node_id) == EdgeResolutionState::Unresolved {
                        partial = true;
                    }
                    trace_from_node(
                        node_id,
                        &insight,
                        cg,
                        &workspace.symbols,
                        &mut points,
                        &mut visited,
                        &mut partial,
                        vec![hop],
                        0,
                        10,
                    );
                }
            }
        }

        collect_published_events(
            &insight,
            &workspace.symbols,
            object_kind,
            object_name,
            &mut points,
        );
    }

    points = dedup_points(points);
    points = apply_filters(points, filter_table, filter_field);
    SuggestEventResult {
        integration_points: points,
        partial,
    }
}

fn query_table(
    workspace: &Workspace,
    table_name: &str,
    filter_field: Option<&str>,
) -> SuggestEventResult {
    let (insight, _cg_guard) = workspace.get_or_build_call_graph();

    let mut points: Vec<IntegrationPoint> = Vec::new();

    collect_published_events(
        &insight,
        &workspace.symbols,
        ObjectKind::Table,
        table_name,
        &mut points,
    );

    let table_lower = table_name.to_lowercase();
    // Scan ALL event publishers for those with a `var Record "TableName"` parameter.
    let all_events = al_symbols::get_events(&workspace.symbols, "");
    for pub_event in &all_events.publishers {
        let has_table_var = pub_event
            .method
            .parameters
            .iter()
            .any(|p| p.is_var && is_record_of_table(&p.type_name, &table_lower));

        if !has_table_var {
            continue;
        }

        let obj = &pub_event.object;
        let event_type_str = match pub_event.event_type {
            al_symbols::EventType::Integration => "integration",
            al_symbols::EventType::Business => "business",
        }
        .to_string();

        let params = map_parameters_to_param_info(&pub_event.method.parameters);

        let example = format_example(obj.kind, &obj.name, &pub_event.method.name);

        points.push(IntegrationPoint {
            event: pub_event.method.name.clone(),
            object: obj.name.clone(),
            event_type: event_type_str,
            params,
            path: Vec::new(),
            example,
        });
    }

    points = dedup_points(points);
    points = apply_filters(points, None, filter_field);
    SuggestEventResult {
        integration_points: points,
        partial: false,
    }
}

fn query_event(
    workspace: &Workspace,
    object_name: &str,
    event_name: &str,
    filter_table: Option<&str>,
    filter_field: Option<&str>,
) -> SuggestEventResult {
    let (insight, cg_guard) = workspace.get_or_build_call_graph();
    let cg_opt = cg_guard.as_ref();

    let object_kind = resolve_object_kind(workspace, object_name);

    let mut points: Vec<IntegrationPoint> = Vec::new();
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut partial = false;

    let event_key = NodeKey::Event(
        object_kind,
        object_name.to_lowercase(),
        event_name.to_lowercase(),
    );

    if let Some(event_node_id) = CallGraph::node_id_for(&insight, &event_key) {
        let (event_type_str, params) =
            resolve_event_details(&insight, &workspace.symbols, &event_key);
        let example = format_example(object_kind, object_name, event_name);

        points.push(IntegrationPoint {
            event: event_name.to_string(),
            object: object_name.to_string(),
            event_type: event_type_str,
            params,
            path: Vec::new(),
            example,
        });

        visited.insert(event_node_id);

        if let Some(cg) = cg_opt {
            for sub_id in cg.subscribers_of(event_node_id) {
                if visited.contains(&sub_id) {
                    continue;
                }
                let sub_hop = match &insight.graph[petgraph::graph::NodeIndex::new(sub_id.0)] {
                    InsightNode::Subscriber {
                        object_name: obj,
                        name,
                        ..
                    } => TraceHop {
                        object: obj.clone(),
                        procedure: name.clone(),
                        edge_kind: "event_subscription".to_string(),
                    },
                    _ => continue,
                };

                trace_from_node(
                    sub_id,
                    &insight,
                    cg,
                    &workspace.symbols,
                    &mut points,
                    &mut visited,
                    &mut partial,
                    vec![sub_hop],
                    0,
                    10,
                );
            }
        }
    }

    points = dedup_points(points);
    points = apply_filters(points, filter_table, filter_field);
    SuggestEventResult {
        integration_points: points,
        partial,
    }
}

/// Recursively trace from a node, collecting events along the way.
// All arguments are recursion state (graph / call-graph / current node / visited
// set / filter / accumulator / depth) that flows through every call. Bundling
// them into a Context struct moves the cognitive load rather than reducing it.
#[allow(clippy::too_many_arguments)]
fn trace_from_node(
    node_id: NodeId,
    insight: &InsightGraph,
    cg: &CallGraph,
    symbols: &Arc<SymbolIndex>,
    points: &mut Vec<IntegrationPoint>,
    visited: &mut HashSet<NodeId>,
    partial: &mut bool,
    path: Vec<TraceHop>,
    depth: usize,
    max_depth: usize,
) {
    if depth >= max_depth || visited.contains(&node_id) {
        return;
    }
    visited.insert(node_id);

    let node_idx = petgraph::graph::NodeIndex::new(node_id.0);
    let node = &insight.graph[node_idx];

    match node {
        InsightNode::Event {
            object_kind,
            object_name,
            name,
            event_type,
        } => {
            let event_type_str = match event_type {
                EventNodeType::Integration => "integration",
                EventNodeType::Business => "business",
            }
            .to_string();

            let params =
                lookup_event_params(symbols, *object_kind, &object_name.to_lowercase(), name);
            let example = format_example(*object_kind, object_name, name);

            points.push(IntegrationPoint {
                event: name.clone(),
                object: object_name.clone(),
                event_type: event_type_str,
                params,
                path: path.clone(),
                example,
            });

            for sub_id in cg.subscribers_of(node_id) {
                if visited.contains(&sub_id) {
                    continue;
                }
                let sub_hop = match &insight.graph[petgraph::graph::NodeIndex::new(sub_id.0)] {
                    InsightNode::Subscriber {
                        object_name: obj,
                        name: sub_name,
                        ..
                    } => TraceHop {
                        object: obj.clone(),
                        procedure: sub_name.clone(),
                        edge_kind: "event_subscription".to_string(),
                    },
                    _ => continue,
                };

                let mut new_path = path.clone();
                new_path.push(sub_hop);
                trace_from_node(
                    sub_id,
                    insight,
                    cg,
                    symbols,
                    points,
                    visited,
                    partial,
                    new_path,
                    depth + 1,
                    max_depth,
                );
            }
        }

        InsightNode::Procedure {
            object_name, name, ..
        }
        | InsightNode::Subscriber {
            object_name, name, ..
        } => {
            if cg.resolution_state(node_id) == EdgeResolutionState::Unresolved {
                *partial = true;
            }

            for edge in cg.callees_of(node_id) {
                if visited.contains(&edge.to) {
                    continue;
                }
                let hop = TraceHop {
                    object: object_name.clone(),
                    procedure: name.clone(),
                    edge_kind: edge.kind.to_string(),
                };
                let mut new_path = path.clone();
                new_path.push(hop);
                trace_from_node(
                    edge.to,
                    insight,
                    cg,
                    symbols,
                    points,
                    visited,
                    partial,
                    new_path,
                    depth + 1,
                    max_depth,
                );
            }
        }

        InsightNode::Object { .. } => {
            // Objects are not traced directly.
        }
    }
}

/// Collect events directly published by `object_name` of `object_kind`.
fn collect_published_events(
    insight: &InsightGraph,
    symbols: &Arc<SymbolIndex>,
    object_kind: ObjectKind,
    object_name: &str,
    points: &mut Vec<IntegrationPoint>,
) {
    let obj_lower = object_name.to_lowercase();
    for key in insight.index.keys() {
        if let NodeKey::Event(kind, obj, _event_lower) = key {
            if *kind == object_kind && obj == &obj_lower {
                let (event_type_str, params) = resolve_event_details(insight, symbols, key);
                let first_idx = match insight.index[key].first() {
                    Some(idx) => *idx,
                    None => continue,
                };
                let event_name = match insight.graph[first_idx] {
                    InsightNode::Event { ref name, .. } => name.clone(),
                    _ => continue,
                };
                let example = format_example(object_kind, object_name, &event_name);
                points.push(IntegrationPoint {
                    event: event_name,
                    object: object_name.to_string(),
                    event_type: event_type_str,
                    params,
                    path: Vec::new(),
                    example,
                });
            }
        }
    }
}

fn resolve_event_details(
    insight: &InsightGraph,
    symbols: &Arc<SymbolIndex>,
    event_key: &NodeKey,
) -> (String, Vec<ParamInfo>) {
    if let NodeKey::Event(kind, obj_lower, event_lower) = event_key {
        let event_type_str =
            if let Some(&idx) = insight.index.get(event_key).and_then(|v| v.first()) {
                match &insight.graph[idx] {
                    InsightNode::Event { event_type, .. } => match event_type {
                        EventNodeType::Integration => "integration",
                        EventNodeType::Business => "business",
                    }
                    .to_string(),
                    _ => "integration".to_string(),
                }
            } else {
                "integration".to_string()
            };

        let params = lookup_event_params(symbols, *kind, obj_lower, event_lower);
        (event_type_str, params)
    } else {
        ("integration".to_string(), Vec::new())
    }
}

fn lookup_event_params(
    symbols: &Arc<SymbolIndex>,
    object_kind: ObjectKind,
    obj_lower: &str,
    event_name: &str,
) -> Vec<ParamInfo> {
    let event_lower = event_name.to_lowercase();
    for entry in symbols.get_by_name(obj_lower) {
        if entry.kind != object_kind {
            continue;
        }
        for method in &entry.methods {
            if method.name.to_lowercase() == event_lower {
                return map_parameters_to_param_info(&method.parameters);
            }
        }
    }
    Vec::new()
}

/// Generate a ready-to-paste [EventSubscriber] attribute.
fn format_example(object_kind: ObjectKind, object_name: &str, event_name: &str) -> String {
    let kind_str = format!("{object_kind}");
    format!(
        "[EventSubscriber(ObjectType::{kind_str}, {kind_str}::\"{object_name}\", '{event_name}', '', false, false)]"
    )
}

/// Remove duplicate integration points by (object, event) key.
fn dedup_points(points: Vec<IntegrationPoint>) -> Vec<IntegrationPoint> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    points
        .into_iter()
        .filter(|p| seen.insert((p.object.to_lowercase(), p.event.to_lowercase())))
        .collect()
}

fn apply_filters(
    points: Vec<IntegrationPoint>,
    filter_table: Option<&str>,
    filter_field: Option<&str>,
) -> Vec<IntegrationPoint> {
    let table_lower = filter_table.map(|t| t.to_lowercase());
    let field_lower = filter_field.map(|f| f.to_lowercase());

    points
        .into_iter()
        .filter(|p| {
            if let Some(ref tbl) = table_lower {
                let has_table_var = p
                    .params
                    .iter()
                    .any(|param| param.is_var && is_record_of_table(&param.type_name, tbl));
                if !has_table_var {
                    return false;
                }
            }
            if let Some(ref fld) = field_lower {
                let has_field = p.params.iter().any(|param| {
                    param.name.to_lowercase().contains(fld.as_str())
                        || param.type_name.to_lowercase().contains(fld.as_str())
                });
                if !has_field {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Check whether `type_name` is a `Record "TableName"` or `Record TableName` reference.
fn is_record_of_table(type_name: &str, table_lower: &str) -> bool {
    let tn = type_name.to_lowercase();
    let rest = if let Some(r) = tn.strip_prefix("record") {
        r.trim()
    } else {
        return false;
    };
    let clean = rest.trim_matches('"').trim_matches('\'').trim();
    clean == table_lower
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;

    fn make_codeunit(id: i32, name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Codeunit,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "TestPkg".to_string(),
            methods,
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn integration_event_with_params(name: &str, params: Vec<ParameterSymbol>) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: params,
            return_type: None,
            attributes: vec![AttributeSymbol {
                name: "IntegrationEvent".to_string(),
                arguments: vec!["false".to_string(), "false".to_string()],
            }],
            is_local: false,
        }
    }

    fn regular_method(name: &str) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        }
    }

    fn param(name: &str, type_name: &str, is_var: bool) -> ParameterSymbol {
        ParameterSymbol {
            name: name.to_string(),
            type_name: type_name.to_string(),
            is_var,
        }
    }

    fn workspace_with_event() -> Workspace {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_codeunit(
            80,
            "Sales-Post",
            vec![
                regular_method("PostDocument"),
                integration_event_with_params(
                    "OnBeforePostSalesDoc",
                    vec![
                        param("SalesHeader", "Record \"Sales Header\"", true),
                        param("IsHandled", "Boolean", true),
                    ],
                ),
            ],
        )]);
        ws
    }

    #[test]
    fn procedure_query_finds_published_events() {
        let ws = workspace_with_event();

        let _ = ws.get_or_build_call_graph();

        let query = EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            filter_table: None,
            filter_field: None,
        };

        let result = suggest_event(&ws, &query);
        assert!(
            !result.integration_points.is_empty(),
            "Should find at least one integration point"
        );
        let events: Vec<&str> = result
            .integration_points
            .iter()
            .map(|p| p.event.as_str())
            .collect();
        assert!(
            events.contains(&"OnBeforePostSalesDoc"),
            "Should find OnBeforePostSalesDoc, got: {events:?}"
        );
    }

    #[test]
    fn table_filter_narrows_results() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_codeunit(
            80,
            "Sales-Post",
            vec![
                integration_event_with_params(
                    "OnBeforePostSalesDoc",
                    vec![param("SalesHeader", "Record \"Sales Header\"", true)],
                ),
                integration_event_with_params(
                    "OnAfterPost",
                    vec![param("Result", "Boolean", false)],
                ),
            ],
        )]);

        let _ = ws.get_or_build_call_graph();

        let query = EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            filter_table: Some("Sales Header".to_string()),
            filter_field: None,
        };

        let result = suggest_event(&ws, &query);
        assert_eq!(
            result.integration_points.len(),
            1,
            "Only one event has Sales Header as var param"
        );
        assert_eq!(result.integration_points[0].event, "OnBeforePostSalesDoc");
    }

    #[test]
    fn table_query_finds_var_params() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_codeunit(
            80,
            "Sales-Post",
            vec![integration_event_with_params(
                "OnBeforePostSalesDoc",
                vec![param("SalesHeader", "Record \"Sales Header\"", true)],
            )],
        )]);

        let _ = ws.get_or_build_call_graph();

        let query = EventQuery {
            source: QuerySource::Table {
                table: "Sales Header".to_string(),
            },
            filter_table: None,
            filter_field: None,
        };

        let result = suggest_event(&ws, &query);
        assert!(
            !result.integration_points.is_empty(),
            "Should find events where Sales Header is a var param"
        );
        assert!(
            result
                .integration_points
                .iter()
                .any(|p| p.event == "OnBeforePostSalesDoc"),
            "Should include OnBeforePostSalesDoc"
        );
    }

    #[test]
    fn empty_results_for_unknown_object() {
        let ws = Workspace::new();

        let _ = ws.get_or_build_call_graph();

        let query = EventQuery {
            source: QuerySource::Procedure {
                object: "NonExistentCU".to_string(),
                procedure: None,
            },
            filter_table: None,
            filter_field: None,
        };

        let result = suggest_event(&ws, &query);
        assert!(
            result.integration_points.is_empty(),
            "Unknown object should return empty results"
        );
    }

    #[test]
    fn is_record_of_table_matches_various_formats() {
        assert!(is_record_of_table(
            "Record \"Sales Header\"",
            "sales header"
        ));
        assert!(is_record_of_table("Record Customer", "customer"));
        assert!(is_record_of_table(
            "record \"Sales Header\"",
            "sales header"
        ));
        assert!(!is_record_of_table("Record \"Sales Header\"", "sales line"));
        assert!(!is_record_of_table("Boolean", "sales header"));
        assert!(!is_record_of_table("", "sales header"));
    }

    #[test]
    fn event_query_returns_event_itself() {
        let ws = workspace_with_event();
        let _ = ws.get_or_build_call_graph();

        let query = EventQuery {
            source: QuerySource::Event {
                object: "Sales-Post".to_string(),
                event: "OnBeforePostSalesDoc".to_string(),
            },
            filter_table: None,
            filter_field: None,
        };

        let result = suggest_event(&ws, &query);
        assert!(
            !result.integration_points.is_empty(),
            "Should find the event itself"
        );
        assert_eq!(result.integration_points[0].event, "OnBeforePostSalesDoc");
    }

    #[test]
    fn format_example_produces_event_subscriber_attribute() {
        let example = format_example(ObjectKind::Codeunit, "Sales-Post", "OnAfterPost");
        assert!(
            example.contains("EventSubscriber"),
            "Should contain EventSubscriber"
        );
        assert!(example.contains("Sales-Post"), "Should contain object name");
        assert!(example.contains("OnAfterPost"), "Should contain event name");
        assert!(
            example.contains("ObjectType::Codeunit"),
            "Should contain ObjectType"
        );
    }

    #[test]
    fn dedup_removes_duplicate_events() {
        let dup = IntegrationPoint {
            event: "OnPost".to_string(),
            object: "Sales-Post".to_string(),
            event_type: "integration".to_string(),
            params: Vec::new(),
            path: Vec::new(),
            example: String::new(),
        };

        let points = vec![dup.clone(), dup];
        let deduped = dedup_points(points);
        assert_eq!(deduped.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Traversal-safety tests (white-box, exercise trace_from_node directly).
    //
    // The depth limit and `visited` set are the two safeguards that keep
    // call-graph traversal bounded on deep, cyclic, or fan-out graphs. The
    // higher-level query tests above only build shallow, acyclic graphs, so
    // these drive the recursion bounds explicitly via a hand-built graph.
    // -----------------------------------------------------------------------

    use al_insight::graph::InsightEdge;

    fn add_proc(g: &mut InsightGraph, object: &str, name: &str) -> NodeId {
        let idx = g.ensure_node(
            NodeKey::Procedure(
                ObjectKind::Codeunit,
                object.to_lowercase(),
                name.to_lowercase(),
            ),
            InsightNode::Procedure {
                object_kind: ObjectKind::Codeunit,
                object_name: object.to_string(),
                name: name.to_string(),
                is_local: false,
            },
        );
        NodeId::from(idx)
    }

    fn add_event(g: &mut InsightGraph, object: &str, name: &str) -> NodeId {
        let idx = g.ensure_node(
            NodeKey::Event(
                ObjectKind::Codeunit,
                object.to_lowercase(),
                name.to_lowercase(),
            ),
            InsightNode::Event {
                object_kind: ObjectKind::Codeunit,
                object_name: object.to_string(),
                name: name.to_string(),
                event_type: EventNodeType::Integration,
            },
        );
        NodeId::from(idx)
    }

    fn add_subscriber(
        g: &mut InsightGraph,
        object: &str,
        name: &str,
        target_object: &str,
        target_event: &str,
    ) -> NodeId {
        let idx = g.ensure_node(
            NodeKey::Subscriber(
                ObjectKind::Codeunit,
                object.to_lowercase(),
                name.to_lowercase(),
            ),
            InsightNode::Subscriber {
                object_kind: ObjectKind::Codeunit,
                object_name: object.to_string(),
                name: name.to_string(),
                target_object: target_object.to_string(),
                target_event: target_event.to_string(),
            },
        );
        NodeId::from(idx)
    }

    fn start_hop(object: &str, procedure: &str) -> TraceHop {
        TraceHop {
            object: object.to_string(),
            procedure: procedure.to_string(),
            edge_kind: "start".to_string(),
        }
    }

    #[test]
    fn trace_deep_call_chain_respects_max_depth() {
        // Build a 15-deep linear chain P0 -> P1 -> ... -> P14, where every
        // procedure also publishes an event Ek. With max_depth = 10 the trace
        // must stop before reaching the deeper events.
        let mut g = InsightGraph::new();
        let chain: Vec<NodeId> = (0..15)
            .map(|i| add_proc(&mut g, "CU", &format!("P{i}")))
            .collect();
        let events: Vec<NodeId> = (0..15)
            .map(|i| add_event(&mut g, "CU", &format!("E{i}")))
            .collect();
        let g = Arc::new(g);

        let mut cg = CallGraph::build_from_insight(&g);
        for i in 0..14 {
            cg.add_direct_call(chain[i], chain[i + 1]);
        }
        // Each Pi calls its event Ei (a DirectCall edge into the Event node).
        for i in 0..15 {
            cg.add_direct_call(chain[i], events[i]);
        }

        let symbols = SymbolIndex::new();
        let symbols = Arc::new(symbols);
        let mut points = Vec::new();
        let mut visited = HashSet::new();
        let mut partial = false;
        trace_from_node(
            chain[0],
            &g,
            &cg,
            &symbols,
            &mut points,
            &mut visited,
            &mut partial,
            vec![start_hop("CU", "P0")],
            0,
            10,
        );

        // Every reported event must lie within the depth bound. Each path's
        // length (number of hops) cannot exceed max_depth.
        assert!(!points.is_empty(), "should discover some events");
        for p in &points {
            assert!(
                p.path.len() <= 10,
                "path length {} exceeds max_depth for {}",
                p.path.len(),
                p.event
            );
        }
        let found: HashSet<&str> = points.iter().map(|p| p.event.as_str()).collect();
        assert!(found.contains("E0"), "shallow event should be found");
        assert!(
            !found.contains("E14"),
            "event beyond max_depth must not be reached"
        );
    }

    #[test]
    fn trace_circular_subscriptions_terminate() {
        // Cycle: ProcA -> EventA -(subscription)-> SubB -> EventB
        //        -(subscription)-> SubA -> EventA (back to start).
        // The visited set must break the cycle and the trace must terminate.
        let mut g = InsightGraph::new();
        let proc_a = add_proc(&mut g, "CU", "ProcA");
        let event_a = add_event(&mut g, "CU", "EventA");
        let event_b = add_event(&mut g, "CU", "EventB");
        // Subscriber nodes; SubscribesTo edges are what build_from_insight
        // turns into EventSubscription edges (followed when tracing an Event).
        let sub_b = add_subscriber(&mut g, "CU", "SubB", "CU", "EventA");
        let sub_a = add_subscriber(&mut g, "CU", "SubA", "CU", "EventB");

        let sub_b_idx = petgraph::graph::NodeIndex::new(sub_b.0);
        let sub_a_idx = petgraph::graph::NodeIndex::new(sub_a.0);
        let event_a_idx = petgraph::graph::NodeIndex::new(event_a.0);
        let event_b_idx = petgraph::graph::NodeIndex::new(event_b.0);
        g.add_edge(sub_b_idx, event_a_idx, InsightEdge::SubscribesTo);
        g.add_edge(sub_a_idx, event_b_idx, InsightEdge::SubscribesTo);
        let g = Arc::new(g);

        let mut cg = CallGraph::build_from_insight(&g);
        // ProcA publishes EventA; SubB calls EventB; SubA calls EventA (cycle).
        cg.add_direct_call(proc_a, event_a);
        cg.add_direct_call(sub_b, event_b);
        cg.add_direct_call(sub_a, event_a);

        let symbols = Arc::new(SymbolIndex::new());
        let mut points = Vec::new();
        let mut visited = HashSet::new();
        let mut partial = false;
        // Must return (not hang / overflow) despite the cycle.
        trace_from_node(
            proc_a,
            &g,
            &cg,
            &symbols,
            &mut points,
            &mut visited,
            &mut partial,
            vec![start_hop("CU", "ProcA")],
            0,
            10,
        );

        // Both events are reachable exactly once; the cycle does not blow up.
        let events: HashSet<&str> = points.iter().map(|p| p.event.as_str()).collect();
        assert!(events.contains("EventA"));
        assert!(events.contains("EventB"));
        // Each node is visited at most once, so each event appears once.
        assert_eq!(
            points.iter().filter(|p| p.event == "EventA").count(),
            1,
            "cycle should not produce duplicate EventA visits"
        );
    }

    #[test]
    fn trace_fanout_graph_bounded() {
        // A 3-wide fan-out at each of 3 levels: root calls 3 children, each
        // child calls 3 grandchildren, each grandchild publishes an event.
        // The visited set keeps shared nodes from being re-explored, so the
        // number of discovered events is bounded by the node count, not the
        // number of distinct root->leaf paths.
        let mut g = InsightGraph::new();
        let root = add_proc(&mut g, "CU", "Root");
        let children: Vec<NodeId> = (0..3)
            .map(|i| add_proc(&mut g, "CU", &format!("C{i}")))
            .collect();
        let grandkids: Vec<NodeId> = (0..9)
            .map(|i| add_proc(&mut g, "CU", &format!("G{i}")))
            .collect();
        let events: Vec<NodeId> = (0..9)
            .map(|i| add_event(&mut g, "CU", &format!("Ev{i}")))
            .collect();
        let g = Arc::new(g);

        let mut cg = CallGraph::build_from_insight(&g);
        for &c in &children {
            cg.add_direct_call(root, c);
        }
        for (ci, &c) in children.iter().enumerate() {
            for gj in 0..3 {
                let g_idx = ci * 3 + gj;
                cg.add_direct_call(c, grandkids[g_idx]);
            }
        }
        for (i, &gk) in grandkids.iter().enumerate() {
            cg.add_direct_call(gk, events[i]);
        }

        let symbols = Arc::new(SymbolIndex::new());
        let mut points = Vec::new();
        let mut visited = HashSet::new();
        let mut partial = false;
        trace_from_node(
            root,
            &g,
            &cg,
            &symbols,
            &mut points,
            &mut visited,
            &mut partial,
            vec![start_hop("CU", "Root")],
            0,
            10,
        );

        // All 9 leaf events discovered, each exactly once (no exponential blowup).
        assert_eq!(
            points.len(),
            9,
            "expected 9 distinct events, got {}",
            points.len()
        );
        let unique: HashSet<&str> = points.iter().map(|p| p.event.as_str()).collect();
        assert_eq!(unique.len(), 9, "events should be distinct");
    }
}
