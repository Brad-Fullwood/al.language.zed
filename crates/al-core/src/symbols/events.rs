//! Event discovery: find event publishers and subscribers across all objects.
//!
//! In AL, events are declared via method attributes:
//! - `[IntegrationEvent]` — publisher, can be subscribed to across apps
//! - `[BusinessEvent]` — publisher, semantically "business logic happened"
//! - `[EventSubscriber]` — subscriber, with arguments specifying the target

use std::sync::Arc;

use super::index::SymbolIndex;
use super::model::{MethodSymbol, SymbolEntry};

/// An event publisher found in the symbol index.
#[derive(Debug, Clone)]
pub struct EventPublisher {
    /// The object containing this event.
    pub object: Arc<SymbolEntry>,
    /// The method that publishes the event.
    pub method: MethodSymbol,
    /// The event type (IntegrationEvent or BusinessEvent).
    pub event_type: EventType,
}

/// An event subscriber found in the symbol index.
#[derive(Debug, Clone)]
pub struct EventSubscriber {
    /// The object containing this subscriber.
    pub object: Arc<SymbolEntry>,
    /// The subscriber method.
    pub method: MethodSymbol,
    /// The object type being subscribed to (from attribute argument).
    pub target_object_type: String,
    /// The object name being subscribed to (from attribute argument).
    pub target_object_name: String,
    /// The event name being subscribed to (from attribute argument).
    pub target_event_name: String,
}

/// Type of event publisher.
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

/// Find all event publishers and subscribers matching a name pattern.
///
/// The `query` is matched case-insensitively against:
/// - Publisher event method names
/// - Subscriber target event names
/// - Object names containing events
pub fn get_events(index: &SymbolIndex, query: &str) -> EventResults {
    let query_lower = query.to_lowercase();
    let mut publishers = Vec::new();
    let mut subscribers = Vec::new();

    // Scan all objects for event attributes
    for entry in index.all_entries() {
        for method in &entry.methods {
            for attr in &method.attributes {
                match attr.name.as_str() {
                    "IntegrationEvent" => {
                        if matches_event_query(&entry.name, &method.name, &query_lower) {
                            publishers.push(EventPublisher {
                                object: Arc::clone(&entry),
                                method: method.clone(),
                                event_type: EventType::Integration,
                            });
                        }
                    }
                    "BusinessEvent" => {
                        if matches_event_query(&entry.name, &method.name, &query_lower) {
                            publishers.push(EventPublisher {
                                object: Arc::clone(&entry),
                                method: method.clone(),
                                event_type: EventType::Business,
                            });
                        }
                    }
                    "EventSubscriber" => {
                        let (target_type, target_name, target_event) =
                            parse_subscriber_args(&attr.arguments);
                        if matches_subscriber_query(
                            &entry.name,
                            &method.name,
                            &target_name,
                            &target_event,
                            &query_lower,
                        ) {
                            subscribers.push(EventSubscriber {
                                object: Arc::clone(&entry),
                                method: method.clone(),
                                target_object_type: target_type,
                                target_object_name: target_name,
                                target_event_name: target_event,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    EventResults {
        publishers,
        subscribers,
    }
}

/// Results from an event search.
#[derive(Debug, Clone)]
pub struct EventResults {
    pub publishers: Vec<EventPublisher>,
    pub subscribers: Vec<EventSubscriber>,
}

/// Check if a publisher matches the query.
fn matches_event_query(object_name: &str, method_name: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    object_name.to_lowercase().contains(query_lower)
        || method_name.to_lowercase().contains(query_lower)
}

/// Check if a subscriber matches the query.
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
            // Remove "Codeunit::" or "Table::" prefix and quotes
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
    use crate::symbols::model::*;
    use super::*;

    fn make_codeunit_with_events() -> Vec<SymbolEntry> {
        vec![
            SymbolEntry {
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
                variables: Vec::new(),
            },
            SymbolEntry {
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
        assert_eq!(results.publishers.len(), 2); // 1 integration + 1 business
        assert_eq!(results.subscribers.len(), 1);
    }

    #[test]
    fn find_by_object_name() {
        let index = SymbolIndex::new();
        index.add_entries(&make_codeunit_with_events());

        let results = get_events(&index, "Sales Event Publisher");
        // Both publisher methods + subscriber (target matches)
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
