//! Graph search algorithms for the insight engine.
//!
//! Provides traversal queries: event chain tracing, entry point finding,
//! and graph export (DOT, JSON).
//!
//! ## Event chain tracing (T902)
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
use super::index::{CallGraph, EdgeKind, NodeId};

/// Upper bound on total nodes visited by `trace_event_chain` across all
/// branches of the recursion. The existing `max_depth` cap protects
/// against deep chains, but a wide-but-shallow event tree (one publisher
/// with thousands of subscribers, each fanning out further) can clone
/// thousands of `ChainNode` strings before exhausting depth. F-OPEN-027.
///
/// 10_000 nodes × ~100-byte chain entries = ~1 MB worst-case response —
/// the bound is per-query so well-behaved BC workspaces (typically <1k
/// nodes per trace) never hit it.
const MAX_CHAIN_NODES: usize = 10_000;

/// A single step in an event trace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceStep {
    pub depth: usize,
    pub edge_type: String,
    pub node_type: String,
    pub name: String,
    pub object: String,
}

/// Trace an event chain: starting from an event, follow SubscribesTo edges
/// to find all subscribers, then follow their published events recursively.
///
/// Returns a flattened list of steps representing the event propagation chain.
pub fn trace_event(graph: &InsightGraph, event_name: &str, max_depth: usize) -> Vec<TraceStep> {
    let event_lower = event_name.to_lowercase();
    let mut steps = Vec::new();
    let mut visited = std::collections::HashSet::new();

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
            node_type: "event".to_string(),
            name: event_label,
            object: obj_name,
        });

        trace_from_node(graph, idx, 1, max_depth, &mut visited, &mut steps);
    }

    steps
}

fn trace_from_node(
    graph: &InsightGraph,
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

    // Follow incoming SubscribesTo edges (subscribers pointing to this event)
    for edge_ref in graph.graph.edges_directed(node_idx, Direction::Incoming) {
        if *edge_ref.weight() != InsightEdge::SubscribesTo {
            continue;
        }

        let sub_idx = edge_ref.source();
        let sub_node = &graph.graph[sub_idx];

        if let InsightNode::Subscriber {
            object_name, name, ..
        } = sub_node
        {
            steps.push(TraceStep {
                depth,
                edge_type: "subscribes_to".to_string(),
                node_type: "subscriber".to_string(),
                name: name.clone(),
                object: object_name.clone(),
            });

            // Find events published by the same object
            let sub_obj_lower = object_name.to_lowercase();
            for (key, indices) in &graph.index {
                if let NodeKey::Event(_, ref obj, _) = key {
                    if *obj == sub_obj_lower {
                        for &evt_idx in indices {
                            let evt_node = &graph.graph[evt_idx];
                            if let InsightNode::Event { name: ename, .. } = evt_node {
                                steps.push(TraceStep {
                                    depth: depth + 1,
                                    edge_type: "publishes".to_string(),
                                    node_type: "event".to_string(),
                                    name: ename.clone(),
                                    object: object_name.clone(),
                                });
                                trace_from_node(
                                    graph,
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
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T902: Full event chain tracing via CallGraph
// ---------------------------------------------------------------------------

/// A node in the event chain tree.
///
/// Each `ChainNode` represents one step in the propagation of an event through
/// the system.  Children are the nodes reachable from this one (subscribers,
/// called procedures that publish further events, etc.).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainNode {
    /// How this node was reached from its parent.
    pub edge_kind: String,
    /// "event", "subscriber", "procedure", or "object".
    pub node_type: String,
    /// Method/procedure/event name.
    pub name: String,
    /// Owning object name.
    pub object: String,
    /// Depth from the root event (0 = root).
    pub depth: usize,
    /// Whether this node was already visited (cycle).  If `true`, children
    /// are empty to prevent infinite recursion.
    pub cycle: bool,
    /// Child steps reachable from this node.
    pub children: Vec<ChainNode>,
}

/// Result of [`trace_event_chain`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventChain {
    /// The publisher event name that was searched.
    pub event_name: String,
    /// The publisher object name (empty when unresolved).
    pub publisher_object: String,
    /// Roots of the chain tree (one per matching event node found in the graph).
    pub chains: Vec<ChainNode>,
    /// Total distinct nodes visited.
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
/// 3. Tracks visited `NodeId`s to break cycles.
///
/// The result is a tree (`EventChain`) that faithfully represents the shape of
/// propagation including diamond patterns and (marked) back-edges.
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

    // Find all event nodes matching the name.
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
                node_type: "event".to_string(),
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

/// Recursive helper: given an event node, find all its subscribers and recurse.
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

    let subscribers = cg.subscribers_of(event_id);
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
            node_type: "subscriber".to_string(),
            name: info.map(|i| i.name.clone()).unwrap_or_default(),
            object: info.map(|i| i.object.clone()).unwrap_or_default(),
            depth,
            cycle: is_cycle,
            children: sub_children,
        });
    }

    children
}

/// Recursive helper: given a subscriber node, find calls it makes that lead to
/// further event publications.
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

    let callees = cg.callees_of(sub_id);
    let mut children = Vec::new();

    for edge in callees {
        let callee_id = edge.to;
        let info = cg.node_info(callee_id);
        let node_type = info.map(|i| i.node_type.as_str()).unwrap_or("unknown");

        match node_type {
            "event" => {
                // This subscriber's object also publishes an event — recurse into it.
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
                    node_type: "event".to_string(),
                    name: info.map(|i| i.name.clone()).unwrap_or_default(),
                    object: info.map(|i| i.object.clone()).unwrap_or_default(),
                    depth,
                    cycle: is_cycle,
                    children: grandchildren,
                });
            }
            "procedure" => {
                // Follow the procedure's own callees one hop to detect event re-publications.
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
                        node_type: "procedure".to_string(),
                        name: info.map(|i| i.name.clone()).unwrap_or_default(),
                        object: info.map(|i| i.object.clone()).unwrap_or_default(),
                        depth,
                        cycle: is_cycle,
                        children: grandchildren,
                    });
                }
            }
            _ => {
                // Objects and other node types: don't expand further.
            }
        }
    }

    children
}

/// Find entry points: objects/procedures that have no incoming Calls/SubscribesTo edges.
/// These are potential starting points for analysis.
pub fn find_entry_points(graph: &InsightGraph) -> Vec<&InsightNode> {
    graph
        .graph
        .node_indices()
        .filter(|&idx| {
            let node = &graph.graph[idx];
            // Only consider procedures as entry points
            matches!(node, InsightNode::Procedure { .. })
                && graph
                    .graph
                    .edges_directed(idx, Direction::Incoming)
                    .all(|e| *e.weight() == InsightEdge::Contains)
        })
        .map(|idx| &graph.graph[idx])
        .collect()
}

/// Export the graph as DOT format for Graphviz visualization.
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

/// Export the graph as JSON.
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
            let mut v = serde_json::to_value(node).unwrap_or_default();
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
    use crate::symbols::{MethodSymbol, ObjectKind, SymbolEntry, SymbolIndex};

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
                attributes: vec![crate::symbols::AttributeSymbol {
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
                attributes: vec![crate::symbols::AttributeSymbol {
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
        // Regression for the audit finding: trace_event used to iterate
        // `graph.index` (a HashMap) directly, so the order of matching
        // event roots was arbitrary between graph rebuilds. The fix
        // sorts matching NodeIndex values before walking. This test
        // builds the same graph 5 times and asserts trace output is
        // byte-identical every time. F-OPEN-(insight-audit-13).
        let index = SymbolIndex::new();
        // Two publishers emitting the same event name — without the
        // sort, their relative order in the trace would be HashMap-
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
        // With max_depth=0, we only get the origin event
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
        // Should have Contains edge (object -> event)
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
        // OnRun is not a Procedure node, it's an Event node
        // There should be no procedure entry points in this graph
        assert!(entry_points.is_empty());
    }

    // -----------------------------------------------------------------------
    // T902: trace_event_chain tests
    // -----------------------------------------------------------------------

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
        // Three subscribers at depth 1
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
        // Result exists (not empty) — chain was explored.
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
        // max_depth=0: only the root event, no children expanded.
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

        // Search with different case
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
        // Subscriber HandleEventA in CU-B is found at depth 1.
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
}
