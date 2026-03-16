//! Event interception discovery for the AL insight engine.
//!
//! Queries the insight graph to produce a complete map of event publishers
//! and their subscribers. Detects orphan subscribers whose target event
//! does not exist in the workspace.
//!
//! Used by `al subscribers` and `al intercept` queries.

use std::collections::HashMap;

use petgraph::Direction;
use petgraph::visit::EdgeRef;
use serde::Serialize;

use super::graph::{InsightEdge, InsightGraph, InsightNode, NodeKey};

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Location of a published event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublisherInfo {
    pub object_kind: String,
    pub object_name: String,
    pub event_name: String,
    pub event_type: String,
}

/// Location of an event subscriber.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriberInfo {
    pub object_kind: String,
    pub object_name: String,
    pub method_name: String,
}

/// A discovered event with its publisher, all known subscribers, and orphan status.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredEvent {
    /// The event name.
    pub event_name: String,
    /// Publisher information. `None` only for orphan subscribers that reference
    /// a non-existent event — this field will always be populated for real events.
    pub publisher: PublisherInfo,
    /// All subscribers of this event within the workspace.
    pub subscribers: Vec<SubscriberInfo>,
    /// Subscriber count (convenience field for JSON consumers).
    pub subscriber_count: usize,
    /// Whether any subscribers are attached.
    pub has_subscribers: bool,
}

/// A subscriber that references an event with no matching publisher in the workspace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanSubscriber {
    pub object_kind: String,
    pub object_name: String,
    pub method_name: String,
    /// The publisher object name the subscriber targets.
    pub target_object: String,
    /// The event name the subscriber targets.
    pub target_event: String,
}

/// Full result of an event discovery query.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDiscoveryResult {
    /// All events published in the workspace, with their subscribers.
    pub events: Vec<DiscoveredEvent>,
    /// Subscribers that reference events not present in the workspace.
    pub orphan_subscribers: Vec<OrphanSubscriber>,
    /// Total number of events discovered.
    pub total_events: usize,
    /// Total number of orphan subscribers.
    pub total_orphans: usize,
}

// ---------------------------------------------------------------------------
// Discovery function
// ---------------------------------------------------------------------------

/// Discover all events and their subscribers from the insight graph.
///
/// Algorithm:
/// 1. Collect all `Event` nodes as publishers.
/// 2. For each event node, follow incoming `SubscribesTo` edges to collect subscribers.
/// 3. Collect all `Subscriber` nodes that have NO outgoing `SubscribesTo` edge
///    (meaning they point to an event not in the graph) — these are orphans.
///
/// Results are sorted by `(object_name, event_name)` for stable output.
pub fn discover_events(graph: &InsightGraph) -> EventDiscoveryResult {
    // Map from event NodeIndex to subscriber list.
    let mut event_subscribers: HashMap<petgraph::graph::NodeIndex, Vec<SubscriberInfo>> =
        HashMap::new();

    // Collect all Event nodes.
    for (key, indices) in &graph.index {
        if matches!(key, NodeKey::Event(..)) {
            for &idx in indices {
                event_subscribers.entry(idx).or_default();
            }
        }
    }

    // Collect all Subscriber nodes and check whether they are connected.
    let mut orphans: Vec<OrphanSubscriber> = Vec::new();

    for (key, indices) in &graph.index {
        if !matches!(key, NodeKey::Subscriber(..)) {
            continue;
        }
        for &sub_idx in indices {
            let sub_node = &graph.graph[sub_idx];
            let (obj_kind, obj_name, method_name, target_object, target_event) = match sub_node {
                InsightNode::Subscriber {
                    object_kind,
                    object_name,
                    name,
                    target_object,
                    target_event,
                } => (
                    object_kind.to_string(),
                    object_name.clone(),
                    name.clone(),
                    target_object.clone(),
                    target_event.clone(),
                ),
                _ => continue,
            };

            // Find outgoing SubscribesTo edges from this subscriber.
            let subscribed_events: Vec<_> = graph
                .graph
                .edges_directed(sub_idx, Direction::Outgoing)
                .filter(|e| *e.weight() == InsightEdge::SubscribesTo)
                .map(|e| e.target())
                .collect();

            if subscribed_events.is_empty() {
                // Orphan: no matching event found in the graph.
                orphans.push(OrphanSubscriber {
                    object_kind: obj_kind,
                    object_name: obj_name,
                    method_name,
                    target_object,
                    target_event,
                });
            } else {
                // Register this subscriber against each event it subscribes to.
                for event_idx in subscribed_events {
                    event_subscribers
                        .entry(event_idx)
                        .or_default()
                        .push(SubscriberInfo {
                            object_kind: obj_kind.clone(),
                            object_name: obj_name.clone(),
                            method_name: method_name.clone(),
                        });
                }
            }
        }
    }

    // Build DiscoveredEvent list.
    let mut events: Vec<DiscoveredEvent> = Vec::new();

    for (event_idx, mut subs) in event_subscribers {
        let event_node = &graph.graph[event_idx];
        let (obj_kind, obj_name, evt_name, evt_type) = match event_node {
            InsightNode::Event {
                object_kind,
                object_name,
                name,
                event_type,
            } => (
                object_kind.to_string(),
                object_name.clone(),
                name.clone(),
                format!("{:?}", event_type),
            ),
            _ => continue,
        };

        // Sort subscribers by object_name then method_name for stability.
        subs.sort_by(|a, b| {
            a.object_name
                .as_bytes().iter().map(u8::to_ascii_lowercase)
                .cmp(b.object_name.as_bytes().iter().map(u8::to_ascii_lowercase))
                .then_with(|| {
                    a.method_name.as_bytes().iter().map(u8::to_ascii_lowercase)
                        .cmp(b.method_name.as_bytes().iter().map(u8::to_ascii_lowercase))
                })
        });

        let sub_count = subs.len();
        let has_subs = !subs.is_empty();

        events.push(DiscoveredEvent {
            event_name: evt_name,
            publisher: PublisherInfo {
                object_kind: obj_kind,
                object_name: obj_name,
                event_name: String::new(), // filled below
                event_type: evt_type,
            },
            subscribers: subs,
            subscriber_count: sub_count,
            has_subscribers: has_subs,
        });
    }

    // Fix up publisher.event_name (same as event_name on the parent).
    for ev in &mut events {
        ev.publisher.event_name = ev.event_name.clone();
    }

    // Sort events by (publisher object_name, event_name).
    events.sort_by(|a, b| {
        a.publisher
            .object_name.as_bytes().iter().map(u8::to_ascii_lowercase)
            .cmp(b.publisher.object_name.as_bytes().iter().map(u8::to_ascii_lowercase))
            .then_with(|| {
                a.event_name.as_bytes().iter().map(u8::to_ascii_lowercase)
                    .cmp(b.event_name.as_bytes().iter().map(u8::to_ascii_lowercase))
            })
    });

    // Sort orphans by (object_name, method_name).
    orphans.sort_by(|a, b| {
        a.object_name.as_bytes().iter().map(u8::to_ascii_lowercase)
            .cmp(b.object_name.as_bytes().iter().map(u8::to_ascii_lowercase))
            .then_with(|| {
                a.method_name.as_bytes().iter().map(u8::to_ascii_lowercase)
                    .cmp(b.method_name.as_bytes().iter().map(u8::to_ascii_lowercase))
            })
    });

    let total_events = events.len();
    let total_orphans = orphans.len();

    EventDiscoveryResult {
        events,
        orphan_subscribers: orphans,
        total_events,
        total_orphans,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{AttributeSymbol, MethodSymbol, ObjectKind, SymbolEntry, SymbolIndex};

    fn base_entry(kind: ObjectKind, id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id,
            name: name.to_string(),
            extends: None,
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn integration_event_method(name: &str) -> MethodSymbol {
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

    fn business_event_method(name: &str) -> MethodSymbol {
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

    fn subscriber_method(
        name: &str,
        target_kind: &str,
        target_obj: &str,
        target_event: &str,
    ) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: vec![AttributeSymbol {
                name: "EventSubscriber".to_string(),
                arguments: vec![
                    format!("ObjectType::{}", target_kind),
                    format!("{}::\"{}\"", target_kind, target_obj),
                    format!("'{}'", target_event),
                    "''".to_string(),
                    "false".to_string(),
                    "false".to_string(),
                ],
            }],
            is_local: false,
        }
    }

    fn build_graph(entries: &[SymbolEntry]) -> InsightGraph {
        let index = SymbolIndex::new();
        index.add_entries(entries);
        let mut g = InsightGraph::new();
        g.build_from_index(&index);
        g
    }

    // --- empty graph ---

    #[test]
    fn empty_graph_returns_empty_result() {
        let g = InsightGraph::new();
        let result = discover_events(&g);
        assert_eq!(result.total_events, 0);
        assert_eq!(result.total_orphans, 0);
        assert!(result.events.is_empty());
        assert!(result.orphan_subscribers.is_empty());
    }

    // --- single event, no subscribers ---

    #[test]
    fn event_with_no_subscribers() {
        let mut publisher = base_entry(ObjectKind::Codeunit, 80, "Sales-Post");
        publisher.methods = vec![integration_event_method("OnAfterPost")];

        let g = build_graph(&[publisher]);
        let result = discover_events(&g);

        assert_eq!(result.total_events, 1);
        assert_eq!(result.total_orphans, 0);

        let ev = &result.events[0];
        assert_eq!(ev.event_name, "OnAfterPost");
        assert_eq!(ev.publisher.object_name, "Sales-Post");
        assert_eq!(ev.publisher.event_type, "Integration");
        assert!(!ev.has_subscribers);
        assert_eq!(ev.subscriber_count, 0);
    }

    // --- business event type ---

    #[test]
    fn business_event_type_detected() {
        let mut publisher = base_entry(ObjectKind::Codeunit, 80, "Sales-Post");
        publisher.methods = vec![business_event_method("OnBeforeRelease")];

        let g = build_graph(&[publisher]);
        let result = discover_events(&g);

        assert_eq!(result.total_events, 1);
        let ev = &result.events[0];
        assert_eq!(ev.publisher.event_type, "Business");
    }

    // --- single event with one subscriber ---

    #[test]
    fn event_with_one_subscriber() {
        let mut publisher = base_entry(ObjectKind::Codeunit, 80, "Sales-Post");
        publisher.methods = vec![integration_event_method("OnAfterPost")];

        let mut subscriber_cu = base_entry(ObjectKind::Codeunit, 50100, "My Extension");
        subscriber_cu.methods = vec![subscriber_method(
            "HandleAfterPost",
            "Codeunit",
            "Sales-Post",
            "OnAfterPost",
        )];

        let g = build_graph(&[publisher, subscriber_cu]);
        let result = discover_events(&g);

        assert_eq!(result.total_events, 1);
        assert_eq!(result.total_orphans, 0);

        let ev = &result.events[0];
        assert!(ev.has_subscribers);
        assert_eq!(ev.subscriber_count, 1);
        assert_eq!(ev.subscribers[0].object_name, "My Extension");
        assert_eq!(ev.subscribers[0].method_name, "HandleAfterPost");
    }

    // --- multiple subscribers ---

    #[test]
    fn event_with_multiple_subscribers() {
        let mut publisher = base_entry(ObjectKind::Codeunit, 80, "Sales-Post");
        publisher.methods = vec![integration_event_method("OnAfterPost")];

        let mut sub1 = base_entry(ObjectKind::Codeunit, 50100, "Sub-A");
        sub1.methods = vec![subscriber_method(
            "HandleA",
            "Codeunit",
            "Sales-Post",
            "OnAfterPost",
        )];

        let mut sub2 = base_entry(ObjectKind::Codeunit, 50101, "Sub-B");
        sub2.methods = vec![subscriber_method(
            "HandleB",
            "Codeunit",
            "Sales-Post",
            "OnAfterPost",
        )];

        let g = build_graph(&[publisher, sub1, sub2]);
        let result = discover_events(&g);

        assert_eq!(result.total_events, 1);
        let ev = &result.events[0];
        assert_eq!(ev.subscriber_count, 2);
    }

    // --- orphan subscriber ---

    #[test]
    fn orphan_subscriber_detected() {
        // Only the subscriber exists — no matching publisher.
        let mut orphan_cu = base_entry(ObjectKind::Codeunit, 50100, "Orphan Sub");
        orphan_cu.methods = vec![subscriber_method(
            "HandleMissing",
            "Codeunit",
            "NonExistent",
            "OnMissingEvent",
        )];

        let g = build_graph(&[orphan_cu]);
        let result = discover_events(&g);

        assert_eq!(result.total_events, 0);
        assert_eq!(result.total_orphans, 1);

        let orphan = &result.orphan_subscribers[0];
        assert_eq!(orphan.object_name, "Orphan Sub");
        assert_eq!(orphan.method_name, "HandleMissing");
        assert_eq!(orphan.target_object, "NonExistent");
        assert_eq!(orphan.target_event, "OnMissingEvent");
    }

    // --- mix of real events and orphans ---

    #[test]
    fn mix_of_events_and_orphans() {
        let mut publisher = base_entry(ObjectKind::Codeunit, 80, "Sales-Post");
        publisher.methods = vec![integration_event_method("OnAfterPost")];

        let mut real_sub = base_entry(ObjectKind::Codeunit, 50100, "Real Sub");
        real_sub.methods = vec![subscriber_method(
            "HandleAfterPost",
            "Codeunit",
            "Sales-Post",
            "OnAfterPost",
        )];

        let mut orphan_cu = base_entry(ObjectKind::Codeunit, 50101, "Orphan Sub");
        orphan_cu.methods = vec![subscriber_method(
            "HandleGhost",
            "Codeunit",
            "Ghost",
            "OnGhost",
        )];

        let g = build_graph(&[publisher, real_sub, orphan_cu]);
        let result = discover_events(&g);

        assert_eq!(result.total_events, 1);
        assert_eq!(result.total_orphans, 1);
        assert_eq!(result.events[0].subscriber_count, 1);
        assert_eq!(result.orphan_subscribers[0].object_name, "Orphan Sub");
    }

    // --- multiple events, sorted ---

    #[test]
    fn events_sorted_by_publisher_then_event_name() {
        let mut cu_a = base_entry(ObjectKind::Codeunit, 1, "Alpha");
        cu_a.methods = vec![
            integration_event_method("OnZ"),
            integration_event_method("OnA"),
        ];

        let mut cu_b = base_entry(ObjectKind::Codeunit, 2, "Beta");
        cu_b.methods = vec![integration_event_method("OnMiddle")];

        let g = build_graph(&[cu_a, cu_b]);
        let result = discover_events(&g);

        assert_eq!(result.total_events, 3);

        // First two events should belong to "Alpha" (sorted before "Beta")
        let alpha_events: Vec<&DiscoveredEvent> = result
            .events
            .iter()
            .filter(|e| e.publisher.object_name == "Alpha")
            .collect();
        assert_eq!(alpha_events.len(), 2);
        // Within Alpha: OnA before OnZ
        assert_eq!(alpha_events[0].event_name, "OnA");
        assert_eq!(alpha_events[1].event_name, "OnZ");

        // Beta after Alpha
        let beta_idx = result
            .events
            .iter()
            .position(|e| e.publisher.object_name == "Beta")
            .unwrap();
        let alpha_last_idx = result
            .events
            .iter()
            .rposition(|e| e.publisher.object_name == "Alpha")
            .unwrap();
        assert!(beta_idx > alpha_last_idx);
    }

    // --- publisher.event_name filled correctly ---

    #[test]
    fn publisher_event_name_matches_event_name() {
        let mut cu = base_entry(ObjectKind::Codeunit, 1, "CU");
        cu.methods = vec![integration_event_method("OnTestEvent")];

        let g = build_graph(&[cu]);
        let result = discover_events(&g);

        let ev = &result.events[0];
        assert_eq!(ev.publisher.event_name, ev.event_name);
    }

    // --- circular event chains don't infinite-loop ---

    #[test]
    fn circular_event_chain_terminates() {
        // CU-A publishes EventA, subscribes to EventC
        // CU-B publishes EventB, subscribes to EventA
        // CU-C publishes EventC, subscribes to EventB
        let mut cu_a = base_entry(ObjectKind::Codeunit, 1, "CU-A");
        cu_a.methods = vec![
            integration_event_method("EventA"),
            subscriber_method("HandleC", "Codeunit", "CU-C", "EventC"),
        ];

        let mut cu_b = base_entry(ObjectKind::Codeunit, 2, "CU-B");
        cu_b.methods = vec![
            integration_event_method("EventB"),
            subscriber_method("HandleA", "Codeunit", "CU-A", "EventA"),
        ];

        let mut cu_c = base_entry(ObjectKind::Codeunit, 3, "CU-C");
        cu_c.methods = vec![
            integration_event_method("EventC"),
            subscriber_method("HandleB", "Codeunit", "CU-B", "EventB"),
        ];

        let g = build_graph(&[cu_a, cu_b, cu_c]);
        let result = discover_events(&g);

        // 3 events, 3 subscribers (one per event), no orphans
        assert_eq!(result.total_events, 3);
        assert_eq!(result.total_orphans, 0);
        for ev in &result.events {
            assert_eq!(ev.subscriber_count, 1);
        }
    }
}
