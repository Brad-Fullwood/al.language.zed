//! Graph search algorithms for the insight engine.
//!
//! Provides traversal queries: event chain tracing, entry point finding,
//! and graph export (DOT, JSON).
//!
//! ## Event chain tracing
//!
//! Two APIs are available:
//!
//! - [`trace_event`] — simple flattened list, uses only the insight graph.
//! - [`trace_event_chain`] — full tree, uses the [`CallGraph`] for richer
//!   traversal through direct/trigger calls as well as event subscriptions.
//!   Cycle detection prevents infinite loops.

use std::collections::HashSet;
use std::fmt::Write as _;

use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::Serialize;

use super::graph::{InsightEdge, InsightGraph, InsightNode, NodeKey};
use super::index::{CallEdge, CallGraph, EdgeKind, NodeId};
use super::node_kind;

/// Upper bound on total nodes visited by `trace_event_chain` across all
/// branches of the recursion. The existing `max_depth` cap protects
/// against deep chains, but a wide-but-shallow event tree (one publisher
/// with thousands of subscribers, each fanning out further) can clone
/// thousands of `ChainNode` strings before exhausting depth.
///
/// 10_000 nodes × ~100-byte chain entries = ~1 MB worst-case response —
/// the bound is per-query so well-behaved BC workspaces (typically <1k
/// nodes per trace) never hit it.
const MAX_CHAIN_NODES: usize = 10_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceStep {
    pub depth: usize,
    pub edge_type: String,
    pub node_type: String,
    pub name: String,
    pub object: String,
}

/// Flattened event trace using only the insight graph.
///
/// Without a call graph the analysis cannot know which events a subscriber's
/// *body* raises, so no `publishes` hops are emitted. Pass a call graph to
/// [`trace_event_with_calls`] for the full chain.
pub fn trace_event(graph: &InsightGraph, event_name: &str, max_depth: usize) -> Vec<TraceStep> {
    trace_event_with_calls(graph, None, event_name, max_depth)
}

/// Flattened event trace, optionally following the events a subscriber's body
/// actually raises (via `call_graph`).
///
/// The previous implementation pushed *every* event published by a subscriber's
/// **object** as a depth+1 "publishes" step, without checking that the
/// subscriber raises it — fabricating chain hops for any publisher object that
/// happened to also contain subscribers. It also shared one `visited` set
/// across all same-named root events, which truncated every root's chain after
/// the first.
pub fn trace_event_with_calls(
    graph: &InsightGraph,
    call_graph: Option<&CallGraph>,
    event_name: &str,
    max_depth: usize,
) -> Vec<TraceStep> {
    let event_lower = event_name.to_lowercase();
    let mut steps = Vec::new();

    // Find all Event nodes matching the name. Sort by NodeIndex so the
    // emitted trace order is deterministic across runs — `graph.index`
    // is a HashMap and would otherwise yield arbitrary order, making
    // export comparisons (DOT / JSON) flaky for downstream tooling.
    let mut matches: Vec<petgraph::graph::NodeIndex> = graph
        .index
        .iter()
        .filter_map(|(key, indices)| match key {
            NodeKey::Event(_, _, name) if name == &event_lower => Some(indices.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect();
    matches.sort_by_key(|idx| idx.index());

    for idx in matches {
        let node = &graph.graph[idx];
        let (obj_name, event_label) = match node {
            InsightNode::Event {
                object_name, name, ..
            } => (object_name.clone(), name.clone()),
            _ => continue,
        };

        steps.push(TraceStep {
            depth: 0,
            edge_type: "origin".to_string(),
            node_type: node_kind::EVENT.to_string(),
            name: event_label,
            object: obj_name,
        });

        // A fresh visited set per root: two objects publishing a same-named
        // event are independent chains, and a shared set silently dropped all
        // but the first.
        let mut visited = HashSet::new();
        trace_from_node(
            graph,
            call_graph,
            idx,
            1,
            max_depth,
            &mut visited,
            &mut steps,
        );
    }

    steps
}

/// Events that `subscriber` actually raises, according to the call graph.
///
/// Only outgoing call edges that land on an `Event` node count; an event merely
/// declared by the same object is not a hop.
fn events_raised_by(
    graph: &InsightGraph,
    call_graph: Option<&CallGraph>,
    subscriber: petgraph::graph::NodeIndex,
) -> Vec<petgraph::graph::NodeIndex> {
    let Some(call_graph) = call_graph else {
        return Vec::new();
    };
    let mut raised: Vec<petgraph::graph::NodeIndex> = call_graph
        .callees_of(NodeId::from(subscriber))
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                EdgeKind::DirectCall | EdgeKind::IndirectCall | EdgeKind::TriggerInvocation
            )
        })
        .map(|edge| petgraph::graph::NodeIndex::new(edge.to.0))
        .filter(|&idx| {
            graph
                .graph
                .node_weight(idx)
                .is_some_and(|node| matches!(node, InsightNode::Event { .. }))
        })
        .collect();
    raised.sort_by_key(|idx| idx.index());
    raised.dedup();
    raised
}

fn trace_from_node(
    graph: &InsightGraph,
    call_graph: Option<&CallGraph>,
    node_idx: petgraph::graph::NodeIndex,
    depth: usize,
    max_depth: usize,
    visited: &mut std::collections::HashSet<petgraph::graph::NodeIndex>,
    steps: &mut Vec<TraceStep>,
) {
    if depth > max_depth || visited.contains(&node_idx) {
        return;
    }
    visited.insert(node_idx);

    // `edges_directed` yields most-recently-added first; sort so the emitted
    // order is stable regardless of graph build order.
    let mut subscribers: Vec<petgraph::graph::NodeIndex> = graph
        .graph
        .edges_directed(node_idx, Direction::Incoming)
        .filter(|edge_ref| *edge_ref.weight() == InsightEdge::SubscribesTo)
        .map(|edge_ref| edge_ref.source())
        .collect();
    subscribers.sort_by_key(|idx| idx.index());

    for sub_idx in subscribers {
        let sub_node = &graph.graph[sub_idx];

        if let InsightNode::Subscriber {
            object_name, name, ..
        } = sub_node
        {
            steps.push(TraceStep {
                depth,
                edge_type: "subscribes_to".to_string(),
                node_type: node_kind::SUBSCRIBER.to_string(),
                name: name.clone(),
                object: object_name.clone(),
            });

            for evt_idx in events_raised_by(graph, call_graph, sub_idx) {
                let InsightNode::Event {
                    name: ename,
                    object_name: eobject,
                    ..
                } = &graph.graph[evt_idx]
                else {
                    continue;
                };
                steps.push(TraceStep {
                    depth: depth + 1,
                    edge_type: "publishes".to_string(),
                    node_type: node_kind::EVENT.to_string(),
                    name: ename.clone(),
                    object: eobject.clone(),
                });
                trace_from_node(
                    graph,
                    call_graph,
                    evt_idx,
                    depth + 2,
                    max_depth,
                    visited,
                    steps,
                );
            }
        }
    }
}

// Full event chain tracing via CallGraph

/// A node in the event chain tree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainNode {
    pub edge_kind: String,
    pub node_type: String,
    pub name: String,
    pub object: String,
    pub depth: usize,
    /// `true` when this node was already expanded elsewhere in the traversal,
    /// so its children are omitted. This covers both genuine back-edges and
    /// diamond fan-ins (a node reached again through a sibling branch); see
    /// [`trace_event_chain`] — the two are **not** distinguished.
    pub cycle: bool,
    pub children: Vec<ChainNode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventChain {
    pub event_name: String,
    /// Empty when unresolved.
    pub publisher_object: String,
    pub chains: Vec<ChainNode>,
    pub nodes_visited: usize,
}

/// Trace the full event propagation chain starting from a named event.
///
/// Starting from all event nodes whose name matches `event_name`
/// (case-insensitive), the algorithm:
///
/// 1. Finds every subscriber via `CallGraph::subscribers_of`.
/// 2. For each subscriber, finds all callees (via `callees_of`):
///    - If the callee is an *event* node, recurses into it.
///    - If the callee is a *procedure* node, follows its outgoing calls one
///      level deeper to detect further event publications.
/// 3. Tracks visited `NodeId`s so the traversal terminates.
///
/// The result is a tree (`EventChain`) that shows every reachable node once.
///
/// **`cycle` means "already expanded", not "back-edge".** The `visited` set is
/// global to the traversal, so a node reached a second time through a *sibling*
/// branch — a diamond fan-in, where two subscribers both call the same
/// procedure — is emitted with `cycle: true` and no children, exactly like a
/// genuine back-edge. Both shapes are therefore reported identically: the flag
/// tells you the sub-tree is repeated elsewhere in the response, and does not
/// distinguish a real recursion from a re-convergence.
///
/// # Performance
/// Single-pass BFS/DFS over the call graph.  For a typical BC app the graph
/// has O(10k) nodes and O(30k) edges; the traversal is sub-millisecond.
pub fn trace_event_chain(
    insight: &InsightGraph,
    call_graph: &CallGraph,
    event_name: &str,
    max_depth: usize,
) -> EventChain {
    let event_lower = event_name.to_lowercase();

    // Find all event nodes matching the name. `insight.index` is a HashMap so
    // the natural iteration order is non-deterministic across rebuilds. Sort
    // by NodeIndex to make the emitted chain order stable for downstream
    // diffing — `trace_event` has the same fix at search.rs:69 (with a
    // dedicated regression test), this path mirrors it.
    let mut roots: Vec<(NodeId, String)> = Vec::new();
    for (key, indices) in &insight.index {
        if let NodeKey::Event(_, _, ref name) = key {
            if name == &event_lower {
                for &idx in indices {
                    let publisher_obj = match &insight.graph[idx] {
                        InsightNode::Event { object_name, .. } => object_name.clone(),
                        _ => String::new(),
                    };
                    roots.push((NodeId::from(idx), publisher_obj));
                }
            }
        }
    }
    roots.sort_by_key(|(id, _)| id.0);

    let publisher_object = roots
        .first()
        .map(|(_, obj)| obj.clone())
        .unwrap_or_default();

    let mut global_visited: HashSet<NodeId> = HashSet::new();
    let mut total_nodes = 0usize;

    let chains: Vec<ChainNode> = roots
        .into_iter()
        .map(|(event_id, pub_obj)| {
            // Mark the root event as visited before recursing so that
            // subscribers whose outgoing EventSubscription edge points back
            // to this root event are detected as a cycle immediately rather
            // than consuming extra depth levels on each re-entry.
            global_visited.insert(event_id);
            let root_info = call_graph.node_info(event_id);
            let chain = ChainNode {
                edge_kind: "origin".to_string(),
                node_type: node_kind::EVENT.to_string(),
                name: root_info
                    .map(|i| i.name.as_str())
                    .unwrap_or(event_name)
                    .to_string(),
                object: pub_obj,
                depth: 0,
                cycle: false,
                children: recurse_event(
                    insight,
                    call_graph,
                    event_id,
                    1,
                    max_depth,
                    &mut global_visited,
                    &mut total_nodes,
                ),
            };
            total_nodes += 1;
            chain
        })
        .collect();

    EventChain {
        event_name: event_name.to_string(),
        publisher_object,
        chains,
        nodes_visited: total_nodes,
    }
}

fn recurse_event(
    insight: &InsightGraph,
    cg: &CallGraph,
    event_id: NodeId,
    depth: usize,
    max_depth: usize,
    visited: &mut HashSet<NodeId>,
    total: &mut usize,
) -> Vec<ChainNode> {
    if depth > max_depth || *total >= MAX_CHAIN_NODES {
        return vec![];
    }

    // `subscribers_of` returns NodeIds in CallGraph build order, which is
    // derived from a DashMap iteration in SymbolIndex — non-deterministic
    // across process restarts. Sort here so chain children render in a
    // stable order independent of the underlying maps' hashing seed.
    let mut subscribers = cg.subscribers_of(event_id);
    subscribers.sort_by_key(|id| id.0);
    let mut children = Vec::new();

    for sub_id in subscribers {
        let is_cycle = visited.contains(&sub_id);
        let info = cg.node_info(sub_id);
        let sub_children = if is_cycle || depth >= max_depth {
            vec![]
        } else {
            visited.insert(sub_id);
            *total += 1;
            recurse_subscriber(insight, cg, sub_id, depth + 1, max_depth, visited, total)
        };

        children.push(ChainNode {
            edge_kind: EdgeKind::EventSubscription.to_string(),
            node_type: node_kind::SUBSCRIBER.to_string(),
            name: info.map(|i| i.name.clone()).unwrap_or_default(),
            object: info.map(|i| i.object.clone()).unwrap_or_default(),
            depth,
            cycle: is_cycle,
            children: sub_children,
        });
    }

    children
}

fn recurse_subscriber(
    insight: &InsightGraph,
    cg: &CallGraph,
    sub_id: NodeId,
    depth: usize,
    max_depth: usize,
    visited: &mut HashSet<NodeId>,
    total: &mut usize,
) -> Vec<ChainNode> {
    if depth > max_depth || *total >= MAX_CHAIN_NODES {
        return vec![];
    }

    // Sort callees by target NodeId so chain children render in a stable
    // order — same rationale as the `subscribers_of` sort in `recurse_event`.
    // Note: callees_of returns `&[CallEdge]` which we don't own; clone into a
    // Vec so we can sort. Per-call allocation is O(degree); fine for the
    // bounded-by-MAX_CHAIN_NODES traversal.
    let mut callees: Vec<CallEdge> = cg.callees_of(sub_id).to_vec();
    // Total-order on (target NodeId, kind discriminant) so duplicate
    // targets with different edge kinds are also stably ordered.
    fn kind_rank(k: &EdgeKind) -> u8 {
        match k {
            EdgeKind::DirectCall => 0,
            EdgeKind::TriggerInvocation => 1,
            EdgeKind::RecordTrigger => 2,
            EdgeKind::EventSubscription => 3,
            EdgeKind::IndirectCall => 4,
        }
    }
    callees.sort_by(|a, b| {
        a.to.0
            .cmp(&b.to.0)
            .then_with(|| kind_rank(&a.kind).cmp(&kind_rank(&b.kind)))
    });
    let mut children = Vec::new();

    for edge in callees {
        let callee_id = edge.to;
        let info = cg.node_info(callee_id);
        let node_type = info.map(|i| i.node_type.as_str()).unwrap_or("unknown");

        match node_type {
            t if t == node_kind::EVENT => {
                let is_cycle = visited.contains(&callee_id);
                let grandchildren = if is_cycle || depth >= max_depth {
                    vec![]
                } else {
                    visited.insert(callee_id);
                    *total += 1;
                    recurse_event(insight, cg, callee_id, depth + 1, max_depth, visited, total)
                };
                children.push(ChainNode {
                    edge_kind: edge.kind.to_string(),
                    node_type: node_kind::EVENT.to_string(),
                    name: info.map(|i| i.name.clone()).unwrap_or_default(),
                    object: info.map(|i| i.object.clone()).unwrap_or_default(),
                    depth,
                    cycle: is_cycle,
                    children: grandchildren,
                });
            }
            t if t == node_kind::PROCEDURE => {
                let is_cycle = visited.contains(&callee_id);
                let grandchildren = if is_cycle || depth >= max_depth {
                    vec![]
                } else {
                    visited.insert(callee_id);
                    *total += 1;
                    recurse_subscriber(insight, cg, callee_id, depth + 1, max_depth, visited, total)
                };
                if !grandchildren.is_empty() {
                    children.push(ChainNode {
                        edge_kind: edge.kind.to_string(),
                        node_type: node_kind::PROCEDURE.to_string(),
                        name: info.map(|i| i.name.clone()).unwrap_or_default(),
                        object: info.map(|i| i.object.clone()).unwrap_or_default(),
                        depth,
                        cycle: is_cycle,
                        children: grandchildren,
                    });
                }
            }
            _ => {}
        }
    }

    children
}

/// Find entry points using only the insight graph.
///
/// **Call edges do not live in the insight graph** — production code only ever
/// records them in the separate [`CallGraph`] — so this variant can only see
/// `SubscribesTo`/`Publishes`/`Triggers` relationships and consequently reports
/// nearly every procedure. Prefer [`find_entry_points_with_calls`], which is
/// what the `entrypoints` command uses.
pub fn find_entry_points(graph: &InsightGraph) -> Vec<&InsightNode> {
    find_entry_points_with_calls(graph, None)
}

/// Find entry points: procedures that nothing else calls, subscribes through,
/// or triggers.
///
/// Incoming `Contains` edges (an object owning its procedure) are structural,
/// not invocations, so they are ignored. When `call_graph` is supplied — which
/// is where direct/indirect/trigger call edges actually live — a procedure with
/// any incoming call edge is excluded. Without it the result degenerates to
/// "every procedure", which is why the daemon passes its call graph.
pub fn find_entry_points_with_calls<'g>(
    graph: &'g InsightGraph,
    call_graph: Option<&CallGraph>,
) -> Vec<&'g InsightNode> {
    graph
        .graph
        .node_indices()
        .filter(|&idx| {
            let node = &graph.graph[idx];
            if !matches!(node, InsightNode::Procedure { .. }) {
                return false;
            }
            let insight_clean = graph
                .graph
                .edges_directed(idx, Direction::Incoming)
                .all(|e| *e.weight() == InsightEdge::Contains);
            if !insight_clean {
                return false;
            }
            match call_graph {
                Some(call_graph) => call_graph.callers_of(NodeId::from(idx)).is_empty(),
                None => true,
            }
        })
        .map(|idx| &graph.graph[idx])
        .collect()
}

pub fn export_dot(graph: &InsightGraph) -> String {
    let mut dot = String::from("digraph insight {\n");
    dot.push_str("    rankdir=LR;\n");
    dot.push_str("    node [shape=box];\n\n");

    for idx in graph.graph.node_indices() {
        let node = &graph.graph[idx];
        let (label, shape) = match node {
            InsightNode::Object { kind, name, .. } => (format!("{kind}\\n{name}"), "box"),
            InsightNode::Procedure {
                object_name, name, ..
            } => (format!("{object_name}.{name}"), "ellipse"),
            InsightNode::Event {
                object_name, name, ..
            } => (format!("{object_name}::{name}"), "diamond"),
            InsightNode::Subscriber {
                object_name, name, ..
            } => (format!("{object_name}::{name}"), "hexagon"),
        };
        // Escape double-quotes in the label to prevent malformed DOT output.
        let escaped_label = label.replace('"', "\\\"");
        writeln!(
            dot,
            "    n{} [label=\"{escaped_label}\", shape={shape}];",
            idx.index()
        )
        .expect("writeln to String is infallible");
    }

    dot.push('\n');

    for edge_ref in graph.graph.edge_references() {
        writeln!(
            dot,
            "    n{} -> n{} [label=\"{}\"];",
            edge_ref.source().index(),
            edge_ref.target().index(),
            edge_ref.weight()
        )
        .expect("writeln to String is infallible");
    }

    dot.push_str("}\n");
    dot
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphJson {
    pub nodes: Vec<serde_json::Value>,
    pub edges: Vec<EdgeJson>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeJson {
    pub from: usize,
    pub to: usize,
    pub edge_type: String,
}

pub fn export_json(graph: &InsightGraph) -> GraphJson {
    let nodes: Vec<serde_json::Value> = graph
        .graph
        .node_indices()
        .map(|idx| {
            let node = &graph.graph[idx];
            // InsightNode contains only JSON-compatible fields. A failure
            // indicates a broken serialization contract and must not produce
            // a plausible but incomplete graph.
            let mut v = serde_json::to_value(node)
                .expect("InsightNode serialization must produce a JSON value");
            if let serde_json::Value::Object(ref mut m) = v {
                m.insert("id".to_string(), serde_json::json!(idx.index()));
            }
            v
        })
        .collect();

    let edges: Vec<EdgeJson> = graph
        .graph
        .edge_references()
        .map(|e| EdgeJson {
            from: e.source().index(),
            to: e.target().index(),
            edge_type: e.weight().to_string(),
        })
        .collect();

    GraphJson { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{MethodSymbol, ObjectKind, SymbolEntry, SymbolIndex};

    fn make_codeunit_with_events(
        id: i32,
        name: &str,
        events: Vec<(&str, &str)>, // (method_name, attr_name)
        subscribers: Vec<(&str, &str, &str, &str)>, // (method, target_kind, target_obj, target_event)
    ) -> SymbolEntry {
        let mut methods = Vec::new();
        for (mname, attr) in events {
            methods.push(MethodSymbol {
                name: mname.to_string(),
                return_type: None,
                parameters: Vec::new(),
                is_local: false,
                attributes: vec![al_symbols::AttributeSymbol {
                    name: attr.to_string(),
                    arguments: vec!["false".to_string(), "false".to_string()],
                }],
            });
        }
        for (mname, tkind, tobj, tevent) in subscribers {
            methods.push(MethodSymbol {
                name: mname.to_string(),
                return_type: None,
                parameters: Vec::new(),
                is_local: false,
                attributes: vec![al_symbols::AttributeSymbol {
                    name: "EventSubscriber".to_string(),
                    arguments: vec![
                        format!("ObjectType::{tkind}"),
                        format!("{tkind}::\"{tobj}\""),
                        format!("'{tevent}'"),
                    ],
                }],
            });
        }
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
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    /// Codeunit with plain (non-event, non-subscriber) procedures.
    fn make_codeunit_with_procs(id: i32, name: &str, procs: Vec<&str>) -> SymbolEntry {
        let mut entry = make_codeunit_with_events(id, name, Vec::new(), Vec::new());
        entry.methods = procs
            .into_iter()
            .map(|proc_name| MethodSymbol {
                name: proc_name.to_string(),
                return_type: None,
                parameters: Vec::new(),
                is_local: false,
                attributes: Vec::new(),
            })
            .collect();
        entry
    }

    #[test]
    fn trace_event_finds_subscribers() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit_with_events(1, "Publisher", vec![("OnPost", "IntegrationEvent")], vec![]),
            make_codeunit_with_events(
                2,
                "Subscriber",
                vec![],
                vec![("HandleOnPost", "Codeunit", "Publisher", "OnPost")],
            ),
        ]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);

        let trace = trace_event(&graph, "OnPost", 10);
        assert!(!trace.is_empty());
        assert_eq!(trace[0].name, "OnPost");
        assert_eq!(trace[0].node_type, "event");
    }

    #[test]
    fn trace_event_is_deterministic_across_repeated_builds() {
        // Multiple publishers with the same event name must have stable ordering.
        let index = SymbolIndex::new();
        // iteration-arbitrary.
        index.add_entries(&[
            make_codeunit_with_events(
                1,
                "Publisher A",
                vec![("Shared", "IntegrationEvent")],
                vec![],
            ),
            make_codeunit_with_events(
                2,
                "Publisher B",
                vec![("Shared", "IntegrationEvent")],
                vec![],
            ),
            make_codeunit_with_events(
                3,
                "Publisher C",
                vec![("Shared", "IntegrationEvent")],
                vec![],
            ),
        ]);

        let mut reference: Option<Vec<TraceStep>> = None;
        for _ in 0..5 {
            let mut graph = InsightGraph::new();
            graph.build_from_index(&index);
            let trace = trace_event(&graph, "Shared", 10);
            match &reference {
                None => reference = Some(trace),
                Some(prev) => {
                    let prev_objs: Vec<&str> = prev.iter().map(|s| s.object.as_str()).collect();
                    let now_objs: Vec<&str> = trace.iter().map(|s| s.object.as_str()).collect();
                    assert_eq!(
                        prev_objs, now_objs,
                        "trace_event order must be deterministic across rebuilds"
                    );
                }
            }
        }
    }

    #[test]
    fn trace_event_respects_max_depth() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit_with_events(
            1,
            "CU",
            vec![("MyEvent", "IntegrationEvent")],
            vec![],
        )]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);

        let trace = trace_event(&graph, "MyEvent", 0);
        assert_eq!(trace.len(), 1);
    }

    #[test]
    fn export_dot_produces_valid_output() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit_with_events(
            1,
            "TestCU",
            vec![("MyEvent", "IntegrationEvent")],
            vec![],
        )]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);

        let dot = export_dot(&graph);
        assert!(dot.starts_with("digraph insight {"));
        assert!(dot.contains("rankdir=LR"));
        assert!(dot.ends_with("}\n"));
        assert!(dot.contains("TestCU"));
    }

    #[test]
    fn export_json_has_nodes_and_edges() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit_with_events(
            1,
            "TestCU",
            vec![("MyEvent", "BusinessEvent")],
            vec![],
        )]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);

        let json = export_json(&graph);
        assert!(!json.nodes.is_empty());
        assert!(!json.edges.is_empty());
        assert!(json.edges.iter().any(|e| e.edge_type == "publishes"));
    }

    #[test]
    fn find_entry_points_returns_procedures_without_callers() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit_with_events(
            1,
            "TestCU",
            vec![("OnRun", "IntegrationEvent")],
            vec![],
        )]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);

        let entry_points = find_entry_points(&graph);
        assert!(entry_points.is_empty());
    }

    // trace_event_chain tests

    fn make_cu(
        id: i32,
        name: &str,
        events: Vec<(&str, &str)>,
        subscribers: Vec<(&str, &str, &str, &str)>,
    ) -> SymbolEntry {
        make_codeunit_with_events(id, name, events, subscribers)
    }

    #[test]
    fn chain_returns_root_event() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_cu(
            1,
            "CU",
            vec![("OnPost", "IntegrationEvent")],
            vec![],
        )]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        let chain = trace_event_chain(&insight, &cg, "OnPost", 10);
        assert_eq!(chain.event_name, "OnPost");
        assert_eq!(chain.chains.len(), 1);
        assert_eq!(chain.chains[0].node_type, "event");
        assert_eq!(chain.chains[0].name, "OnPost");
        assert_eq!(chain.chains[0].depth, 0);
    }

    #[test]
    fn chain_finds_single_subscriber() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(
                1,
                "SalesPost",
                vec![("OnAfterPost", "IntegrationEvent")],
                vec![],
            ),
            make_cu(
                2,
                "MyExt",
                vec![],
                vec![("HandleAfterPost", "Codeunit", "SalesPost", "OnAfterPost")],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        let chain = trace_event_chain(&insight, &cg, "OnAfterPost", 10);
        assert_eq!(chain.chains.len(), 1);

        let root = &chain.chains[0];
        assert_eq!(root.children.len(), 1);
        let sub = &root.children[0];
        assert_eq!(sub.node_type, "subscriber");
        assert_eq!(sub.name, "HandleAfterPost");
        assert_eq!(sub.object, "MyExt");
        assert!(!sub.cycle);
    }

    #[test]
    fn chain_finds_multiple_subscribers() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "Publisher", vec![("OnRelease", "BusinessEvent")], vec![]),
            make_cu(
                2,
                "SubA",
                vec![],
                vec![("H1", "Codeunit", "Publisher", "OnRelease")],
            ),
            make_cu(
                3,
                "SubB",
                vec![],
                vec![("H2", "Codeunit", "Publisher", "OnRelease")],
            ),
            make_cu(
                4,
                "SubC",
                vec![],
                vec![("H3", "Codeunit", "Publisher", "OnRelease")],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        let chain = trace_event_chain(&insight, &cg, "OnRelease", 10);
        let root = &chain.chains[0];
        assert_eq!(root.children.len(), 3);
        for child in &root.children {
            assert_eq!(child.node_type, "subscriber");
            assert_eq!(child.depth, 1);
        }
    }

    #[test]
    fn chain_cycle_detection_prevents_infinite_loop() {
        // A subscribes to EventC (published by C) and publishes EventA.
        // B subscribes to EventA and publishes EventB.
        // C subscribes to EventB and publishes EventC.
        // This forms a cycle: EventA → SubB(B) → ... → SubA(A) → EventA
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(
                1,
                "CU-A",
                vec![("EventA", "IntegrationEvent")],
                vec![("HandleEventC", "Codeunit", "CU-C", "EventC")],
            ),
            make_cu(
                2,
                "CU-B",
                vec![("EventB", "IntegrationEvent")],
                vec![("HandleEventA", "Codeunit", "CU-A", "EventA")],
            ),
            make_cu(
                3,
                "CU-C",
                vec![("EventC", "IntegrationEvent")],
                vec![("HandleEventB", "Codeunit", "CU-B", "EventB")],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        // Must complete without hanging; cycle nodes get `cycle: true`.
        let chain = trace_event_chain(&insight, &cg, "EventA", 20);
        assert_eq!(chain.event_name, "EventA");
        assert!(!chain.chains.is_empty());
    }

    #[test]
    fn chain_respects_max_depth_zero() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "Pub", vec![("OnPost", "IntegrationEvent")], vec![]),
            make_cu(
                2,
                "Sub",
                vec![],
                vec![("Handle", "Codeunit", "Pub", "OnPost")],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        let chain = trace_event_chain(&insight, &cg, "OnPost", 0);
        assert_eq!(chain.chains.len(), 1);
        assert!(chain.chains[0].children.is_empty());
    }

    #[test]
    fn chain_through_direct_call_to_event() {
        // Sub handles EventA, then calls ProcedureX (via direct call),
        // and ProcedureX is wired to EventB (via direct call edge to an event node).
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "Pub", vec![("EventA", "IntegrationEvent")], vec![]),
            make_cu(
                2,
                "Mid",
                vec![("EventB", "IntegrationEvent")],
                vec![("HandleEventA", "Codeunit", "Pub", "EventA")],
            ),
            make_cu(
                3,
                "Final",
                vec![],
                vec![("HandleEventB", "Codeunit", "Mid", "EventB")],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        // EventA → SubMid(HandleEventA) — Mid also publishes EventB
        // EventB → SubFinal(HandleEventB)
        // This is a two-hop event chain entirely through subscriptions.
        let chain = trace_event_chain(&insight, &cg, "EventA", 10);

        let root = &chain.chains[0];
        // Root has one subscriber: HandleEventA in Mid
        assert_eq!(root.children.len(), 1);
        let sub_mid = &root.children[0];
        assert_eq!(sub_mid.object, "Mid");

        // Mid.HandleEventA has no *direct-call* edges to EventB in the call graph
        // because those are subscription-level — the test validates the chain terminates
        // gracefully even when further hops require direct-call edges that aren't present.
        // The subscriber is still found correctly.
        assert_eq!(sub_mid.node_type, "subscriber");
    }

    #[test]
    fn chain_event_name_case_insensitive() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_cu(
            1,
            "CU",
            vec![("OnPost", "IntegrationEvent")],
            vec![],
        )]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        let chain_lower = trace_event_chain(&insight, &cg, "onpost", 10);
        let chain_upper = trace_event_chain(&insight, &cg, "ONPOST", 10);
        let chain_mixed = trace_event_chain(&insight, &cg, "OnPost", 10);

        assert_eq!(chain_lower.chains.len(), 1);
        assert_eq!(chain_upper.chains.len(), 1);
        assert_eq!(chain_mixed.chains.len(), 1);
    }

    #[test]
    fn chain_unknown_event_returns_empty() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_cu(
            1,
            "CU",
            vec![("OnPost", "IntegrationEvent")],
            vec![],
        )]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        let chain = trace_event_chain(&insight, &cg, "NonExistent", 10);
        assert!(chain.chains.is_empty());
        assert_eq!(chain.nodes_visited, 0);
    }

    #[test]
    fn trace_event_chain_is_deterministic_across_repeated_builds() {
        // Regression: trace_event_chain (the richer twin of trace_event)
        // collected roots from `insight.index` (HashMap) without sorting,
        // so the chain order was arbitrary across rebuilds. trace_event was
        // already fixed and has its own determinism test; this asserts
        // parity for trace_event_chain.
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "Pub A", vec![("Shared", "IntegrationEvent")], vec![]),
            make_cu(2, "Pub B", vec![("Shared", "IntegrationEvent")], vec![]),
            make_cu(3, "Pub C", vec![("Shared", "IntegrationEvent")], vec![]),
        ]);

        let mut reference: Option<Vec<String>> = None;
        for _ in 0..5 {
            let mut insight = InsightGraph::new();
            insight.build_from_index(&index);
            let cg = CallGraph::build_from_insight(&insight);
            let chain = trace_event_chain(&insight, &cg, "Shared", 5);
            let objects: Vec<String> = chain.chains.iter().map(|c| c.object.clone()).collect();
            match &reference {
                None => reference = Some(objects),
                Some(prev) => assert_eq!(
                    prev, &objects,
                    "trace_event_chain root order must be deterministic across rebuilds"
                ),
            }
        }
    }

    #[test]
    fn trace_event_chain_children_are_deterministic_across_repeated_builds() {
        // Regression: `subscribers_of` / `callees_of` returned NodeIds in
        // CallGraph build order, which is itself driven by DashMap iteration
        // in SymbolIndex — non-deterministic across process restarts. Now
        // sorted inside `recurse_event` / `recurse_subscriber` so the children
        // list is stable. Test: a single publisher emitting an event subscribed
        // by three subscribers; chain roots are deterministic (already
        // covered) AND the subscriber list under each root is too.
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "Pub", vec![("OnPost", "IntegrationEvent")], vec![]),
            make_cu(
                2,
                "Sub A",
                vec![],
                vec![("OnPost1", "Codeunit", "Pub", "OnPost")],
            ),
            make_cu(
                3,
                "Sub B",
                vec![],
                vec![("OnPost2", "Codeunit", "Pub", "OnPost")],
            ),
            make_cu(
                4,
                "Sub C",
                vec![],
                vec![("OnPost3", "Codeunit", "Pub", "OnPost")],
            ),
        ]);

        let mut reference: Option<Vec<String>> = None;
        for _ in 0..5 {
            let mut insight = InsightGraph::new();
            insight.build_from_index(&index);
            let cg = CallGraph::build_from_insight(&insight);
            let chain = trace_event_chain(&insight, &cg, "OnPost", 5);
            // Flatten subscriber objects across all roots in the order they
            // appear — any non-determinism in either roots or children would
            // perturb this vector.
            let flat: Vec<String> = chain
                .chains
                .iter()
                .flat_map(|c| c.children.iter().map(|s| s.object.clone()))
                .collect();
            match &reference {
                None => {
                    assert_eq!(
                        flat.len(),
                        3,
                        "test fixture must produce 3 subscribers across the chain"
                    );
                    reference = Some(flat);
                }
                Some(prev) => assert_eq!(
                    prev, &flat,
                    "trace_event_chain children must be deterministic across rebuilds"
                ),
            }
        }
    }

    #[test]
    fn export_dot_escapes_double_quotes_in_labels() {
        // Object name containing a double-quote must not produce malformed DOT.
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit_with_events(
            1,
            "My \"Special\" CU",
            vec![("OnPost", "IntegrationEvent")],
            vec![],
        )]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);

        let dot = export_dot(&graph);
        // The label must not contain an unescaped bare double-quote that would
        // break DOT parsing.  After escaping, every `"` in the name becomes `\"`.
        // We verify by checking the label does not contain `"My "` which would
        // close the label attribute prematurely.
        assert!(!dot.contains(r#"label="My "Special""#));
        // The escaped form must be present.
        assert!(dot.contains(r#"My \"Special\" CU"#));
        // Overall DOT structure is still intact.
        assert!(dot.starts_with("digraph insight {"));
        assert!(dot.ends_with("}\n"));
    }

    #[test]
    fn chain_direct_cycle_subscriber_to_root_event_detected() {
        // CU-A publishes EventA AND subscribes to EventA (self-subscription via alias).
        // Verifies that the root event is marked visited before recursion so that
        // re-entry via the subscriber's outgoing EventSubscription edge is detected
        // as a cycle in one step rather than consuming two extra depth levels.
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "CU-A", vec![("EventA", "IntegrationEvent")], vec![]),
            make_cu(
                2,
                "CU-B",
                vec![],
                vec![("HandleEventA", "Codeunit", "CU-A", "EventA")],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&insight);

        // Use max_depth=2 — with the fix, even a tight depth limit should
        // find the subscriber without running out of budget to cycle detection.
        let chain = trace_event_chain(&insight, &cg, "EventA", 2);
        assert_eq!(chain.chains.len(), 1);
        let root = &chain.chains[0];
        assert_eq!(root.children.len(), 1);
        let sub = &root.children[0];
        assert_eq!(sub.name, "HandleEventA");
        assert_eq!(sub.object, "CU-B");
        // The subscriber's callee (EventA) is the root — it must be detected
        // as a cycle rather than consuming more depth.
        for child in &sub.children {
            if child.node_type == "event" {
                assert!(child.cycle, "re-entry into root EventA must be cycle=true");
            }
        }
    }

    /// `find_entry_points` filters on insight-graph `Calls` edges, but no
    /// production code ever adds those — so without a call graph the result
    /// degenerates to "every procedure". With the call graph, a called
    /// procedure is correctly excluded.
    #[test]
    fn entry_points_use_the_call_graph_to_exclude_called_procedures() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit_with_procs(1, "CU", vec!["Entry", "Helper"])]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let mut cg = CallGraph::build_from_insight(&insight);

        let entry = insight
            .get_node(&NodeKey::Procedure(
                ObjectKind::Codeunit,
                "cu".to_string(),
                "entry".to_string(),
            ))
            .unwrap();
        let helper = insight
            .get_node(&NodeKey::Procedure(
                ObjectKind::Codeunit,
                "cu".to_string(),
                "helper".to_string(),
            ))
            .unwrap();
        cg.add_direct_call(NodeId::from(entry), NodeId::from(helper));

        // Without a call graph both procedures look like entry points.
        let names_without: Vec<String> = find_entry_points(&insight)
            .into_iter()
            .filter_map(|node| match node {
                InsightNode::Procedure { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names_without.len(), 2);

        let mut names: Vec<String> = find_entry_points_with_calls(&insight, Some(&cg))
            .into_iter()
            .filter_map(|node| match node {
                InsightNode::Procedure { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["Entry".to_string()],
            "a called procedure is not an entry point"
        );
    }

    /// The flat trace pushed every event published by a subscriber's *object*
    /// as a "publishes" hop without checking the subscriber ever raises it.
    #[test]
    fn trace_event_does_not_fabricate_publishes_hops() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "Pub", vec![("OnPost", "IntegrationEvent")], vec![]),
            // The subscriber's object also publishes an unrelated event.
            make_cu(
                2,
                "Sub",
                vec![("OnUnrelated", "IntegrationEvent")],
                vec![("Handle", "Codeunit", "Pub", "OnPost")],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);

        let steps = trace_event(&insight, "OnPost", 10);
        assert!(
            steps.iter().any(|s| s.name == "Handle"),
            "the subscriber must still be listed: {steps:?}"
        );
        assert!(
            !steps.iter().any(|s| s.name == "OnUnrelated"),
            "an event the subscriber never raises must not appear: {steps:?}"
        );
    }

    /// A subscriber that really does raise another event still produces the hop
    /// when a call graph is supplied.
    #[test]
    fn trace_event_follows_events_the_subscriber_actually_raises() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "Pub", vec![("OnPost", "IntegrationEvent")], vec![]),
            make_cu(
                2,
                "Sub",
                vec![("OnSecondary", "IntegrationEvent")],
                vec![("Handle", "Codeunit", "Pub", "OnPost")],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let mut cg = CallGraph::build_from_insight(&insight);

        let handler = insight
            .get_node(&NodeKey::Subscriber(
                ObjectKind::Codeunit,
                "sub".to_string(),
                "handle".to_string(),
            ))
            .unwrap();
        let secondary = insight
            .get_node(&NodeKey::Event(
                ObjectKind::Codeunit,
                "sub".to_string(),
                "onsecondary".to_string(),
            ))
            .unwrap();
        cg.add_direct_call(NodeId::from(handler), NodeId::from(secondary));

        let steps = trace_event_with_calls(&insight, Some(&cg), "OnPost", 10);
        assert!(
            steps
                .iter()
                .any(|s| s.name == "OnSecondary" && s.edge_type == "publishes"),
            "a genuinely raised event must appear: {steps:?}"
        );
    }

    /// One `visited` set shared across same-named roots truncated every chain
    /// after the first.
    #[test]
    fn trace_event_gives_each_root_its_own_visited_set() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_cu(1, "PubA", vec![("OnPost", "IntegrationEvent")], vec![]),
            make_cu(2, "PubB", vec![("OnPost", "IntegrationEvent")], vec![]),
            make_cu(
                3,
                "SubBoth",
                vec![],
                vec![
                    ("HandleA", "Codeunit", "PubA", "OnPost"),
                    ("HandleB", "Codeunit", "PubB", "OnPost"),
                ],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);

        let steps = trace_event(&insight, "OnPost", 10);
        let origins = steps.iter().filter(|s| s.edge_type == "origin").count();
        assert_eq!(origins, 2, "both publishers are roots: {steps:?}");
        assert!(steps.iter().any(|s| s.name == "HandleA"), "{steps:?}");
        assert!(
            steps.iter().any(|s| s.name == "HandleB"),
            "the second root's chain must not be truncated: {steps:?}"
        );
    }
}
