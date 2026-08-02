//! Event discovery: find event publishers and subscribers across all objects.
//!
//! In AL, events are declared via method attributes:
//! - `[IntegrationEvent]` — publisher, can be subscribed to across apps
//! - `[BusinessEvent]` — publisher, semantically "business logic happened"
//! - `[EventSubscriber]` — subscriber, with arguments specifying the target

use std::sync::Arc;

use super::index::SymbolIndex;
use super::model::{MethodSymbol, SymbolEntry};

#[derive(Debug, Clone)]
pub struct EventPublisher {
    pub object: Arc<SymbolEntry>,
    pub method: MethodSymbol,
    pub event_type: EventType,
}

#[derive(Debug, Clone)]
pub struct EventSubscriber {
    pub object: Arc<SymbolEntry>,
    pub method: MethodSymbol,
    pub target_object_type: String,
    pub target_object_name: String,
    pub target_event_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    Integration,
    Business,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventType::Integration => write!(f, "IntegrationEvent"),
            EventType::Business => write!(f, "BusinessEvent"),
        }
    }
}

/// One publisher method in the pre-extracted event catalog. The method is
/// referenced by index into `object.methods` so building the catalog never
/// deep-clones symbol payloads.
#[derive(Debug, Clone)]
pub(crate) struct PublisherRef {
    object: Arc<SymbolEntry>,
    method_index: usize,
    event_type: EventType,
}

/// One subscriber method in the pre-extracted event catalog.
#[derive(Debug, Clone)]
pub(crate) struct SubscriberRef {
    object: Arc<SymbolEntry>,
    method_index: usize,
    target_type: String,
    target_name: String,
    target_event: String,
}

/// Publishers and subscribers extracted once per index generation, so event
/// queries filter a small pre-built list instead of scanning every method of
/// every indexed object per call.
#[derive(Debug, Default)]
pub(crate) struct EventCatalog {
    publishers: Vec<PublisherRef>,
    subscribers: Vec<SubscriberRef>,
}

/// Catalog plus the index mutation generation it was built from.
#[derive(Debug)]
pub(crate) struct EventCatalogCache {
    pub(crate) generation: u64,
    pub(crate) catalog: Arc<EventCatalog>,
}

/// Extract every publisher/subscriber from the index. AL attribute names are
/// case-insensitive, and workspace-derived symbols carry the attribute as
/// typed in source, so matching must ignore case.
pub(crate) fn build_event_catalog(index: &SymbolIndex) -> EventCatalog {
    let mut catalog = EventCatalog::default();
    for entry in index.all_entries() {
        for (method_index, method) in entry.methods.iter().enumerate() {
            for attr in &method.attributes {
                if attr.name.eq_ignore_ascii_case("IntegrationEvent") {
                    catalog.publishers.push(PublisherRef {
                        object: Arc::clone(&entry),
                        method_index,
                        event_type: EventType::Integration,
                    });
                } else if attr.name.eq_ignore_ascii_case("BusinessEvent") {
                    catalog.publishers.push(PublisherRef {
                        object: Arc::clone(&entry),
                        method_index,
                        event_type: EventType::Business,
                    });
                } else if attr.name.eq_ignore_ascii_case("EventSubscriber") {
                    let (target_type, target_name, target_event) =
                        parse_subscriber_args(&attr.arguments);
                    catalog.subscribers.push(SubscriberRef {
                        object: Arc::clone(&entry),
                        method_index,
                        target_type,
                        target_name,
                        target_event,
                    });
                }
            }
        }
    }
    // DashMap iteration order is arbitrary; sort so results are deterministic.
    catalog.publishers.sort_by(|a, b| {
        (&a.object.name, a.object.kind, a.object.id, a.method_index).cmp(&(
            &b.object.name,
            b.object.kind,
            b.object.id,
            b.method_index,
        ))
    });
    catalog.subscribers.sort_by(|a, b| {
        (&a.object.name, a.object.kind, a.object.id, a.method_index).cmp(&(
            &b.object.name,
            b.object.kind,
            b.object.id,
            b.method_index,
        ))
    });
    catalog
}

/// Find all event publishers and subscribers matching a name pattern.
///
/// The `query` is matched case-insensitively against:
/// - Publisher event method names
/// - Subscriber target event names
/// - Object names containing events
pub fn get_events(index: &SymbolIndex, query: &str) -> EventResults {
    let query_lower = query.to_lowercase();
    let catalog = index.event_catalog();
    let mut publishers = Vec::new();
    let mut subscribers = Vec::new();

    for publisher in &catalog.publishers {
        let Some(method) = publisher.object.methods.get(publisher.method_index) else {
            continue;
        };
        if matches_event_query(&publisher.object.name, &method.name, &query_lower) {
            publishers.push(EventPublisher {
                object: Arc::clone(&publisher.object),
                method: method.clone(),
                event_type: publisher.event_type,
            });
        }
    }
    for subscriber in &catalog.subscribers {
        let Some(method) = subscriber.object.methods.get(subscriber.method_index) else {
            continue;
        };
        if matches_subscriber_query(
            &subscriber.object.name,
            &method.name,
            &subscriber.target_name,
            &subscriber.target_event,
            &query_lower,
        ) {
            subscribers.push(EventSubscriber {
                object: Arc::clone(&subscriber.object),
                method: method.clone(),
                target_object_type: subscriber.target_type.clone(),
                target_object_name: subscriber.target_name.clone(),
                target_event_name: subscriber.target_event.clone(),
            });
        }
    }

    EventResults {
        publishers,
        subscribers,
    }
}

#[derive(Debug, Clone)]
pub struct EventResults {
    pub publishers: Vec<EventPublisher>,
    pub subscribers: Vec<EventSubscriber>,
}

fn matches_event_query(object_name: &str, method_name: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    object_name.to_lowercase().contains(query_lower)
        || method_name.to_lowercase().contains(query_lower)
}

fn matches_subscriber_query(
    object_name: &str,
    method_name: &str,
    target_name: &str,
    target_event: &str,
    query_lower: &str,
) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    object_name.to_lowercase().contains(query_lower)
        || method_name.to_lowercase().contains(query_lower)
        || target_name.to_lowercase().contains(query_lower)
        || target_event.to_lowercase().contains(query_lower)
}

/// Parse EventSubscriber attribute arguments.
///
/// The attribute format is:
/// `[EventSubscriber(ObjectType::Codeunit, Codeunit::"My Codeunit", 'OnAfterDoSomething', '', false, false)]`
///
/// In SymbolReference.json the arguments array typically has:
/// [0] = object type (e.g., "ObjectType::Codeunit")
/// [1] = object reference (e.g., "Codeunit::\"My Codeunit\"")
/// [2] = event name (e.g., "'OnAfterDoSomething'")
fn parse_subscriber_args(arguments: &[String]) -> (String, String, String) {
    let target_type = arguments
        .first()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let target_name = arguments
        .get(1)
        .map(|s| {
            let cleaned = s.trim().to_string();
            if let Some(pos) = cleaned.find("::") {
                clean_quotes(&cleaned[pos + 2..])
            } else {
                clean_quotes(&cleaned)
            }
        })
        .unwrap_or_default();
    let target_event = arguments
        .get(2)
        .map(|s| clean_quotes(s.trim()))
        .unwrap_or_default();

    (target_type, target_name, target_event)
}

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
    use crate::model::*;

    fn make_codeunit_with_events() -> Vec<SymbolEntry> {
        vec![
            SymbolEntry {
                synthetic: false,
                kind: ObjectKind::Codeunit,
                id: 50100,
                name: "Sales Event Publisher".to_string(),
                extends: None,
                implements: Vec::new(),
                namespace: String::new(),
                package: "TestPkg".to_string(),
                methods: vec![
                    MethodSymbol {
                        name: "OnAfterPostSalesOrder".to_string(),
                        parameters: Vec::new(),
                        return_type: None,
                        attributes: vec![AttributeSymbol {
                            name: "IntegrationEvent".to_string(),
                            arguments: vec!["false".to_string(), "false".to_string()],
                        }],
                        is_local: false,
                    },
                    MethodSymbol {
                        name: "OnBeforeRelease".to_string(),
                        parameters: Vec::new(),
                        return_type: None,
                        attributes: vec![AttributeSymbol {
                            name: "BusinessEvent".to_string(),
                            arguments: vec!["false".to_string()],
                        }],
                        is_local: false,
                    },
                    MethodSymbol {
                        name: "RegularMethod".to_string(),
                        parameters: Vec::new(),
                        return_type: None,
                        attributes: Vec::new(),
                        is_local: false,
                    },
                ],
                fields: Vec::new(),
                controls: Vec::new(),
                enum_values: Vec::new(),
                keys: Vec::new(),
                properties: Vec::new(),
                permissions: Vec::new(),
                variables: Vec::new(),
            },
            SymbolEntry {
                synthetic: false,
                kind: ObjectKind::Codeunit,
                id: 50101,
                name: "Sales Subscriber".to_string(),
                extends: None,
                implements: Vec::new(),
                namespace: String::new(),
                package: "TestPkg".to_string(),
                methods: vec![MethodSymbol {
                    name: "HandlePostSalesOrder".to_string(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: vec![AttributeSymbol {
                        name: "EventSubscriber".to_string(),
                        arguments: vec![
                            "ObjectType::Codeunit".to_string(),
                            "Codeunit::\"Sales Event Publisher\"".to_string(),
                            "'OnAfterPostSalesOrder'".to_string(),
                            "''".to_string(),
                            "false".to_string(),
                            "false".to_string(),
                        ],
                    }],
                    is_local: false,
                }],
                fields: Vec::new(),
                controls: Vec::new(),
                enum_values: Vec::new(),
                keys: Vec::new(),
                properties: Vec::new(),
                permissions: Vec::new(),
                variables: Vec::new(),
            },
        ]
    }

    #[test]
    fn find_publishers_by_method_name() {
        let index = SymbolIndex::new();
        index.add_entries(&make_codeunit_with_events());

        let results = get_events(&index, "PostSalesOrder");
        assert_eq!(results.publishers.len(), 1);
        assert_eq!(results.publishers[0].method.name, "OnAfterPostSalesOrder");
        assert_eq!(results.publishers[0].event_type, EventType::Integration);
    }

    #[test]
    fn find_business_events() {
        let index = SymbolIndex::new();
        index.add_entries(&make_codeunit_with_events());

        let results = get_events(&index, "Release");
        assert_eq!(results.publishers.len(), 1);
        assert_eq!(results.publishers[0].event_type, EventType::Business);
    }

    #[test]
    fn find_subscribers() {
        let index = SymbolIndex::new();
        index.add_entries(&make_codeunit_with_events());

        let results = get_events(&index, "PostSalesOrder");
        assert_eq!(results.subscribers.len(), 1);
        assert_eq!(
            results.subscribers[0].target_object_name,
            "Sales Event Publisher"
        );
        assert_eq!(
            results.subscribers[0].target_event_name,
            "OnAfterPostSalesOrder"
        );
    }

    #[test]
    fn find_all_events() {
        let index = SymbolIndex::new();
        index.add_entries(&make_codeunit_with_events());

        let results = get_events(&index, "");
        assert_eq!(results.publishers.len(), 2);
        assert_eq!(results.subscribers.len(), 1);
    }

    #[test]
    fn find_by_object_name() {
        let index = SymbolIndex::new();
        index.add_entries(&make_codeunit_with_events());

        let results = get_events(&index, "Sales Event Publisher");
        assert_eq!(results.publishers.len(), 2);
        assert_eq!(results.subscribers.len(), 1);
    }

    #[test]
    fn no_results_for_unmatched_query() {
        let index = SymbolIndex::new();
        index.add_entries(&make_codeunit_with_events());

        let results = get_events(&index, "NonexistentEvent");
        assert!(results.publishers.is_empty());
        assert!(results.subscribers.is_empty());
    }

    /// AL attribute names are case-insensitive and the workspace pipeline
    /// preserves as-typed casing (`[integrationevent]`, `[EVENTSUBSCRIBER]`),
    /// so discovery must match attributes case-insensitively.
    #[test]
    fn attribute_matching_is_case_insensitive() {
        let index = SymbolIndex::new();
        index.add_entries(&[SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 50_200,
            name: "Casing Publisher".to_string(),
            package: "TestPkg".to_string(),
            methods: vec![
                MethodSymbol {
                    name: "OnLowercasePublisher".to_string(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: vec![AttributeSymbol {
                        name: "integrationevent".to_string(),
                        arguments: vec!["false".to_string(), "false".to_string()],
                    }],
                    is_local: false,
                },
                MethodSymbol {
                    name: "OnUppercaseBusiness".to_string(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: vec![AttributeSymbol {
                        name: "BUSINESSEVENT".to_string(),
                        arguments: vec!["false".to_string()],
                    }],
                    is_local: false,
                },
                MethodSymbol {
                    name: "HandleMixedCase".to_string(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: vec![AttributeSymbol {
                        name: "eventsubscriber".to_string(),
                        arguments: vec![
                            "ObjectType::Codeunit".to_string(),
                            "Codeunit::\"Casing Publisher\"".to_string(),
                            "'OnLowercasePublisher'".to_string(),
                        ],
                    }],
                    is_local: false,
                },
            ],
            ..Default::default()
        }]);

        let results = get_events(&index, "");
        assert_eq!(
            results.publishers.len(),
            2,
            "non-canonical attribute casing must still classify publishers"
        );
        assert_eq!(results.subscribers.len(), 1);
        assert_eq!(
            results.subscribers[0].target_event_name,
            "OnLowercasePublisher"
        );
    }

    /// The cached catalog must be invalidated when the index mutates.
    #[test]
    fn event_results_track_index_mutations() {
        let index = SymbolIndex::new();
        index.add_entries(&make_codeunit_with_events());
        assert_eq!(get_events(&index, "").publishers.len(), 2);

        index.remove_package_entries("TestPkg");
        assert_eq!(get_events(&index, "").publishers.len(), 0);

        index.add_entries(&make_codeunit_with_events());
        assert_eq!(get_events(&index, "").publishers.len(), 2);
    }

    #[test]
    fn parse_subscriber_arguments() {
        let args = vec![
            "ObjectType::Codeunit".to_string(),
            "Codeunit::\"My Codeunit\"".to_string(),
            "'OnAfterPost'".to_string(),
        ];
        let (typ, name, event) = parse_subscriber_args(&args);
        assert_eq!(typ, "ObjectType::Codeunit");
        assert_eq!(name, "My Codeunit");
        assert_eq!(event, "OnAfterPost");
    }
}
