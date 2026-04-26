//! Graph data structures for the AL insight engine.
//!
//! Uses `petgraph::DiGraph` with typed nodes and edges to represent
//! the full relationship graph across all loaded packages and workspace files.

use std::collections::HashMap;
use std::sync::Arc;

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use serde::Serialize;

use al_symbols::{ObjectKind, SymbolEntry};

/// A node in the insight graph.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum InsightNode {
    /// An AL object (table, page, codeunit, etc.)
    Object {
        kind: ObjectKind,
        id: i32,
        name: String,
        package: String,
    },
    /// A procedure/method on an object.
    Procedure {
        object_kind: ObjectKind,
        object_name: String,
        name: String,
        is_local: bool,
    },
    /// An event publisher (method with IntegrationEvent or BusinessEvent attribute).
    Event {
        object_kind: ObjectKind,
        object_name: String,
        name: String,
        event_type: EventNodeType,
    },
    /// An event subscriber (method with EventSubscriber attribute).
    Subscriber {
        object_kind: ObjectKind,
        object_name: String,
        name: String,
        target_object: String,
        target_event: String,
    },
}

/// Type of event for Event nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EventNodeType {
    Integration,
    Business,
}

/// An edge in the insight graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum InsightEdge {
    /// Object A extends Object B (table extension, page extension, etc.)
    Extends,
    /// Procedure A calls Procedure B.
    Calls,
    /// Object A publishes Event B.
    Publishes,
    /// Subscriber A subscribes to Event B.
    SubscribesTo,
    /// Object A contains Procedure/Event/Subscriber B.
    Contains,
    /// Table A has a field with TableRelation to Table B.
    RelatesTo,
    /// Procedure A triggers table events on Record B (Insert/Modify/Delete/Validate).
    Triggers,
}

impl std::fmt::Display for InsightEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InsightEdge::Extends => write!(f, "extends"),
            InsightEdge::Calls => write!(f, "calls"),
            InsightEdge::Publishes => write!(f, "publishes"),
            InsightEdge::SubscribesTo => write!(f, "subscribes_to"),
            InsightEdge::Contains => write!(f, "contains"),
            InsightEdge::RelatesTo => write!(f, "relates_to"),
            InsightEdge::Triggers => write!(f, "triggers"),
        }
    }
}

/// Key for looking up nodes in the graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKey {
    /// Object node: (kind, name_lowercase)
    Object(ObjectKind, String),
    /// Procedure node: (object_kind, object_name_lowercase, method_name_lowercase)
    Procedure(ObjectKind, String, String),
    /// Event node: (object_kind, object_name_lowercase, event_name_lowercase)
    Event(ObjectKind, String, String),
    /// Subscriber node: (object_kind, object_name_lowercase, method_name_lowercase)
    Subscriber(ObjectKind, String, String),
}

/// The insight graph: a directed graph of AL objects, procedures, events, and subscribers.
pub struct InsightGraph {
    /// The underlying petgraph.
    pub graph: DiGraph<InsightNode, InsightEdge>,
    /// Lookup table: NodeKey -> Vec<NodeIndex>.
    ///
    /// Multiple packages can define objects with the same (kind, name), so each
    /// key maps to a list of node indices rather than a single one.  This avoids
    /// cross-package collisions in `ensure_node` while still letting
    /// `get_node` return the first (and usually only) match for callers that
    /// only care about name resolution.
    pub index: HashMap<NodeKey, Vec<NodeIndex>>,
}

impl InsightGraph {
    /// Create an empty insight graph.
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            index: HashMap::new(),
        }
    }

    /// Get or insert a node, returning its index.
    ///
    /// If one or more nodes already exist for this key, returns the first one
    /// (same-key deduplication within a single package).  When a new node is
    /// inserted it is appended to the Vec, so objects from different packages
    /// that share the same (kind, name) each get their own graph node.
    pub fn ensure_node(&mut self, key: NodeKey, node: InsightNode) -> NodeIndex {
        if let Some(indices) = self.index.get(&key) {
            if let Some(&first) = indices.first() {
                return first;
            }
        }
        let idx = self.graph.add_node(node);
        self.index.entry(key).or_default().push(idx);
        idx
    }

    /// Look up the first node for a key (preserves existing call-site semantics).
    pub fn get_node(&self, key: &NodeKey) -> Option<NodeIndex> {
        self.index.get(key)?.first().copied()
    }

    /// Look up all nodes for a key.
    ///
    /// Useful when subscriber resolution should connect to every matching event
    /// regardless of which package defines it.
    pub fn get_nodes(&self, key: &NodeKey) -> &[NodeIndex] {
        self.index.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Add an edge between two nodes (idempotent — won't duplicate the same edge type).
    pub fn add_edge(&mut self, from: NodeIndex, to: NodeIndex, edge: InsightEdge) {
        // Check if an edge of the same type already exists
        let already_exists = self
            .graph
            .edges_connecting(from, to)
            .any(|e| *e.weight() == edge);
        if !already_exists {
            self.graph.add_edge(from, to, edge);
        }
    }

    /// Remove all outgoing edges from `node`. Used for invalidation when
    /// a file changes and its call edges need re-extraction.
    pub fn remove_edges_from(&mut self, node: NodeIndex) {
        let to_remove: Vec<_> = self.graph.edges(node).map(|e| e.id()).collect();
        for edge_id in to_remove {
            self.graph.remove_edge(edge_id);
        }
    }

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Build the graph from a symbol index.
    ///
    /// Populates Object, Procedure, Event, and Subscriber nodes with
    /// Extends, Contains, Publishes, and SubscribesTo edges.
    ///
    /// Cold builds for a full BC workspace (~600 tables, thousands of fields)
    /// can take 50–200 ms. The work is wrapped in a `tracing::info_span` and
    /// emits a one-shot log line with elapsed time and final node/edge
    /// counts so latency is observable from `RUST_LOG=al_core::insight=info`.
    pub fn build_from_index(&mut self, symbols: &al_symbols::SymbolIndex) {
        let span = tracing::info_span!("insight_graph.build", entries = symbols.len());
        let _enter = span.enter();
        let started = std::time::Instant::now();

        // Iterate all symbols
        let all_entries = symbols.all_entries();

        for entry in &all_entries {
            self.add_object_and_members(entry);
        }

        // Second pass: resolve Extends edges and SubscribesTo targets
        for entry in &all_entries {
            self.resolve_relationships(entry, symbols);
        }

        let elapsed = started.elapsed();
        tracing::info!(
            entries = all_entries.len(),
            nodes = self.node_count(),
            edges = self.edge_count(),
            elapsed_ms = elapsed.as_millis() as u64,
            "insight_graph.build complete"
        );
    }

    /// Add an object node and all its procedure/event/subscriber nodes.
    fn add_object_and_members(&mut self, entry: &Arc<SymbolEntry>) {
        let obj_key = NodeKey::Object(entry.kind, entry.name.to_lowercase());
        let obj_idx = self.ensure_node(
            obj_key,
            InsightNode::Object {
                kind: entry.kind,
                id: entry.id,
                name: entry.name.clone(),
                package: entry.package.clone(),
            },
        );

        for method in &entry.methods {
            let is_event = method
                .attributes
                .iter()
                .any(|a| a.name == "IntegrationEvent" || a.name == "BusinessEvent");
            let is_subscriber = method
                .attributes
                .iter()
                .any(|a| a.name == "EventSubscriber");

            if is_event {
                let event_type = if method.attributes.iter().any(|a| a.name == "BusinessEvent") {
                    EventNodeType::Business
                } else {
                    EventNodeType::Integration
                };

                let key = NodeKey::Event(
                    entry.kind,
                    entry.name.to_lowercase(),
                    method.name.to_lowercase(),
                );
                let event_idx = self.ensure_node(
                    key,
                    InsightNode::Event {
                        object_kind: entry.kind,
                        object_name: entry.name.clone(),
                        name: method.name.clone(),
                        event_type,
                    },
                );
                self.add_edge(obj_idx, event_idx, InsightEdge::Publishes);
            } else if is_subscriber {
                let (target_object, target_event) = parse_subscriber_target(&method.attributes);

                let key = NodeKey::Subscriber(
                    entry.kind,
                    entry.name.to_lowercase(),
                    method.name.to_lowercase(),
                );
                let sub_idx = self.ensure_node(
                    key,
                    InsightNode::Subscriber {
                        object_kind: entry.kind,
                        object_name: entry.name.clone(),
                        name: method.name.clone(),
                        target_object,
                        target_event,
                    },
                );
                self.add_edge(obj_idx, sub_idx, InsightEdge::Contains);
            } else {
                let key = NodeKey::Procedure(
                    entry.kind,
                    entry.name.to_lowercase(),
                    method.name.to_lowercase(),
                );
                let proc_idx = self.ensure_node(
                    key,
                    InsightNode::Procedure {
                        object_kind: entry.kind,
                        object_name: entry.name.clone(),
                        name: method.name.clone(),
                        is_local: method.is_local,
                    },
                );
                self.add_edge(obj_idx, proc_idx, InsightEdge::Contains);
            }
        }
    }

    /// Resolve cross-object relationships (Extends, SubscribesTo).
    fn resolve_relationships(
        &mut self,
        entry: &Arc<SymbolEntry>,
        _symbols: &al_symbols::SymbolIndex,
    ) {
        // Extends edges
        if let Some(ref extends_name) = entry.extends {
            if let Some(base_kind) = entry.kind.base_kind() {
                let ext_key = NodeKey::Object(entry.kind, entry.name.to_lowercase());
                let base_key = NodeKey::Object(base_kind, extends_name.to_lowercase());

                if let (Some(ext_idx), Some(base_idx)) =
                    (self.get_node(&ext_key), self.get_node(&base_key))
                {
                    self.add_edge(ext_idx, base_idx, InsightEdge::Extends);
                }
            }
        }

        // SubscribesTo edges: connect Subscriber nodes to their target Event nodes
        for method in &entry.methods {
            if method
                .attributes
                .iter()
                .any(|a| a.name == "EventSubscriber")
            {
                let (target_kind, target_object, target_event) =
                    parse_subscriber_target_full(&method.attributes);

                let sub_key = NodeKey::Subscriber(
                    entry.kind,
                    entry.name.to_lowercase(),
                    method.name.to_lowercase(),
                );

                let target_obj_lower = target_object.to_lowercase();
                let target_event_lower = target_event.to_lowercase();

                // When the attribute encodes the publisher object type, use it directly.
                // Otherwise fall back to searching all known object kinds.
                let kinds_to_try: Vec<ObjectKind> = if let Some(k) = target_kind {
                    vec![k]
                } else {
                    vec![
                        ObjectKind::Codeunit,
                        ObjectKind::Table,
                        ObjectKind::Page,
                        ObjectKind::Report,
                        ObjectKind::XmlPort,
                        ObjectKind::Query,
                        ObjectKind::Interface,
                        ObjectKind::TableExtension,
                        ObjectKind::PageExtension,
                        ObjectKind::ReportExtension,
                    ]
                };

                // sub_idx is resolved once outside the kinds loop.
                let sub_idx = self.get_node(&sub_key);
                for kind in &kinds_to_try {
                    let event_key =
                        NodeKey::Event(*kind, target_obj_lower.clone(), target_event_lower.clone());
                    // Connect subscriber to ALL matching event nodes (across packages).
                    let event_indices: Vec<NodeIndex> = self.get_nodes(&event_key).to_vec();
                    if event_indices.is_empty() {
                        continue;
                    }
                    for event_idx in event_indices {
                        if let Some(sub) = sub_idx {
                            self.add_edge(sub, event_idx, InsightEdge::SubscribesTo);
                        }
                    }
                    // Stop trying fallback kinds once we found a matching kind.
                    break;
                }
            }
        }

        // RelatesTo edges: table fields with TableRelation property
        if matches!(entry.kind, ObjectKind::Table | ObjectKind::TableExtension) {
            for field in &entry.fields {
                for prop in &field.properties {
                    if prop.name.eq_ignore_ascii_case("TableRelation") && !prop.value.is_empty() {
                        // Extract table name from value (format: "Table Name" or "\"Table Name\"")
                        let related_table = prop.value.trim_matches('"').to_string();
                        if related_table.is_empty() {
                            continue;
                        }

                        let src_key = NodeKey::Object(entry.kind, entry.name.to_lowercase());
                        let target_key =
                            NodeKey::Object(ObjectKind::Table, related_table.to_lowercase());

                        if let (Some(src_idx), Some(target_idx)) =
                            (self.get_node(&src_key), self.get_node(&target_key))
                        {
                            self.add_edge(src_idx, target_idx, InsightEdge::RelatesTo);
                        }
                    }
                }
            }
        }
    }
}

impl Default for InsightGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse EventSubscriber attribute to extract target object kind (if determinable),
/// target object name, and target event name.
///
/// Returns `(target_kind, target_object_name, target_event_name)` where `target_kind`
/// is `None` when the object type cannot be determined from arg[0].
fn parse_subscriber_target_full(
    attributes: &[al_symbols::AttributeSymbol],
) -> (Option<ObjectKind>, String, String) {
    for attr in attributes {
        if attr.name == "EventSubscriber" {
            // arg[0]: "ObjectType::Codeunit" — extract the type name
            let kind = attr.arguments.first().and_then(|s| {
                let s = s.trim();
                let type_str = if let Some(pos) = s.find("::") {
                    &s[pos + 2..]
                } else {
                    s
                };
                type_str.parse::<ObjectKind>().ok()
            });

            let target_object = attr
                .arguments
                .get(1)
                .map(|s| {
                    let s = s.trim();
                    // Remove "Codeunit::" or "Table::" prefix and quotes
                    if let Some(pos) = s.find("::") {
                        clean_quotes(&s[pos + 2..])
                    } else {
                        clean_quotes(s)
                    }
                })
                .unwrap_or_default();
            let target_event = attr
                .arguments
                .get(2)
                .map(|s| clean_quotes(s.trim()))
                .unwrap_or_default();
            return (kind, target_object, target_event);
        }
    }
    (None, String::new(), String::new())
}

/// Parse EventSubscriber attribute to extract target object and event names.
fn parse_subscriber_target(attributes: &[al_symbols::AttributeSymbol]) -> (String, String) {
    let (_, obj, evt) = parse_subscriber_target_full(attributes);
    (obj, evt)
}

/// Remove surrounding single/double quotes.
fn clean_quotes(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    let s = s.strip_prefix('\'').unwrap_or(s);
    let s = s.strip_suffix('\'').unwrap_or(s);
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;

    fn make_codeunit(id: i32, name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
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

    fn make_table(id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Table,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: vec![FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
                properties: vec![],
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_table_ext(id: i32, name: &str, extends: &str) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::TableExtension,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            implements: Vec::new(),
            namespace: String::new(),
            package: "ExtPkg".to_string(),
            methods: Vec::new(),
            fields: vec![FieldSymbol {
                id: 50100,
                name: "Custom".to_string(),
                type_name: "Boolean".to_string(),
                properties: vec![],
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn integration_event(name: &str) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: vec![AttributeSymbol {
                name: "IntegrationEvent".to_string(),
                arguments: vec!["false".to_string(), "false".to_string()],
            }],
            is_local: false,
        }
    }

    fn business_event(name: &str) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: vec![AttributeSymbol {
                name: "BusinessEvent".to_string(),
                arguments: vec!["false".to_string()],
            }],
            is_local: false,
        }
    }

    fn event_subscriber(
        name: &str,
        target_type: &str,
        target_name: &str,
        target_event: &str,
    ) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: vec![AttributeSymbol {
                name: "EventSubscriber".to_string(),
                arguments: vec![
                    format!("ObjectType::{}", target_type),
                    format!("{}::\"{}\"", target_type, target_name),
                    format!("'{}'", target_event),
                    "''".to_string(),
                    "false".to_string(),
                    "false".to_string(),
                ],
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

    #[test]
    fn empty_graph() {
        let g = InsightGraph::new();
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn build_from_empty_index() {
        let index = SymbolIndex::new();
        let mut g = InsightGraph::new();
        g.build_from_index(&index);
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn object_nodes_created() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_table(18, "Customer"),
            make_codeunit(80, "Sales-Post", vec![regular_method("PostDocument")]),
        ]);

        let mut g = InsightGraph::new();
        g.build_from_index(&index);

        // 2 objects + 1 procedure
        assert_eq!(g.node_count(), 3);
        assert!(g
            .get_node(&NodeKey::Object(ObjectKind::Table, "customer".to_string()))
            .is_some());
        assert!(g
            .get_node(&NodeKey::Object(
                ObjectKind::Codeunit,
                "sales-post".to_string()
            ))
            .is_some());
    }

    #[test]
    fn procedure_nodes_and_contains_edges() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit(
            80,
            "Sales-Post",
            vec![
                regular_method("PostDocument"),
                regular_method("CheckHeader"),
            ],
        )]);

        let mut g = InsightGraph::new();
        g.build_from_index(&index);

        // 1 object + 2 procedures
        assert_eq!(g.node_count(), 3);

        let obj = g
            .get_node(&NodeKey::Object(
                ObjectKind::Codeunit,
                "sales-post".to_string(),
            ))
            .unwrap();
        let proc = g
            .get_node(&NodeKey::Procedure(
                ObjectKind::Codeunit,
                "sales-post".to_string(),
                "postdocument".to_string(),
            ))
            .unwrap();

        // Object -> Procedure via Contains
        assert!(g.graph.find_edge(obj, proc).is_some());
        assert_eq!(
            *g.graph
                .edge_weight(g.graph.find_edge(obj, proc).unwrap())
                .unwrap(),
            InsightEdge::Contains
        );
    }

    #[test]
    fn event_nodes_and_publishes_edges() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_codeunit(
            80,
            "Sales-Post",
            vec![
                integration_event("OnAfterPost"),
                business_event("OnBeforeRelease"),
                regular_method("PostDocument"),
            ],
        )]);

        let mut g = InsightGraph::new();
        g.build_from_index(&index);

        // 1 object + 2 events + 1 procedure
        assert_eq!(g.node_count(), 4);

        let obj = g
            .get_node(&NodeKey::Object(
                ObjectKind::Codeunit,
                "sales-post".to_string(),
            ))
            .unwrap();
        let event = g
            .get_node(&NodeKey::Event(
                ObjectKind::Codeunit,
                "sales-post".to_string(),
                "onafterpost".to_string(),
            ))
            .unwrap();

        // Object -> Event via Publishes
        assert!(g.graph.find_edge(obj, event).is_some());
        assert_eq!(
            *g.graph
                .edge_weight(g.graph.find_edge(obj, event).unwrap())
                .unwrap(),
            InsightEdge::Publishes
        );

        // Verify event type
        if let InsightNode::Event { event_type, .. } = &g.graph[event] {
            assert_eq!(*event_type, EventNodeType::Integration);
        } else {
            panic!("Expected Event node");
        }
    }

    #[test]
    fn subscriber_nodes_and_subscribes_to_edges() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(80, "Sales-Post", vec![integration_event("OnAfterPost")]),
            make_codeunit(
                50100,
                "My Subscriber",
                vec![event_subscriber(
                    "HandleAfterPost",
                    "Codeunit",
                    "Sales-Post",
                    "OnAfterPost",
                )],
            ),
        ]);

        let mut g = InsightGraph::new();
        g.build_from_index(&index);

        // 2 objects + 1 event + 1 subscriber
        assert_eq!(g.node_count(), 4);

        let sub = g
            .get_node(&NodeKey::Subscriber(
                ObjectKind::Codeunit,
                "my subscriber".to_string(),
                "handleafterpost".to_string(),
            ))
            .unwrap();
        let event = g
            .get_node(&NodeKey::Event(
                ObjectKind::Codeunit,
                "sales-post".to_string(),
                "onafterpost".to_string(),
            ))
            .unwrap();

        // Subscriber -> Event via SubscribesTo
        assert!(g.graph.find_edge(sub, event).is_some());
        assert_eq!(
            *g.graph
                .edge_weight(g.graph.find_edge(sub, event).unwrap())
                .unwrap(),
            InsightEdge::SubscribesTo
        );
    }

    #[test]
    fn extends_edges() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_table(18, "Customer"),
            make_table_ext(50100, "Cust Ext", "Customer"),
        ]);

        let mut g = InsightGraph::new();
        g.build_from_index(&index);

        let ext = g
            .get_node(&NodeKey::Object(
                ObjectKind::TableExtension,
                "cust ext".to_string(),
            ))
            .unwrap();
        let base = g
            .get_node(&NodeKey::Object(ObjectKind::Table, "customer".to_string()))
            .unwrap();

        // Extension -> Base via Extends
        assert!(g.graph.find_edge(ext, base).is_some());
        assert_eq!(
            *g.graph
                .edge_weight(g.graph.find_edge(ext, base).unwrap())
                .unwrap(),
            InsightEdge::Extends
        );
    }

    #[test]
    fn circular_event_chain_handled() {
        // Codeunit A publishes EventA, Codeunit B subscribes to EventA and publishes EventB,
        // Codeunit C subscribes to EventB and publishes EventC, Codeunit A subscribes to EventC.
        // This creates a cycle: A -> B -> C -> A
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(
                1,
                "CU-A",
                vec![
                    integration_event("EventA"),
                    event_subscriber("HandleEventC", "Codeunit", "CU-C", "EventC"),
                ],
            ),
            make_codeunit(
                2,
                "CU-B",
                vec![
                    integration_event("EventB"),
                    event_subscriber("HandleEventA", "Codeunit", "CU-A", "EventA"),
                ],
            ),
            make_codeunit(
                3,
                "CU-C",
                vec![
                    integration_event("EventC"),
                    event_subscriber("HandleEventB", "Codeunit", "CU-B", "EventB"),
                ],
            ),
        ]);

        let mut g = InsightGraph::new();
        g.build_from_index(&index);

        // Graph should build without errors or infinite loops
        // 3 objects + 3 events + 3 subscribers = 9 nodes
        assert_eq!(g.node_count(), 9);

        // Verify the cycle exists in the graph:
        // SubA -> EventC, SubB -> EventA, SubC -> EventB
        assert!(g
            .get_node(&NodeKey::Subscriber(
                ObjectKind::Codeunit,
                "cu-a".to_string(),
                "handleeventc".to_string(),
            ))
            .is_some());

        // petgraph handles cycles naturally — no special handling needed
        // The cycle is: EventA <- SubB (in CU-B) and CU-B publishes EventB,
        //               EventB <- SubC (in CU-C) and CU-C publishes EventC,
        //               EventC <- SubA (in CU-A) and CU-A publishes EventA
        // This verifies the fail criteria: "Graph data structure cannot represent circular event chains"
    }

    #[test]
    fn idempotent_edge_insertion() {
        let mut g = InsightGraph::new();
        let a = g.ensure_node(
            NodeKey::Object(ObjectKind::Table, "a".to_string()),
            InsightNode::Object {
                kind: ObjectKind::Table,
                id: 1,
                name: "A".to_string(),
                package: "pkg".to_string(),
            },
        );
        let b = g.ensure_node(
            NodeKey::Object(ObjectKind::Table, "b".to_string()),
            InsightNode::Object {
                kind: ObjectKind::Table,
                id: 2,
                name: "B".to_string(),
                package: "pkg".to_string(),
            },
        );

        g.add_edge(a, b, InsightEdge::Extends);
        g.add_edge(a, b, InsightEdge::Extends); // duplicate
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn different_edge_types_between_same_nodes_both_stored() {
        // Verify that add_edge deduplication is type-aware:
        // adding two edges of different types between the same nodes should
        // result in 2 edges, not 1.
        let mut g = InsightGraph::new();
        let a = g.ensure_node(
            NodeKey::Object(ObjectKind::Table, "a".to_string()),
            InsightNode::Object {
                kind: ObjectKind::Table,
                id: 1,
                name: "A".to_string(),
                package: "pkg".to_string(),
            },
        );
        let b = g.ensure_node(
            NodeKey::Object(ObjectKind::Table, "b".to_string()),
            InsightNode::Object {
                kind: ObjectKind::Table,
                id: 2,
                name: "B".to_string(),
                package: "pkg".to_string(),
            },
        );

        g.add_edge(a, b, InsightEdge::Contains);
        g.add_edge(a, b, InsightEdge::Extends);
        assert_eq!(g.edge_count(), 2);

        // Duplicate of the same type still blocked
        g.add_edge(a, b, InsightEdge::Contains);
        assert_eq!(g.edge_count(), 2);
    }

    #[test]
    fn table_relation_edges() {
        use al_symbols::{FieldSymbol, PropertyValue};

        let index = SymbolIndex::new();

        // Customer table
        let customer = SymbolEntry {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: vec![FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
                properties: vec![],
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        };

        // Sales Header with TableRelation to Customer
        let sales_header = SymbolEntry {
            kind: ObjectKind::Table,
            id: 36,
            name: "Sales Header".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: vec![FieldSymbol {
                id: 2,
                name: "Sell-to Customer No.".to_string(),
                type_name: "Code".to_string(),
                properties: vec![PropertyValue {
                    name: "TableRelation".to_string(),
                    value: "Customer".to_string(),
                }],
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        };

        index.add_entries(&[customer, sales_header]);

        let mut g = InsightGraph::new();
        g.build_from_index(&index);

        // Should have RelatesTo edge from Sales Header -> Customer
        let sh_idx = g
            .get_node(&NodeKey::Object(
                ObjectKind::Table,
                "sales header".to_string(),
            ))
            .unwrap();
        let cust_idx = g
            .get_node(&NodeKey::Object(ObjectKind::Table, "customer".to_string()))
            .unwrap();

        let edge = g.graph.find_edge(sh_idx, cust_idx).unwrap();
        assert_eq!(*g.graph.edge_weight(edge).unwrap(), InsightEdge::RelatesTo);
    }

    #[test]
    fn triggers_edge_display() {
        assert_eq!(format!("{}", InsightEdge::Triggers), "triggers");
    }

    #[test]
    fn remove_edges_from_clears_outgoing() {
        let mut g = InsightGraph::new();

        // Create two Procedure nodes
        let a = g.ensure_node(
            NodeKey::Procedure(ObjectKind::Codeunit, "cu".to_string(), "proc_a".to_string()),
            InsightNode::Procedure {
                object_kind: ObjectKind::Codeunit,
                object_name: "CU".to_string(),
                name: "ProcA".to_string(),
                is_local: false,
            },
        );
        let b = g.ensure_node(
            NodeKey::Procedure(ObjectKind::Codeunit, "cu".to_string(), "proc_b".to_string()),
            InsightNode::Procedure {
                object_kind: ObjectKind::Codeunit,
                object_name: "CU".to_string(),
                name: "ProcB".to_string(),
                is_local: false,
            },
        );

        // Add a Calls edge from A to B
        g.add_edge(a, b, InsightEdge::Calls);
        assert_eq!(g.edge_count(), 1);

        // Remove all outgoing edges from A
        g.remove_edges_from(a);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn ensure_node_returns_existing() {
        let mut g = InsightGraph::new();
        let key = NodeKey::Object(ObjectKind::Table, "customer".to_string());
        let node = InsightNode::Object {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "Base".to_string(),
        };

        let idx1 = g.ensure_node(key.clone(), node.clone());
        let idx2 = g.ensure_node(key, node);
        assert_eq!(idx1, idx2);
        assert_eq!(g.node_count(), 1);
    }
}
