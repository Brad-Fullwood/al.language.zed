//! Structured integration point discovery.
//!
//! Given an object+procedure, table, or event, traces the call/event graph
//! to find all reachable integration points (events that fire along the
//! execution path). Supports filtering results to events that expose a
//! specific table as a `var` parameter.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use al_symbols::{ObjectKind, SymbolEntry, SymbolIndex};
use serde::{Deserialize, Serialize};

use al_insight::graph::{EventNodeType, InsightGraph, InsightNode, NodeKey};
use al_insight::index::{CallGraph, EdgeResolutionState, NodeId};
use al_symbols::ParameterSymbol;
use al_workspace::Workspace;

#[derive(Debug, thiserror::Error)]
pub enum SuggestEventError {
    #[error(transparent)]
    Graph(#[from] al_workspace::CallGraphBuildError),
    #[error("invalid AL object kind '{kind}': {reason}")]
    InvalidObjectKind { kind: String, reason: String },
    #[error("AL object '{name}' was not found{kind_suffix}")]
    ObjectNotFound { name: String, kind_suffix: String },
    #[error("AL object name '{name}' is ambiguous across kinds {kinds}; supply objectKind/--kind")]
    AmbiguousObject { name: String, kinds: String },
    #[error("{target} '{name}' was not found in {kind} '{object}'")]
    TargetNotFound {
        target: &'static str,
        name: String,
        kind: ObjectKind,
        object: String,
    },
}

fn resolve_object_kind(
    workspace: &Workspace,
    object_name: &str,
    requested_kind: Option<&str>,
) -> Result<ObjectKind, SuggestEventError> {
    let requested_kind = requested_kind
        .map(|kind| {
            kind.parse::<ObjectKind>()
                .map_err(|reason| SuggestEventError::InvalidObjectKind {
                    kind: kind.to_string(),
                    reason: reason.to_string(),
                })
        })
        .transpose()?;
    let mut kinds = workspace
        .symbols
        .get_by_name(object_name)
        .into_iter()
        .map(|entry| entry.kind)
        .collect::<Vec<_>>();
    // A file can declare several objects; only those named `object_name`
    // count, not whichever object the file happens to declare first.
    for path in workspace.file_index.object_paths(object_name) {
        for info in workspace
            .file_index
            .object_infos_in(&path)
            .into_iter()
            .filter(|info| info.name.eq_ignore_ascii_case(object_name))
        {
            let kind = info.kind.parse::<ObjectKind>().map_err(|reason| {
                SuggestEventError::InvalidObjectKind {
                    kind: info.kind.clone(),
                    reason: reason.to_string(),
                }
            })?;
            kinds.push(kind);
        }
    }
    kinds.sort_unstable();
    kinds.dedup();

    if let Some(requested_kind) = requested_kind {
        return kinds
            .contains(&requested_kind)
            .then_some(requested_kind)
            .ok_or_else(|| SuggestEventError::ObjectNotFound {
                name: object_name.to_string(),
                kind_suffix: format!(" as {requested_kind}"),
            });
    }
    match kinds.as_slice() {
        [] => Err(SuggestEventError::ObjectNotFound {
            name: object_name.to_string(),
            kind_suffix: String::new(),
        }),
        [kind] => Ok(*kind),
        _ => Err(SuggestEventError::AmbiguousObject {
            name: object_name.to_string(),
            kinds: kinds
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
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
    /// Optional AL object kind used to disambiguate same-named objects.
    #[serde(default)]
    pub object_kind: Option<String>,
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
    /// True when the answer is known to leave something out: see
    /// `depth_cut` and `without_source`. The same query gives the same
    /// answer every time; nothing is still being analysed.
    pub partial: bool,
    /// A branch reached [`MAX_TRACE_DEPTH`] calls and was cut there.
    pub depth_cut: bool,
    /// The depth the trace stops at.
    pub max_depth: usize,
    /// Procedures on the trace whose calls are not known, because their
    /// source is not loaded (package code): the events they raise are not
    /// followed. The first 20, sorted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub without_source: Vec<String>,
    /// How many such procedures there were in all.
    pub without_source_count: usize,
}

/// How many procedures without source a result names.
const MAX_LISTED_WITHOUT_SOURCE: usize = 20;

/// What a trace could not follow.
#[derive(Debug, Default)]
struct TraceGaps {
    /// Nodes the depth limit stopped at that have something below them (an
    /// event, callees or subscribers) and were not traced from a shallower
    /// depth instead.
    cut: HashSet<NodeId>,
    without_source: std::collections::BTreeSet<String>,
}

impl TraceGaps {
    fn note_unresolved(&mut self, cg: &CallGraph, node_id: NodeId, object: &str, name: &str) {
        if cg.resolution_state(node_id) == EdgeResolutionState::Unresolved {
            self.without_source.insert(format!("{object}.{name}"));
        }
    }
}

fn result_from(points: Vec<IntegrationPoint>, gaps: TraceGaps) -> SuggestEventResult {
    let without_source_count = gaps.without_source.len();
    SuggestEventResult {
        integration_points: points,
        partial: !gaps.cut.is_empty() || without_source_count > 0,
        depth_cut: !gaps.cut.is_empty(),
        max_depth: MAX_TRACE_DEPTH,
        without_source: gaps
            .without_source
            .into_iter()
            .take(MAX_LISTED_WITHOUT_SOURCE)
            .collect(),
        without_source_count,
    }
}

/// How many calls deep a trace follows before it stops.
pub const MAX_TRACE_DEPTH: usize = 10;

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
    /// Ready-to-paste `[EventSubscriber]` attribute. Empty when the
    /// publisher's object kind cannot carry a subscriber.
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

pub fn suggest_event(
    workspace: &Workspace,
    query: &EventQuery,
) -> Result<SuggestEventResult, SuggestEventError> {
    match &query.source {
        QuerySource::Procedure { object, procedure } => query_procedure(
            workspace,
            object,
            procedure.as_deref(),
            query.object_kind.as_deref(),
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
            query.object_kind.as_deref(),
            query.filter_table.as_deref(),
            query.filter_field.as_deref(),
        ),
    }
}

fn query_procedure(
    workspace: &Workspace,
    object_name: &str,
    procedure_name: Option<&str>,
    requested_kind: Option<&str>,
    filter_table: Option<&str>,
    filter_field: Option<&str>,
) -> Result<SuggestEventResult, SuggestEventError> {
    let (insight, cg_guard) = workspace.get_or_build_call_graph()?;
    let cg_opt = cg_guard.as_ref();

    let object_kind = resolve_object_kind(workspace, object_name, requested_kind)?;

    let mut points: Vec<IntegrationPoint> = Vec::new();
    let mut visited: HashMap<NodeId, usize> = HashMap::new();
    let mut gaps = TraceGaps::default();

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
                trace_from_node(
                    node_id,
                    &insight,
                    cg,
                    &workspace.symbols,
                    &mut points,
                    &mut visited,
                    &mut gaps,
                    vec![hop],
                    0,
                    MAX_TRACE_DEPTH,
                );
            }
        } else {
            return Err(SuggestEventError::TargetNotFound {
                target: "procedure",
                name: proc_name.to_string(),
                kind: object_kind,
                object: object_name.to_string(),
            });
        }
    } else {
        let obj_lower = object_name.to_lowercase();
        // `insight.index` is a HashMap, so iterating its keys directly made
        // which procedures were traced first depend on key hashes — and with a
        // shared `visited` set that changed which events came back.
        for key in sorted_index_keys(&insight) {
            let proc_id = match &key {
                NodeKey::Procedure(kind, obj, _name)
                    if *kind == object_kind && obj == &obj_lower =>
                {
                    CallGraph::node_id_for(&insight, &key)
                }
                _ => None,
            };
            if let Some(node_id) = proc_id {
                if visited.contains_key(&node_id) {
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
                    trace_from_node(
                        node_id,
                        &insight,
                        cg,
                        &workspace.symbols,
                        &mut points,
                        &mut visited,
                        &mut gaps,
                        vec![hop],
                        0,
                        MAX_TRACE_DEPTH,
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
    points = apply_filters(&workspace.symbols, points, filter_table, filter_field);
    Ok(result_from(points, gaps))
}

fn query_table(
    workspace: &Workspace,
    table_name: &str,
    filter_field: Option<&str>,
) -> Result<SuggestEventResult, SuggestEventError> {
    let (insight, _cg_guard) = workspace.get_or_build_call_graph()?;

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

        let example = format_example(
            &workspace.symbols,
            obj.kind,
            &obj.name,
            &pub_event.method.name,
        );

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
    points = apply_filters(&workspace.symbols, points, None, filter_field);
    Ok(result_from(points, TraceGaps::default()))
}

fn query_event(
    workspace: &Workspace,
    object_name: &str,
    event_name: &str,
    requested_kind: Option<&str>,
    filter_table: Option<&str>,
    filter_field: Option<&str>,
) -> Result<SuggestEventResult, SuggestEventError> {
    let (insight, cg_guard) = workspace.get_or_build_call_graph()?;
    let cg_opt = cg_guard.as_ref();

    let object_kind = resolve_object_kind(workspace, object_name, requested_kind)?;

    let mut points: Vec<IntegrationPoint> = Vec::new();
    let mut visited: HashMap<NodeId, usize> = HashMap::new();
    let mut gaps = TraceGaps::default();

    let event_key = NodeKey::Event(
        object_kind,
        object_name.to_lowercase(),
        event_name.to_lowercase(),
    );

    if let Some(event_node_id) = CallGraph::node_id_for(&insight, &event_key) {
        let (event_type_str, params) =
            resolve_event_details(&insight, &workspace.symbols, &event_key);
        let example = format_example(&workspace.symbols, object_kind, object_name, event_name);

        points.push(IntegrationPoint {
            event: event_name.to_string(),
            object: object_name.to_string(),
            event_type: event_type_str,
            params,
            path: Vec::new(),
            example,
        });

        visited.insert(event_node_id, 0);

        if let Some(cg) = cg_opt {
            for sub_id in cg.subscribers_of(event_node_id) {
                if visited.contains_key(&sub_id) {
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
                    &mut gaps,
                    vec![sub_hop],
                    0,
                    MAX_TRACE_DEPTH,
                );
            }
        }
    } else {
        return Err(SuggestEventError::TargetNotFound {
            target: "event",
            name: event_name.to_string(),
            kind: object_kind,
            object: object_name.to_string(),
        });
    }

    points = dedup_points(points);
    points = apply_filters(&workspace.symbols, points, filter_table, filter_field);
    Ok(result_from(points, gaps))
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
    visited: &mut HashMap<NodeId, usize>,
    gaps: &mut TraceGaps,
    path: Vec<TraceHop>,
    depth: usize,
    max_depth: usize,
) {
    if depth >= max_depth {
        // The branch below this node is not in the result. Say so rather than
        // report a silently truncated set as complete, but only when there
        // is something below it, and not for a node traced from a shallower
        // depth (which `visited` records).
        let traced = visited.get(&node_id).is_some_and(|&seen| seen < max_depth);
        let is_event = matches!(
            insight.graph[petgraph::graph::NodeIndex::new(node_id.0)],
            InsightNode::Event { .. }
        );
        let has_more = is_event
            || !cg.callees_of(node_id).is_empty()
            || !cg.subscribers_of(node_id).is_empty();
        if !traced && has_more {
            gaps.cut.insert(node_id);
        }
        return;
    }
    // Best depth per node, not a plain visited set. A node first reached at
    // depth 9 had its own callees refused at depth 10, and a later trace that
    // reached it at depth 1 then skipped it entirely, so every event below it
    // was missing.
    match visited.get(&node_id) {
        Some(&seen) if seen <= depth => return,
        _ => visited.insert(node_id, depth),
    };
    // Reached within the limit after all: it is traced below.
    gaps.cut.remove(&node_id);

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
            let example = format_example(symbols, *object_kind, object_name, name);

            points.push(IntegrationPoint {
                event: name.clone(),
                object: object_name.clone(),
                event_type: event_type_str,
                params,
                path: path.clone(),
                example,
            });

            // No pre-check against `visited` here: the recursive call keeps
            // the best depth per node, and skipping a node already recorded at
            // a worse depth is exactly the bug that dropped events.
            for sub_id in cg.subscribers_of(node_id) {
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
                    gaps,
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
            gaps.note_unresolved(cg, node_id, object_name, name);

            for edge in cg.callees_of(node_id) {
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
                    gaps,
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
    for key in sorted_index_keys(insight) {
        if let NodeKey::Event(kind, obj, _event_lower) = &key {
            if *kind == object_kind && obj == &obj_lower {
                let (event_type_str, params) = resolve_event_details(insight, symbols, &key);
                let first_idx = match insight.index[&key].first() {
                    Some(idx) => *idx,
                    None => continue,
                };
                let event_name = match insight.graph[first_idx] {
                    InsightNode::Event { ref name, .. } => name.clone(),
                    _ => continue,
                };
                let example = format_example(symbols, object_kind, object_name, &event_name);
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

/// AL's `ObjectType::` member and object-reference scope for a publisher kind.
///
/// The two arguments of an `[EventSubscriber]` do not use the same word. A
/// table subscriber is
/// `[EventSubscriber(ObjectType::Table, Database::"Sales Header", ...)]`:
/// Microsoft's EventSubscriber page states "For a table event, specify ObjectId
/// by name with `Database::<ObjectName>`, not `Table::<ObjectName>`".
///
/// `None` for a kind that cannot publish a subscribable event: AL's `ObjectType`
/// option has only Codeunit, MenuSuite, Page, Query, Report, Table and XmlPort.
fn subscriber_scope(object_kind: ObjectKind) -> Option<(&'static str, &'static str)> {
    match object_kind {
        ObjectKind::Table | ObjectKind::TableExtension => Some(("Table", "Database")),
        ObjectKind::Page | ObjectKind::PageExtension => Some(("Page", "Page")),
        ObjectKind::Report | ObjectKind::ReportExtension => Some(("Report", "Report")),
        ObjectKind::Codeunit => Some(("Codeunit", "Codeunit")),
        ObjectKind::XmlPort => Some(("XmlPort", "Xmlport")),
        ObjectKind::Query => Some(("Query", "Query")),
        _ => None,
    }
}

/// Generate a ready-to-paste `[EventSubscriber]` attribute.
///
/// Empty when the publisher's kind carries no subscriber, and empty for an
/// extension object whose base cannot be resolved: an event declared in a
/// `tableextension` is subscribed through the table it extends, so writing the
/// extension's own name would not compile.
fn format_example(
    symbols: &SymbolIndex,
    object_kind: ObjectKind,
    object_name: &str,
    event_name: &str,
) -> String {
    let Some((object_type, scope)) = subscriber_scope(object_kind) else {
        return String::new();
    };
    let target = if matches!(
        object_kind,
        ObjectKind::TableExtension | ObjectKind::PageExtension | ObjectKind::ReportExtension
    ) {
        match extended_object_name(symbols, object_kind, object_name) {
            Some(base) => base,
            None => return String::new(),
        }
    } else {
        object_name.to_string()
    };
    format!(
        "[EventSubscriber(ObjectType::{object_type}, {scope}::\"{target}\", '{event_name}', '', false, false)]"
    )
}

/// The object an extension object extends, from the symbol index.
fn extended_object_name(
    symbols: &SymbolIndex,
    object_kind: ObjectKind,
    object_name: &str,
) -> Option<String> {
    symbols
        .get_by_name(object_name)
        .into_iter()
        .find(|entry| entry.kind == object_kind)
        .and_then(|entry| entry.extends.clone())
}

/// The insight index's keys in a stable order.
fn sorted_index_keys(insight: &InsightGraph) -> Vec<NodeKey> {
    let mut keys: Vec<NodeKey> = insight.index.keys().cloned().collect();
    keys.sort();
    keys
}

/// Order integration points and drop duplicates of the same `(object, event)`.
///
/// Sorting before the dedup decides which duplicate's `path` breadcrumb the
/// user is shown: the shortest trace wins, and ties break on the rendered
/// path, so the answer no longer depends on which duplicate the traversal
/// happened to reach first.
fn dedup_points(mut points: Vec<IntegrationPoint>) -> Vec<IntegrationPoint> {
    points.sort_by(|left, right| {
        (
            left.object.to_lowercase(),
            left.event.to_lowercase(),
            left.path.len(),
        )
            .cmp(&(
                right.object.to_lowercase(),
                right.event.to_lowercase(),
                right.path.len(),
            ))
            .then_with(|| path_key(&left.path).cmp(&path_key(&right.path)))
    });
    let mut seen: HashSet<(String, String)> = HashSet::new();
    points
        .into_iter()
        .filter(|p| seen.insert((p.object.to_lowercase(), p.event.to_lowercase())))
        .collect()
}

fn path_key(path: &[TraceHop]) -> Vec<(String, String, String)> {
    path.iter()
        .map(|hop| {
            (
                hop.object.to_lowercase(),
                hop.procedure.to_lowercase(),
                hop.edge_kind.clone(),
            )
        })
        .collect()
}

/// Keep only the points matching the caller's table and field filters.
///
/// `filter_field` names a *table field*, as the MCP tool and `al-explorer
/// --field` both document. It used to be matched against the parameter's own
/// name and type text, with no `is_var` check and no notion of a field at all,
/// so `--field Amount` against
/// `OnAfterPostSalesDoc(var SalesHeader: Record "Sales Header")` returned
/// nothing although the table has an `Amount` field, while `--field Record`
/// returned every event with a record parameter. The field is now resolved
/// through the symbol index, against the base table and any tableextension of
/// it.
fn apply_filters(
    symbols: &SymbolIndex,
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
                let has_field = p
                    .params
                    .iter()
                    .any(|param| param_exposes_field(symbols, param, fld));
                if !has_field {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// True when `param` is a `var Record "T"` whose table declares a field named
/// `field_lower`.
fn param_exposes_field(symbols: &SymbolIndex, param: &ParamInfo, field_lower: &str) -> bool {
    if !param.is_var {
        return false;
    }
    let Some(table) = record_table_name(&param.type_name) else {
        return false;
    };
    let declares_field = |entry: &SymbolEntry| {
        entry
            .fields
            .iter()
            .any(|field| field.name.to_lowercase() == field_lower)
    };
    let base_has_field = symbols
        .get_by_name(&table)
        .into_iter()
        .any(|entry| entry.kind == ObjectKind::Table && declares_field(&entry));
    if base_has_field {
        return true;
    }
    // A tableextension's fields belong to the table it extends.
    symbols
        .get_by_kind(ObjectKind::TableExtension)
        .into_iter()
        .any(|entry| {
            entry
                .extends
                .as_deref()
                .is_some_and(|extends| extends.to_lowercase() == table)
                && declares_field(&entry)
        })
}

/// The table named by a `Record "T"` / `Record T` type reference, lowercased.
fn record_table_name(type_name: &str) -> Option<String> {
    let rest = type_name.to_lowercase();
    let rest = rest.strip_prefix("record")?.trim();
    let clean = rest.trim_matches('"').trim_matches('\'').trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
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

    /// "Some call paths are still being analyzed" printed on every run: the
    /// flag meant a depth cut or package code without source, and neither
    /// changes on a retry. The result now says which.
    #[test]
    fn the_result_says_why_it_is_partial() {
        let complete = result_from(Vec::new(), TraceGaps::default());
        assert!(!complete.partial);
        assert_eq!(complete.without_source_count, 0);

        let mut gaps = TraceGaps::default();
        gaps.without_source
            .extend((0..25).map(|index| format!("Sales-Post.P{index:02}")));
        let result = result_from(Vec::new(), gaps);
        assert!(result.partial);
        assert!(!result.depth_cut);
        assert_eq!(result.without_source_count, 25);
        assert_eq!(result.without_source.len(), MAX_LISTED_WITHOUT_SOURCE);
        assert_eq!(result.without_source[0], "Sales-Post.P00");

        let cut = result_from(
            Vec::new(),
            TraceGaps {
                cut: [NodeId(0)].into_iter().collect(),
                ..TraceGaps::default()
            },
        );
        assert!(cut.partial && cut.depth_cut);
        let json = serde_json::to_value(&cut).unwrap();
        assert_eq!(json["maxDepth"], MAX_TRACE_DEPTH);
        assert!(json.get("withoutSource").is_none());
    }

    /// A codeunit declared after a table in the same file took the table's
    /// kind, because only the file's first object was read.
    #[test]
    fn the_kind_of_a_second_object_in_a_file_is_its_own() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Posting.al"),
            "table 50200 \"Posting Buffer\" { }\n\ncodeunit 50100 \"Posting Mgt\"\n{\n    procedure Post()\n    begin\n    end;\n}\n"
                .to_string(),
        );
        assert_eq!(
            resolve_object_kind(&ws, "Posting Mgt", None).unwrap(),
            ObjectKind::Codeunit
        );
        assert!(
            matches!(
                resolve_object_kind(&ws, "Posting Mgt", Some("table")),
                Err(SuggestEventError::ObjectNotFound { .. })
            ),
            "no table is named Posting Mgt"
        );
    }
    use al_symbols::*;

    fn make_codeunit(id: i32, name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id,
            name: name.to_string(),
            package: "TestPkg".to_string(),
            methods,
            ..Default::default()
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

        let _ = ws.get_or_build_call_graph().unwrap();

        let query = EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            object_kind: None,
            filter_table: None,
            filter_field: None,
        };

        let result = suggest_event(&ws, &query).unwrap();
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

        let _ = ws.get_or_build_call_graph().unwrap();

        let query = EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            object_kind: None,
            filter_table: Some("Sales Header".to_string()),
            filter_field: None,
        };

        let result = suggest_event(&ws, &query).unwrap();
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

        let _ = ws.get_or_build_call_graph().unwrap();

        let query = EventQuery {
            source: QuerySource::Table {
                table: "Sales Header".to_string(),
            },
            object_kind: None,
            filter_table: None,
            filter_field: None,
        };

        let result = suggest_event(&ws, &query).unwrap();
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
    fn unknown_object_is_an_explicit_query_error() {
        let ws = Workspace::new();

        let _ = ws.get_or_build_call_graph().unwrap();

        let query = EventQuery {
            source: QuerySource::Procedure {
                object: "NonExistentCU".to_string(),
                procedure: None,
            },
            object_kind: None,
            filter_table: None,
            filter_field: None,
        };

        assert!(matches!(
            suggest_event(&ws, &query),
            Err(SuggestEventError::ObjectNotFound { .. })
        ));
    }

    #[test]
    fn ambiguous_object_requires_kind_and_kind_resolves_it() {
        let ws = workspace_with_event();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: ObjectKind::Table,
            id: 50_100,
            name: "Sales-Post".to_string(),
            package: "Test".to_string(),
            ..Default::default()
        }]);

        let mut query = EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            object_kind: None,
            filter_table: None,
            filter_field: None,
        };
        assert!(matches!(
            suggest_event(&ws, &query),
            Err(SuggestEventError::AmbiguousObject { .. })
        ));

        query.object_kind = Some("codeunit".to_string());
        let result = suggest_event(&ws, &query).expect("kind disambiguates the object");
        assert!(result
            .integration_points
            .iter()
            .any(|point| point.event == "OnBeforePostSalesDoc"));
    }

    #[test]
    fn missing_procedure_and_event_are_explicit_query_errors() {
        let ws = workspace_with_event();
        for source in [
            QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: Some("DoesNotExist".to_string()),
            },
            QuerySource::Event {
                object: "Sales-Post".to_string(),
                event: "DoesNotExist".to_string(),
            },
        ] {
            let query = EventQuery {
                source,
                object_kind: Some("codeunit".to_string()),
                filter_table: None,
                filter_field: None,
            };
            assert!(matches!(
                suggest_event(&ws, &query),
                Err(SuggestEventError::TargetNotFound { .. })
            ));
        }
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
        let _ = ws.get_or_build_call_graph().unwrap();

        let query = EventQuery {
            source: QuerySource::Event {
                object: "Sales-Post".to_string(),
                event: "OnBeforePostSalesDoc".to_string(),
            },
            object_kind: None,
            filter_table: None,
            filter_field: None,
        };

        let result = suggest_event(&ws, &query).unwrap();
        assert!(
            !result.integration_points.is_empty(),
            "Should find the event itself"
        );
        assert_eq!(result.integration_points[0].event, "OnBeforePostSalesDoc");
    }

    /// The attribute must parse as AL and spell the object reference the way
    /// AL requires — `Database::` for a table, not `Table::`.
    fn assert_example_parses(example: &str, expected: &str) {
        assert_eq!(example, expected);
        let source = format!(
            "codeunit 50100 \"Sub\"\n{{\n    {example}\n    local procedure Handle()\n    begin\n    end;\n}}"
        );
        let parsed = al_syntax::AlParser::parse_quick(&source);
        assert!(
            !parsed.tree.root_node().has_error(),
            "emitted attribute does not parse:\n{source}"
        );
        let mut attributes = Vec::new();
        al_syntax::walk_tree(parsed.tree.root_node(), &mut |node| {
            if node.kind() == "attribute" {
                attributes.push(node.utf8_text(source.as_bytes()).unwrap().to_string());
            }
        });
        assert_eq!(
            attributes,
            vec![example.to_string()],
            "the attribute must survive a parse round trip"
        );
    }

    #[test]
    fn format_example_names_a_table_with_the_database_scope() {
        let symbols = SymbolIndex::new();
        assert_example_parses(
            &format_example(&symbols, ObjectKind::Table, "Sales Header", "OnAfterInsertEvent"),
            "[EventSubscriber(ObjectType::Table, Database::\"Sales Header\", 'OnAfterInsertEvent', '', false, false)]",
        );
    }

    #[test]
    fn format_example_covers_every_subscribable_kind() {
        let symbols = SymbolIndex::new();
        let cases = [
            (
                ObjectKind::Codeunit,
                "ObjectType::Codeunit, Codeunit::\"X\"",
            ),
            (ObjectKind::Page, "ObjectType::Page, Page::\"X\""),
            (ObjectKind::Report, "ObjectType::Report, Report::\"X\""),
            (ObjectKind::XmlPort, "ObjectType::XmlPort, Xmlport::\"X\""),
            (ObjectKind::Query, "ObjectType::Query, Query::\"X\""),
        ];
        for (kind, expected) in cases {
            assert_example_parses(
                &format_example(&symbols, kind, "X", "OnEvent"),
                &format!("[EventSubscriber({expected}, 'OnEvent', '', false, false)]"),
            );
        }
    }

    #[test]
    fn format_example_subscribes_to_an_extension_through_its_base_object() {
        let symbols = SymbolIndex::new();
        symbols.add_entries_owned(vec![SymbolEntry {
            kind: ObjectKind::TableExtension,
            id: 50100,
            name: "Cust Ext".to_string(),
            extends: Some("Customer".to_string()),
            ..Default::default()
        }]);
        assert_example_parses(
            &format_example(&symbols, ObjectKind::TableExtension, "Cust Ext", "OnMyEvent"),
            "[EventSubscriber(ObjectType::Table, Database::\"Customer\", 'OnMyEvent', '', false, false)]",
        );
    }

    #[test]
    fn format_example_is_empty_for_a_kind_that_cannot_publish_a_subscriber() {
        let symbols = SymbolIndex::new();
        assert_eq!(
            format_example(&symbols, ObjectKind::Interface, "IFoo", "OnEvent"),
            "",
            "AL's ObjectType option has no Interface member"
        );
        assert_eq!(
            format_example(
                &symbols,
                ObjectKind::TableExtension,
                "Unknown Ext",
                "OnEvent"
            ),
            "",
            "without the base table there is no name to write"
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

    fn point(object: &str, event: &str, path_len: usize) -> IntegrationPoint {
        IntegrationPoint {
            event: event.to_string(),
            object: object.to_string(),
            event_type: "integration".to_string(),
            params: Vec::new(),
            path: (0..path_len)
                .map(|index| TraceHop {
                    object: format!("Hop{index}"),
                    procedure: "P".to_string(),
                    edge_kind: "direct_call".to_string(),
                })
                .collect(),
            example: String::new(),
        }
    }

    /// `points` was never sorted, so the JSON array's order changed run to run
    /// and `dedup_points` kept whichever duplicate arrived first, which decided
    /// the `path` breadcrumb the user was shown.
    #[test]
    fn dedup_orders_points_and_keeps_the_shortest_path() {
        let deduped = dedup_points(vec![
            point("Sales-Post", "OnPost", 4),
            point("Purch-Post", "OnPost", 1),
            point("Sales-Post", "OnAfterPost", 2),
            point("Sales-Post", "OnPost", 2),
        ]);

        assert_eq!(
            deduped
                .iter()
                .map(|p| (p.object.as_str(), p.event.as_str(), p.path.len()))
                .collect::<Vec<_>>(),
            vec![
                ("Purch-Post", "OnPost", 1),
                ("Sales-Post", "OnAfterPost", 2),
                ("Sales-Post", "OnPost", 2),
            ]
        );
    }

    /// The filter used to match the parameter's *name* and *type text*, so
    /// `--field Amount` against a `Sales Header` parameter returned nothing
    /// while `--field Record` returned every event with a record parameter.
    #[test]
    fn filter_field_resolves_the_table_s_fields() {
        let symbols = SymbolIndex::new();
        symbols.add_entries_owned(vec![
            SymbolEntry {
                kind: ObjectKind::Table,
                id: 36,
                name: "Sales Header".to_string(),
                fields: vec![al_symbols::FieldSymbol {
                    id: 60,
                    name: "Amount".to_string(),
                    type_name: "Decimal".to_string(),
                    properties: Vec::new(),
                }],
                ..Default::default()
            },
            SymbolEntry {
                kind: ObjectKind::TableExtension,
                id: 50100,
                name: "Sales Header Ext".to_string(),
                extends: Some("Sales Header".to_string()),
                fields: vec![al_symbols::FieldSymbol {
                    id: 50100,
                    name: "Ship Reference".to_string(),
                    type_name: "Code[20]".to_string(),
                    properties: Vec::new(),
                }],
                ..Default::default()
            },
        ]);

        let mut event = point("Sales-Post", "OnAfterPostSalesDoc", 0);
        event.params = vec![ParamInfo {
            name: "SalesHeader".to_string(),
            type_name: "Record \"Sales Header\"".to_string(),
            is_var: true,
        }];

        let keeps = |field: &str| {
            !apply_filters(&symbols, vec![event.clone()], None, Some(field)).is_empty()
        };
        assert!(keeps("Amount"), "the table declares an Amount field");
        assert!(keeps("amount"), "field names are case-insensitive");
        assert!(
            keeps("Ship Reference"),
            "a tableextension's fields belong to the table it extends"
        );
        assert!(!keeps("Record"), "the type text is not a field");
        assert!(!keeps("SalesHeader"), "the parameter name is not a field");
        assert!(!keeps("e"), "a substring of a field name is not a field");
        assert!(!keeps("Quantity"), "the table has no Quantity field");
    }

    #[test]
    fn filter_field_ignores_a_non_var_parameter() {
        let symbols = SymbolIndex::new();
        symbols.add_entries_owned(vec![SymbolEntry {
            kind: ObjectKind::Table,
            id: 36,
            name: "Sales Header".to_string(),
            fields: vec![al_symbols::FieldSymbol {
                id: 60,
                name: "Amount".to_string(),
                type_name: "Decimal".to_string(),
                properties: Vec::new(),
            }],
            ..Default::default()
        }]);

        let mut event = point("Sales-Post", "OnAfterPostSalesDoc", 0);
        event.params = vec![ParamInfo {
            name: "SalesHeader".to_string(),
            type_name: "Record \"Sales Header\"".to_string(),
            is_var: false,
        }];

        assert!(
            apply_filters(&symbols, vec![event], None, Some("Amount")).is_empty(),
            "the documented contract is a `var` parameter"
        );
    }

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
                target_kind: None,
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

    /// A chain that ends exactly at the limit leaves nothing out; one that
    /// goes on does. `depthCut` was set for both.
    #[test]
    fn a_leaf_at_the_depth_limit_is_not_a_cut() {
        let run = |length: usize| {
            let mut g = InsightGraph::new();
            let chain: Vec<NodeId> = (0..length)
                .map(|i| add_proc(&mut g, "CU", &format!("P{i}")))
                .collect();
            let g = Arc::new(g);
            let mut cg = CallGraph::build_from_insight(&g);
            for pair in chain.windows(2) {
                cg.add_direct_call(pair[0], pair[1]);
            }
            let symbols = Arc::new(SymbolIndex::new());
            let mut gaps = TraceGaps::default();
            trace_from_node(
                chain[0],
                &g,
                &cg,
                &symbols,
                &mut Vec::new(),
                &mut HashMap::new(),
                &mut gaps,
                vec![start_hop("CU", "P0")],
                0,
                3,
            );
            !gaps.cut.is_empty()
        };
        assert!(!run(4), "P3 at depth 3 is a leaf");
        assert!(run(5), "P3 at depth 3 calls P4");
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
        let mut visited = HashMap::new();
        let mut gaps = TraceGaps::default();
        trace_from_node(
            chain[0],
            &g,
            &cg,
            &symbols,
            &mut points,
            &mut visited,
            &mut gaps,
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
        let mut visited = HashMap::new();
        let mut gaps = TraceGaps::default();
        // Must return (not hang / overflow) despite the cycle.
        trace_from_node(
            proc_a,
            &g,
            &cg,
            &symbols,
            &mut points,
            &mut visited,
            &mut gaps,
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
        let mut visited = HashMap::new();
        let mut gaps = TraceGaps::default();
        trace_from_node(
            root,
            &g,
            &cg,
            &symbols,
            &mut points,
            &mut visited,
            &mut gaps,
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
