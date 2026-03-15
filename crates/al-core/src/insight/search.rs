//! Graph search algorithms for the insight engine.
//!
//! Provides traversal queries: event chain tracing, entry point finding,
//! and graph export (DOT, JSON).

use petgraph::Direction;
use petgraph::visit::EdgeRef;
use serde::Serialize;

use super::graph::{InsightEdge, InsightGraph, InsightNode, NodeKey};

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

    // Find all Event nodes matching the name
    for (key, &idx) in &graph.index {
        if let NodeKey::Event(_, _, ref name) = key {
            if name == &event_lower {
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
        }
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
            for (key, &evt_idx) in &graph.index {
                if let NodeKey::Event(_, ref obj, _) = key {
                    if *obj == sub_obj_lower {
                        let evt_node = &graph.graph[evt_idx];
                        if let InsightNode::Event { name: ename, .. } = evt_node {
                            steps.push(TraceStep {
                                depth: depth + 1,
                                edge_type: "publishes".to_string(),
                                node_type: "event".to_string(),
                                name: ename.clone(),
                                object: object_name.clone(),
                            });
                            trace_from_node(graph, evt_idx, depth + 2, max_depth, visited, steps);
                        }
                    }
                }
            }
        }
    }
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
            InsightNode::Object { kind, name, .. } => {
                (format!("{kind}\\n{name}"), "box")
            }
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
        dot.push_str(&format!(
            "    n{} [label=\"{}\", shape={}];\n",
            idx.index(),
            label,
            shape
        ));
    }

    dot.push('\n');

    for edge_ref in graph.graph.edge_references() {
        dot.push_str(&format!(
            "    n{} -> n{} [label=\"{}\"];\n",
            edge_ref.source().index(),
            edge_ref.target().index(),
            edge_ref.weight()
        ));
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
    use al_symbols::{MethodSymbol, ObjectKind, SymbolEntry, SymbolIndex};

    fn make_codeunit_with_events(
        id: i32,
        name: &str,
        events: Vec<(&str, &str)>,       // (method_name, attr_name)
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
            kind: ObjectKind::Codeunit,
            id,
            name: name.to_string(),
            extends: None,
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
            make_codeunit_with_events(
                1,
                "Publisher",
                vec![("OnPost", "IntegrationEvent")],
                vec![],
            ),
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
}
