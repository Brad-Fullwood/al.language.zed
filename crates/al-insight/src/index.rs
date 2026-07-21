//! Call graph construction for the AL insight engine.
//!
//! Builds a `CallGraph` — an adjacency-list representation of procedure-level
//! relationships — from the insight graph.  Three kinds of edges are tracked:
//!
//! | Kind | Meaning |
//! |------|---------|
//! | `DirectCall` | Procedure A calls Procedure B (populated from source-parsed trees) |
//! | `EventSubscription` | Subscriber A subscribes to Event B |
//! | `TriggerInvocation` | Trigger A invokes Procedure B |
//!
//! Because `.app` symbol files contain declarations only (no call-site
//! information), `DirectCall` edges can only be added when the source AST is
//! available (i.e., for workspace files parsed by al_syntax).  The graph is
//! therefore built *incrementally*: symbol-index data populates event edges
//! immediately; direct-call edges are added as source files are indexed.
//!
//! ## "Who calls this procedure?" query
//! Use [`CallGraph::callers_of`] which does an O(|edges|) scan in the worst
//! case but is fast in practice because the graph is sparse.

use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::graph::NodeIndex;
use serde::Serialize;

use super::graph::{InsightEdge, InsightGraph, InsightNode, NodeKey};

/// Stable identifier for a node inside a `CallGraph`.
///
/// **Lifetime warning:** `NodeId` is stable only within a single
/// `Workspace` graph build. After
/// `Workspace::invalidate_insight_graph` runs (e.g.
/// when packages reload or a file changes), the rebuilt graph creates new
/// indices and old `NodeId`s no longer point at the same node — they may
/// be out-of-bounds or refer to an unrelated node. Do not cache `NodeId`s
/// across request boundaries or across graph rebuilds; always re-resolve
/// from a fresh `Arc<InsightGraph>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct NodeId(pub usize);

impl From<NodeIndex> for NodeId {
    fn from(idx: NodeIndex) -> Self {
        NodeId(idx.index())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// A direct procedure-to-procedure call (source-derived).
    DirectCall,
    EventSubscription,
    TriggerInvocation,
    /// A record operation (Insert/Modify/Delete/Validate) triggers table events.
    RecordTrigger,
    /// A *polymorphic / indirect* call that cannot be resolved to a single
    /// concrete callee at the call site, so it is **over-approximated** to all
    /// possible runtime targets. Three patterns are modelled, all as
    /// forward `caller → possible-target` edges:
    ///
    /// - **Interface dispatch** — an `Interface "IFoo"`-typed variable calling
    ///   `.Bar()` produces an edge to `Bar` in *every* codeunit that
    ///   `implements IFoo`.
    /// - **`Codeunit.Run(Codeunit::"X")` / `RunModal`** with a literal target
    ///   produces an edge to `X`'s `OnRun` trigger.
    /// - **Event publish site → subscribers** — a procedure that invokes an
    ///   event publisher produces an edge to each `[EventSubscriber]` handler.
    ///
    /// The over-approximation is **sound for reachability** (coverage and
    /// affected-test analysis): it only ever *adds* edges, so it removes false
    /// negatives at the cost of possible false positives. Kept distinct from
    /// `DirectCall` so consumers can tell a resolved call from an
    /// over-approximated one.
    IndirectCall,
}

impl std::fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EdgeKind::DirectCall => write!(f, "direct_call"),
            EdgeKind::EventSubscription => write!(f, "event_subscription"),
            EdgeKind::TriggerInvocation => write!(f, "trigger_invocation"),
            EdgeKind::RecordTrigger => write!(f, "record_trigger"),
            EdgeKind::IndirectCall => write!(f, "indirect_call"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeResolutionState {
    Unresolved,
    Resolving,
    Resolved,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallEdge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub node_type: String,
    pub name: String,
    pub object: String,
}

/// A directed call graph over AL procedures, events, and subscribers.
///
/// Stored as an adjacency list: `outgoing[node] = [edges leaving node]`.
/// A reverse index `incoming[node] = [edges entering node]` is maintained in
/// sync so that "callers of X" queries are O(deg(X)) rather than O(|E|).
#[derive(Debug, Default)]
pub struct CallGraph {
    outgoing: HashMap<NodeId, Vec<CallEdge>>,
    /// Reverse adjacency list (same edges, indexed by target).
    incoming: HashMap<NodeId, Vec<CallEdge>>,
    nodes: HashMap<NodeId, NodeInfo>,
    resolution: HashMap<NodeId, EdgeResolutionState>,
}

impl CallGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate the call graph from an already-built [`InsightGraph`].
    ///
    /// This extracts:
    /// - `EventSubscription` edges: for every `SubscribesTo` edge in the
    ///   insight graph, a subscriber → event edge is recorded.
    /// - Object/procedure/event/subscriber node metadata for display.
    ///
    /// Direct-call edges are NOT populated here because `InsightGraph` does
    /// not store them (they require source-level parsing).  Call
    /// [`add_direct_call`] or [`add_trigger_invocation`] after parsing.
    pub fn build_from_insight(graph: &InsightGraph) -> Self {
        let mut cg = CallGraph::new();

        for idx in graph.graph.node_indices() {
            let id = NodeId::from(idx);
            let info = node_info(id, &graph.graph[idx]);
            cg.nodes.insert(id, info);
        }

        for edge_ref in graph.graph.edge_references() {
            use petgraph::visit::EdgeRef;
            if *edge_ref.weight() == InsightEdge::SubscribesTo {
                let from = NodeId::from(edge_ref.source());
                let to = NodeId::from(edge_ref.target());
                cg.insert_edge(CallEdge {
                    from,
                    to,
                    kind: EdgeKind::EventSubscription,
                });
            }
        }

        cg
    }

    /// Duplicate edges are silently ignored.
    pub fn add_direct_call(&mut self, from: NodeId, to: NodeId) {
        let edge = CallEdge {
            from,
            to,
            kind: EdgeKind::DirectCall,
        };
        self.insert_edge(edge);
    }

    pub fn add_trigger_invocation(&mut self, from: NodeId, to: NodeId) {
        let edge = CallEdge {
            from,
            to,
            kind: EdgeKind::TriggerInvocation,
        };
        self.insert_edge(edge);
    }

    pub fn add_trigger(&mut self, from: NodeId, to: NodeId) {
        let edge = CallEdge {
            from,
            to,
            kind: EdgeKind::RecordTrigger,
        };
        self.insert_edge(edge);
    }

    /// Add an over-approximated indirect-call edge (interface dispatch,
    /// `Codeunit.Run` dispatch, or event publish→subscriber). See
    /// [`EdgeKind::IndirectCall`]. Duplicate edges are silently ignored.
    pub fn add_indirect_call(&mut self, from: NodeId, to: NodeId) {
        let edge = CallEdge {
            from,
            to,
            kind: EdgeKind::IndirectCall,
        };
        self.insert_edge(edge);
    }

    /// Remove all outgoing edges from `node`. Used for invalidation.
    pub fn remove_edges_from(&mut self, node: NodeId) {
        if let Some(edges) = self.outgoing.remove(&node) {
            for edge in &edges {
                if let Some(incoming) = self.incoming.get_mut(&edge.to) {
                    incoming.retain(|e| e.from != node);
                }
            }
        }
    }

    pub fn node_id_for(graph: &InsightGraph, key: &NodeKey) -> Option<NodeId> {
        graph.get_node(key).map(NodeId::from)
    }

    pub fn callees_of(&self, node: NodeId) -> &[CallEdge] {
        self.outgoing.get(&node).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Return all incoming edges into `node` — i.e., who calls `node`.
    pub fn callers_of(&self, node: NodeId) -> &[CallEdge] {
        self.incoming.get(&node).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Reverse reachability: every node that transitively *reaches* any node in
    /// `seeds` by following incoming edges (callers/subscribers/triggerers),
    /// plus the seeds themselves.
    ///
    /// This is the core primitive for call-graph-based affected-test detection
    /// seed with the procedures/events of a changed object and the
    /// returned set contains every procedure (test or otherwise) that
    /// transitively depends on the change. A test is "affected" iff its
    /// procedure node is in this set.
    ///
    /// Breadth-first over the reverse adjacency list; each reachable node is
    /// expanded exactly once, so the cost is O(V + E) over the reachable
    /// sub-graph regardless of how many seeds share ancestors.
    pub fn reachable_callers<I>(&self, seeds: I) -> HashSet<NodeId>
    where
        I: IntoIterator<Item = NodeId>,
    {
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut queue: VecDeque<NodeId> = VecDeque::new();
        for seed in seeds {
            if visited.insert(seed) {
                queue.push_back(seed);
            }
        }
        while let Some(node) = queue.pop_front() {
            for edge in self.callers_of(node) {
                if visited.insert(edge.from) {
                    queue.push_back(edge.from);
                }
            }
        }
        visited
    }

    pub fn node_info(&self, id: NodeId) -> Option<&NodeInfo> {
        self.nodes.get(&id)
    }

    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.keys().copied()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.outgoing.values().map(Vec::len).sum()
    }

    pub fn subscribers_of(&self, event_id: NodeId) -> Vec<NodeId> {
        self.incoming
            .get(&event_id)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.kind == EdgeKind::EventSubscription)
                    .map(|e| e.from)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn register_node(&mut self, id: NodeId, info: NodeInfo) {
        self.nodes.insert(id, info);
    }

    pub fn resolution_state(&self, node: NodeId) -> EdgeResolutionState {
        self.resolution
            .get(&node)
            .copied()
            .unwrap_or(EdgeResolutionState::Unresolved)
    }

    pub fn set_resolution_state(&mut self, node: NodeId, state: EdgeResolutionState) {
        self.resolution.insert(node, state);
    }

    fn insert_edge(&mut self, edge: CallEdge) {
        let already = self
            .outgoing
            .get(&edge.from)
            .map(|v| v.iter().any(|e| e.to == edge.to && e.kind == edge.kind))
            .unwrap_or(false);

        if already {
            return;
        }

        self.outgoing
            .entry(edge.from)
            .or_default()
            .push(edge.clone());
        self.incoming.entry(edge.to).or_default().push(edge);
    }
}

fn node_info(id: NodeId, node: &InsightNode) -> NodeInfo {
    use super::node_kind;
    match node {
        InsightNode::Object { kind, name, .. } => NodeInfo {
            id,
            node_type: node_kind::OBJECT.to_string(),
            name: name.clone(),
            object: format!("{kind}"),
        },
        InsightNode::Procedure {
            object_name, name, ..
        } => NodeInfo {
            id,
            node_type: node_kind::PROCEDURE.to_string(),
            name: name.clone(),
            object: object_name.clone(),
        },
        InsightNode::Event {
            object_name, name, ..
        } => NodeInfo {
            id,
            node_type: node_kind::EVENT.to_string(),
            name: name.clone(),
            object: object_name.clone(),
        },
        InsightNode::Subscriber {
            object_name, name, ..
        } => NodeInfo {
            id,
            node_type: node_kind::SUBSCRIBER.to_string(),
            name: name.clone(),
            object: object_name.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{AttributeSymbol, MethodSymbol, ObjectKind, SymbolEntry, SymbolIndex};

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
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        }
    }

    fn integration_event(name: &str) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: vec![],
            return_type: None,
            is_local: false,
            attributes: vec![AttributeSymbol {
                name: "IntegrationEvent".to_string(),
                arguments: vec!["false".to_string(), "false".to_string()],
            }],
        }
    }

    fn event_subscriber(
        name: &str,
        target_type: &str,
        target_obj: &str,
        target_event: &str,
    ) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: vec![],
            return_type: None,
            is_local: false,
            attributes: vec![AttributeSymbol {
                name: "EventSubscriber".to_string(),
                arguments: vec![
                    format!("ObjectType::{target_type}"),
                    format!("{target_type}::\"{target_obj}\""),
                    format!("'{target_event}'"),
                    "''".to_string(),
                    "false".to_string(),
                    "false".to_string(),
                ],
            }],
        }
    }

    fn regular_method(name: &str) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: vec![],
            return_type: None,
            is_local: false,
            attributes: vec![],
        }
    }

    #[test]
    fn empty_call_graph() {
        let cg = CallGraph::new();
        assert_eq!(cg.node_count(), 0);
        assert_eq!(cg.edge_count(), 0);
    }

    #[test]
    fn build_from_insight_registers_all_nodes() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(1, "Publisher", vec![integration_event("OnPost")]),
            make_codeunit(
                2,
                "Subscriber",
                vec![event_subscriber(
                    "HandleOnPost",
                    "Codeunit",
                    "Publisher",
                    "OnPost",
                )],
            ),
        ]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);

        let cg = CallGraph::build_from_insight(&graph);

        // 2 objects + 1 event + 1 subscriber = 4 nodes in insight graph → 4 nodes in call graph
        assert_eq!(cg.node_count(), 4);
    }

    #[test]
    fn event_subscription_edges_present() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(1, "Publisher", vec![integration_event("OnPost")]),
            make_codeunit(
                2,
                "Subscriber",
                vec![event_subscriber(
                    "HandleOnPost",
                    "Codeunit",
                    "Publisher",
                    "OnPost",
                )],
            ),
        ]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&graph);

        let sub_key = NodeKey::Subscriber(
            ObjectKind::Codeunit,
            "subscriber".to_string(),
            "handleonpost".to_string(),
        );
        let sub_id = CallGraph::node_id_for(&graph, &sub_key).expect("subscriber node");

        let edges = cg.callees_of(sub_id);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::EventSubscription);
    }

    #[test]
    fn callers_of_event_returns_subscribers() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(1, "Publisher", vec![integration_event("OnPost")]),
            make_codeunit(
                2,
                "SubA",
                vec![event_subscriber(
                    "HandlePost",
                    "Codeunit",
                    "Publisher",
                    "OnPost",
                )],
            ),
            make_codeunit(
                3,
                "SubB",
                vec![event_subscriber(
                    "AlsoHandle",
                    "Codeunit",
                    "Publisher",
                    "OnPost",
                )],
            ),
        ]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&graph);

        let event_key = NodeKey::Event(
            ObjectKind::Codeunit,
            "publisher".to_string(),
            "onpost".to_string(),
        );
        let event_id = CallGraph::node_id_for(&graph, &event_key).expect("event node");

        let subs = cg.subscribers_of(event_id);
        assert_eq!(subs.len(), 2, "two subscribers expected");
    }

    #[test]
    fn callers_of_procedure_via_direct_call() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit(
            1,
            "MyCU",
            vec![regular_method("Caller"), regular_method("Callee")],
        )]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let mut cg = CallGraph::build_from_insight(&graph);

        let caller_key = NodeKey::Procedure(
            ObjectKind::Codeunit,
            "mycu".to_string(),
            "caller".to_string(),
        );
        let callee_key = NodeKey::Procedure(
            ObjectKind::Codeunit,
            "mycu".to_string(),
            "callee".to_string(),
        );

        let caller_id = CallGraph::node_id_for(&graph, &caller_key).expect("caller");
        let callee_id = CallGraph::node_id_for(&graph, &callee_key).expect("callee");

        cg.add_direct_call(caller_id, callee_id);

        let callers = cg.callers_of(callee_id);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].from, caller_id);
        assert_eq!(callers[0].kind, EdgeKind::DirectCall);
    }

    #[test]
    fn trigger_invocation_edge() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit(
            1,
            "MyCU",
            vec![regular_method("OnValidate"), regular_method("Validate")],
        )]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let mut cg = CallGraph::build_from_insight(&graph);

        let trigger_key = NodeKey::Procedure(
            ObjectKind::Codeunit,
            "mycu".to_string(),
            "onvalidate".to_string(),
        );
        let proc_key = NodeKey::Procedure(
            ObjectKind::Codeunit,
            "mycu".to_string(),
            "validate".to_string(),
        );

        let trigger_id = CallGraph::node_id_for(&graph, &trigger_key).unwrap();
        let proc_id = CallGraph::node_id_for(&graph, &proc_key).unwrap();

        cg.add_trigger_invocation(trigger_id, proc_id);

        let callees = cg.callees_of(trigger_id);
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].kind, EdgeKind::TriggerInvocation);

        let callers = cg.callers_of(proc_id);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].kind, EdgeKind::TriggerInvocation);
    }

    #[test]
    fn duplicate_edges_not_inserted() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit(
            1,
            "MyCU",
            vec![regular_method("A"), regular_method("B")],
        )]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let mut cg = CallGraph::build_from_insight(&graph);

        let a_key = NodeKey::Procedure(ObjectKind::Codeunit, "mycu".to_string(), "a".to_string());
        let b_key = NodeKey::Procedure(ObjectKind::Codeunit, "mycu".to_string(), "b".to_string());

        let a_id = CallGraph::node_id_for(&graph, &a_key).unwrap();
        let b_id = CallGraph::node_id_for(&graph, &b_key).unwrap();

        cg.add_direct_call(a_id, b_id);
        cg.add_direct_call(a_id, b_id); // duplicate
        cg.add_direct_call(a_id, b_id); // duplicate

        assert_eq!(cg.callees_of(a_id).len(), 1);
        assert_eq!(cg.callers_of(b_id).len(), 1);
    }

    #[test]
    fn node_info_available_for_all_nodes() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(1, "Publisher", vec![integration_event("OnPost")]),
            make_codeunit(
                2,
                "Sub",
                vec![event_subscriber(
                    "Handle",
                    "Codeunit",
                    "Publisher",
                    "OnPost",
                )],
            ),
        ]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&graph);

        for id in cg.node_ids() {
            let info = cg.node_info(id).expect("every node has info");
            assert!(!info.name.is_empty());
            assert!(!info.node_type.is_empty());
        }
    }

    #[test]
    fn multiple_subscribers_to_same_event() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(1, "Publisher", vec![integration_event("OnRelease")]),
            make_codeunit(
                2,
                "SubA",
                vec![event_subscriber("H1", "Codeunit", "Publisher", "OnRelease")],
            ),
            make_codeunit(
                3,
                "SubB",
                vec![event_subscriber("H2", "Codeunit", "Publisher", "OnRelease")],
            ),
            make_codeunit(
                4,
                "SubC",
                vec![event_subscriber("H3", "Codeunit", "Publisher", "OnRelease")],
            ),
        ]);

        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let cg = CallGraph::build_from_insight(&graph);

        let event_key = NodeKey::Event(
            ObjectKind::Codeunit,
            "publisher".to_string(),
            "onrelease".to_string(),
        );
        let event_id = CallGraph::node_id_for(&graph, &event_key).unwrap();
        assert_eq!(cg.subscribers_of(event_id).len(), 3);
    }

    #[test]
    fn record_trigger_edge() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(1, "PostCU", vec![regular_method("DoPost")]),
            make_codeunit(2, "Events", vec![integration_event("OnBeforeInsertEvent")]),
        ]);
        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let mut cg = CallGraph::build_from_insight(&graph);

        let proc_key = NodeKey::Procedure(ObjectKind::Codeunit, "postcu".into(), "dopost".into());
        let event_key = NodeKey::Event(
            ObjectKind::Codeunit,
            "events".into(),
            "onbeforeinsertevent".into(),
        );
        let proc_id = CallGraph::node_id_for(&graph, &proc_key).unwrap();
        let event_id = CallGraph::node_id_for(&graph, &event_key).unwrap();

        cg.add_trigger(proc_id, event_id);
        let callees = cg.callees_of(proc_id);
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].kind, EdgeKind::RecordTrigger);
    }

    #[test]
    fn remove_edges_from_node() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit(
            1,
            "MyCU",
            vec![
                regular_method("A"),
                regular_method("B"),
                regular_method("C"),
            ],
        )]);
        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let mut cg = CallGraph::build_from_insight(&graph);

        let a = CallGraph::node_id_for(
            &graph,
            &NodeKey::Procedure(ObjectKind::Codeunit, "mycu".into(), "a".into()),
        )
        .unwrap();
        let b = CallGraph::node_id_for(
            &graph,
            &NodeKey::Procedure(ObjectKind::Codeunit, "mycu".into(), "b".into()),
        )
        .unwrap();
        let c = CallGraph::node_id_for(
            &graph,
            &NodeKey::Procedure(ObjectKind::Codeunit, "mycu".into(), "c".into()),
        )
        .unwrap();

        cg.add_direct_call(a, b);
        cg.add_direct_call(a, c);
        cg.add_direct_call(b, c);
        assert_eq!(cg.edge_count(), 3);

        cg.remove_edges_from(a);
        assert_eq!(cg.edge_count(), 1);
        assert_eq!(cg.callees_of(a).len(), 0);
        assert_eq!(cg.callers_of(b).len(), 0);
        assert_eq!(cg.callers_of(c).len(), 1);
    }

    /// Reverse reachability must follow a transitive chain A → B → H and
    /// must NOT pull in unrelated nodes. Seeding with H returns {H, B, A};
    /// seeding with an unrelated node U returns only {U}.
    #[test]
    fn reachable_callers_transitive_chain() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit(
            1,
            "MyCU",
            vec![
                regular_method("A"),
                regular_method("B"),
                regular_method("H"),
                regular_method("U"),
            ],
        )]);
        let mut graph = InsightGraph::new();
        graph.build_from_index(&index);
        let mut cg = CallGraph::build_from_insight(&graph);

        let id = |m: &str| {
            CallGraph::node_id_for(
                &graph,
                &NodeKey::Procedure(ObjectKind::Codeunit, "mycu".into(), m.into()),
            )
            .unwrap()
        };
        let (a, b, h, u) = (id("a"), id("b"), id("h"), id("u"));

        // A → B → H ; U stands alone.
        cg.add_direct_call(a, b);
        cg.add_direct_call(b, h);

        let reach_h = cg.reachable_callers([h]);
        assert!(reach_h.contains(&h), "seed itself is reachable");
        assert!(reach_h.contains(&b), "direct caller B reaches H");
        assert!(reach_h.contains(&a), "transitive caller A reaches H");
        assert!(!reach_h.contains(&u), "unrelated U does not reach H");
        assert_eq!(reach_h.len(), 3);

        // Seeding an unrelated node returns only itself.
        let reach_u = cg.reachable_callers([u]);
        assert_eq!(reach_u.len(), 1);
        assert!(reach_u.contains(&u));

        // Multiple seeds union their ancestors without double-counting.
        let reach_both = cg.reachable_callers([h, u]);
        assert_eq!(reach_both.len(), 4);
    }
}
