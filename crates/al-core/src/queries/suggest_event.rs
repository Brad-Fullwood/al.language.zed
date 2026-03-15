//! AI-assisted event wiring query.
//!
//! Given a natural-language business scenario description, suggests the most relevant
//! event publishers from the symbol index. Uses keyword matching against event names,
//! their parent object names, and parameter types.

use std::sync::Arc;

use al_symbols::{MethodSymbol, SymbolEntry};
use serde::Serialize;

use crate::workspace::Workspace;

/// A suggested event for the given business scenario.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSuggestion {
    /// The event name.
    pub event: String,
    /// The object publishing the event.
    pub obj: String,
    /// Event type: "integration" or "business".
    #[serde(rename = "type")]
    pub event_type: String,
    /// Event parameters.
    pub params: Vec<String>,
    /// Explanation of why this event is relevant.
    pub why: String,
    /// Ready-to-paste EventSubscriber attribute.
    pub example: String,
}

/// Suggest events matching a natural-language business scenario.
///
/// Tokenizes the description into keywords, then scores every event publisher
/// in the symbol index by how many keywords match the event name, object name,
/// or parameter types. Returns the top results sorted by relevance.
pub fn suggest_event(workspace: &Workspace, description: &str) -> Vec<EventSuggestion> {
    let keywords = extract_keywords(description);
    if keywords.is_empty() {
        return Vec::new();
    }

    let all_entries = workspace.symbols.search("", usize::MAX);
    let mut scored: Vec<(EventSuggestion, usize)> = Vec::new();

    for entry in &all_entries {
        for method in &entry.methods {
            let (is_event, event_type) = classify_event(method);
            if !is_event {
                continue;
            }

            let score = score_event(entry, method, &keywords);
            if score > 0 {
                let params: Vec<String> = method
                    .parameters
                    .iter()
                    .map(|p| {
                        if p.is_var {
                            format!("var {}: {}", p.name, p.type_name)
                        } else {
                            format!("{}: {}", p.name, p.type_name)
                        }
                    })
                    .collect();

                let obj_kind = entry.kind.to_string();
                let example = format!(
                    "[EventSubscriber(ObjectType::{}, {}::\"{}\", '{}', '', false, false)]",
                    obj_kind, obj_kind, entry.name, method.name
                );

                let why = generate_why(entry, method, &keywords);

                scored.push((
                    EventSuggestion {
                        event: method.name.clone(),
                        obj: entry.name.clone(),
                        event_type,
                        params,
                        why,
                        example,
                    },
                    score,
                ));
            }
        }
    }

    // Sort by score descending, take top 10
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().take(10).map(|(s, _)| s).collect()
}

/// Extract keywords from a natural-language description.
/// Filters out common stop words and returns lowercase tokens.
fn extract_keywords(description: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "shall",
        "should", "may", "might", "can", "could", "of", "in", "on", "at",
        "to", "for", "with", "by", "from", "as", "into", "through", "during",
        "before", "after", "above", "below", "between", "and", "or", "but",
        "not", "no", "if", "then", "when", "where", "how", "what", "which",
        "who", "that", "this", "it", "i", "we", "you", "they", "my",
    ];

    description
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| !w.is_empty() && w.len() > 1)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// Classify whether a method is an event publisher and what type.
fn classify_event(method: &MethodSymbol) -> (bool, String) {
    for attr in &method.attributes {
        if attr.name == "IntegrationEvent" {
            return (true, "integration".to_string());
        }
        if attr.name == "BusinessEvent" {
            return (true, "business".to_string());
        }
    }
    (false, String::new())
}

/// Score an event against the search keywords.
/// Higher = more relevant.
fn score_event(entry: &Arc<SymbolEntry>, method: &MethodSymbol, keywords: &[String]) -> usize {
    let event_lower = method.name.to_lowercase();
    let obj_lower = entry.name.to_lowercase();

    let mut score = 0;
    for kw in keywords {
        // Event name contains keyword (strongest signal)
        if event_lower.contains(kw.as_str()) {
            score += 3;
        }
        // Object name contains keyword
        if obj_lower.contains(kw.as_str()) {
            score += 2;
        }
        // Parameter types contain keyword
        for param in &method.parameters {
            let type_lower = param.type_name.to_lowercase();
            if type_lower.contains(kw.as_str()) {
                score += 1;
            }
        }
    }
    score
}

/// Generate a human-readable explanation of why this event is relevant.
fn generate_why(entry: &Arc<SymbolEntry>, method: &MethodSymbol, keywords: &[String]) -> String {
    let event_lower = method.name.to_lowercase();
    let obj_lower = entry.name.to_lowercase();

    let mut matched_in_event: Vec<&str> = Vec::new();
    let mut matched_in_obj: Vec<&str> = Vec::new();

    for kw in keywords {
        if event_lower.contains(kw.as_str()) {
            matched_in_event.push(kw);
        }
        if obj_lower.contains(kw.as_str()) {
            matched_in_obj.push(kw);
        }
    }

    let has_var_params = method.parameters.iter().any(|p| p.is_var);

    let mut parts = Vec::new();
    if !matched_in_event.is_empty() {
        parts.push(format!(
            "Event name matches: {}",
            matched_in_event.join(", ")
        ));
    }
    if !matched_in_obj.is_empty() {
        parts.push(format!(
            "Published by '{}'",
            entry.name
        ));
    }
    if has_var_params {
        parts.push("Has var parameters (subscriber can modify behavior)".to_string());
    }

    if parts.is_empty() {
        "Matches query keywords in parameter types".to_string()
    } else {
        parts.join(". ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;

    fn make_codeunit_with_event(
        id: i32,
        name: &str,
        event_name: &str,
        attr_name: &str,
        params: Vec<ParameterSymbol>,
    ) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id,
            name: name.to_string(),
            extends: None,
            package: "Base".to_string(),
            methods: vec![MethodSymbol {
                name: event_name.to_string(),
                parameters: params,
                return_type: None,
                attributes: vec![AttributeSymbol {
                    name: attr_name.to_string(),
                    arguments: vec!["false".to_string(), "false".to_string()],
                }],
                is_local: false,
            }],
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn param(name: &str, type_name: &str, is_var: bool) -> ParameterSymbol {
        ParameterSymbol {
            name: name.to_string(),
            type_name: type_name.to_string(),
            is_var,
        }
    }

    #[test]
    fn suggest_finds_events_by_keyword() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_codeunit_with_event(
                80,
                "Sales-Post",
                "OnBeforePostSalesDoc",
                "IntegrationEvent",
                vec![
                    param("SalesHeader", "Record \"Sales Header\"", true),
                    param("IsHandled", "Boolean", true),
                ],
            ),
            make_codeunit_with_event(
                90,
                "Item Jnl.-Post",
                "OnBeforePostItemJnlLine",
                "IntegrationEvent",
                vec![param("ItemJnlLine", "Record \"Item Journal Line\"", true)],
            ),
        ]);

        let results = suggest_event(&ws, "post sales document");

        assert!(
            !results.is_empty(),
            "Expected at least one suggestion for 'post sales document'"
        );
        assert_eq!(
            results[0].event, "OnBeforePostSalesDoc",
            "Sales-Post event should rank highest"
        );
        assert_eq!(results[0].event_type, "integration");
    }

    #[test]
    fn suggest_returns_empty_for_no_match() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_codeunit_with_event(
            80,
            "Sales-Post",
            "OnBeforePostSalesDoc",
            "IntegrationEvent",
            vec![],
        )]);

        let results = suggest_event(&ws, "xyzzy gibberish");
        assert!(results.is_empty(), "Expected no results for gibberish query");
    }

    #[test]
    fn suggest_empty_description_returns_empty() {
        let ws = Workspace::new();
        let results = suggest_event(&ws, "");
        assert!(results.is_empty());
    }

    #[test]
    fn suggest_generates_example_attribute() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_codeunit_with_event(
            80,
            "Sales-Post",
            "OnAfterPost",
            "IntegrationEvent",
            vec![],
        )]);

        let results = suggest_event(&ws, "post sales");
        assert!(!results.is_empty());
        assert!(
            results[0].example.contains("EventSubscriber"),
            "Example should contain EventSubscriber attribute"
        );
        assert!(
            results[0].example.contains("Sales-Post"),
            "Example should contain the object name"
        );
    }

    #[test]
    fn suggest_ranks_business_events_too() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_codeunit_with_event(
            50,
            "Cust. Check Cr. Limit",
            "OnAfterCheckCreditLimit",
            "BusinessEvent",
            vec![
                param("Customer", "Record Customer", false),
                param("CreditOK", "Boolean", true),
            ],
        )]);

        let results = suggest_event(&ws, "credit limit customer");
        assert!(!results.is_empty());
        assert_eq!(results[0].event_type, "business");
        assert!(results[0].why.contains("Customer") || results[0].why.contains("credit"));
    }

    #[test]
    fn keyword_extraction_filters_stop_words() {
        let keywords = extract_keywords("validate the customer credit limit on a sales order");
        assert!(keywords.contains(&"validate".to_string()));
        assert!(keywords.contains(&"customer".to_string()));
        assert!(keywords.contains(&"credit".to_string()));
        assert!(!keywords.contains(&"the".to_string()));
        assert!(!keywords.contains(&"on".to_string()));
        assert!(!keywords.contains(&"a".to_string()));
    }
}
