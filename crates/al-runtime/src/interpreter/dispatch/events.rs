//! Events: calling a publisher (`[IntegrationEvent]`, `[BusinessEvent]`,
//! `[InternalEvent]`) or raising a table event (`OnAfterInsertEvent`,
//! `OnBeforeValidateEvent`, ...) runs every workspace `[EventSubscriber]`
//! bound to it, as BC does.
//!
//! Subscribers receive the publisher's arguments by parameter name and may
//! declare only some of them; a `var` parameter's final value flows back to
//! the publisher. Subscribers in a codeunit with
//! `EventSubscriberInstance = Manual` run only when bound, which the local
//! runtime does not model, so they are skipped here and `BindSubscription`
//! routes a test to live BC.

use std::collections::HashMap;
use std::sync::Arc;

use al_syntax::IdentifierText;

use crate::interpreter::eval_error;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;

use super::frames::collect_params;
use super::workspace_procedure::dispatch_workspace_procedure;
use super::DispatchCtx;

/// One `[EventSubscriber]` procedure.
#[derive(Debug, Clone)]
pub struct Subscriber {
    object: String,
    procedure: String,
    /// Parameter names, lowercased, in declaration order.
    params: Vec<String>,
}

/// `(publisher kind, publisher name, event, element)`, all lowercased.
type EventKey = (String, String, String, String);

/// The workspace's automatic event subscribers by the event they bind.
#[derive(Debug, Default)]
pub struct SubscriberIndex {
    by_event: HashMap<EventKey, Vec<Subscriber>>,
}

impl SubscriberIndex {
    fn build(ctx: &DispatchCtx) -> Self {
        let mut objects_by_id: HashMap<(String, i64), String> = HashMap::new();
        let mut pending: Vec<(Subscriber, String, String, String, String)> = Vec::new();
        for path in ctx.source.iter_paths() {
            let Some((text, tree)) = ctx.source.get_cached_parse(&path) else {
                continue;
            };
            let source = text.as_bytes();
            let mut cursor = tree.root_node().walk();
            for object in tree.root_node().named_children(&mut cursor) {
                if object.kind() != "object_declaration" {
                    continue;
                }
                let kind = object
                    .child_by_field_name("kind")
                    .and_then(|kind| kind.utf8_text(source).ok())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let name = object
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source).ok())
                    .map(|name| name.unquote_identifier().into_owned())
                    .unwrap_or_default();
                if let Some(id) = object
                    .child_by_field_name("id")
                    .and_then(|id| id.utf8_text(source).ok())
                    .and_then(|id| id.trim().parse::<i64>().ok())
                {
                    objects_by_id.insert((kind.clone(), id), name.clone());
                }
                if kind != "codeunit" || manual_instance(object, source) {
                    continue;
                }
                for procedure in declarations(object, "procedure_declaration") {
                    let Some(binding) = subscriber_binding(procedure, source) else {
                        continue;
                    };
                    let Some(procedure_name) = procedure
                        .child_by_field_name("name")
                        .and_then(|name| name.utf8_text(source).ok())
                        .map(|name| name.unquote_identifier().into_owned())
                    else {
                        continue;
                    };
                    let params = collect_params(procedure, source)
                        .into_iter()
                        .map(|param| param.name.to_ascii_lowercase())
                        .collect();
                    let (publisher_kind, publisher, event, element) = binding;
                    pending.push((
                        Subscriber {
                            object: name.clone(),
                            procedure: procedure_name,
                            params,
                        },
                        publisher_kind,
                        publisher,
                        event,
                        element,
                    ));
                }
            }
        }
        let mut by_event: HashMap<EventKey, Vec<Subscriber>> = HashMap::new();
        for (subscriber, kind, publisher, event, element) in pending {
            // `Codeunit::50100` names its publisher by ID.
            let publisher = match publisher.parse::<i64>() {
                Ok(id) => match objects_by_id.get(&(kind.clone(), id)) {
                    Some(name) => name.to_ascii_lowercase(),
                    None => continue,
                },
                Err(_) => publisher,
            };
            by_event
                .entry((kind, publisher, event, element))
                .or_default()
                .push(subscriber);
        }
        Self { by_event }
    }

    fn subscribers(&self, key: &EventKey) -> &[Subscriber] {
        self.by_event
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// The subscriber index, built on first use.
fn index(ctx: &mut DispatchCtx) -> Arc<SubscriberIndex> {
    match &ctx.event_subscribers {
        Some(index) => Arc::clone(index),
        None => {
            let index = Arc::new(SubscriberIndex::build(ctx));
            ctx.event_subscribers = Some(Arc::clone(&index));
            index
        }
    }
}

fn event_key(publisher_kind: &str, publisher: &str, event: &str, element: &str) -> EventKey {
    (
        publisher_kind.to_ascii_lowercase(),
        publisher.unquote_identifier().to_ascii_lowercase(),
        event.to_ascii_lowercase(),
        element.to_ascii_lowercase(),
    )
}

/// Whether any automatic subscriber is bound to the event.
pub(crate) fn has_subscribers(
    publisher_kind: &str,
    publisher: &str,
    event: &str,
    element: &str,
    ctx: &mut DispatchCtx,
) -> bool {
    !index(ctx)
        .subscribers(&event_key(publisher_kind, publisher, event, element))
        .is_empty()
}

/// Run every subscriber of event `event` (with `element`, such as a field
/// name for a validate event) published by `publisher_kind` object
/// `publisher`. `names` and `values` are the publisher's parameters; a
/// subscriber's `var` parameters write their final values back into
/// `values`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn raise(
    publisher_kind: &str,
    publisher: &str,
    event: &str,
    element: &str,
    names: &[&str],
    values: &mut [Value],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<(), Eval> {
    let index = index(ctx);
    let key = event_key(publisher_kind, publisher, event, element);
    for subscriber in index.subscribers(&key) {
        let mut positions = Vec::with_capacity(subscriber.params.len());
        for param in &subscriber.params {
            match names.iter().position(|name| name.eq_ignore_ascii_case(param)) {
                Some(position) => positions.push(position),
                None => {
                    return Err(eval_error(format!(
                        "subscriber {}.{} declares parameter '{param}', which event {event} does not publish",
                        subscriber.object, subscriber.procedure
                    )))
                }
            }
        }
        let args = positions.iter().map(|&at| values[at].clone()).collect();
        match dispatch_workspace_procedure(
            Some(&subscriber.object),
            &subscriber.procedure,
            args,
            stack,
            ctx,
        ) {
            Eval::Normal(_) => {}
            other => return Err(other),
        }
        for (arg_index, value) in std::mem::take(&mut ctx.var_writebacks) {
            if let Some(&at) = positions.get(arg_index) {
                values[at] = value;
            }
        }
    }
    Ok(())
}

/// Whether `procedure` publishes an event, and so raises it when called.
pub(crate) fn is_publisher(procedure: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    attributes(procedure, source).iter().any(|attribute| {
        let name = attribute_name(attribute);
        ["integrationevent", "businessevent", "internalevent"].contains(&name.as_str())
    })
}

/// The event an `[EventSubscriber(...)]` procedure binds, as `(publisher
/// kind, publisher name or ID, event, element)`, lowercased.
fn subscriber_binding(
    procedure: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<(String, String, String, String)> {
    let attribute = attributes(procedure, source)
        .into_iter()
        .find(|attribute| attribute_name(attribute) == "eventsubscriber")?;
    let open = attribute.find('(')?;
    let close = attribute.rfind(')')?;
    let args = split_arguments(attribute.get(open + 1..close)?);
    let [_, publisher, event, element, ..] = args.as_slice() else {
        return None;
    };
    // `Codeunit::"Sales-Post"`, `Database::Customer`, `Page::50100`.
    let (kind, target) = publisher.split_once("::")?;
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "database" => "table".to_string(),
        other => other.to_string(),
    };
    Some((
        kind,
        target.trim().unquote_identifier().to_ascii_lowercase(),
        unquote_literal(event).to_ascii_lowercase(),
        unquote_literal(element).to_ascii_lowercase(),
    ))
}

/// The attribute texts on `procedure` (`[EventSubscriber(...)]`).
fn attributes(procedure: tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = procedure.walk();
    for child in procedure.named_children(&mut cursor) {
        if child.kind().starts_with("attribute") {
            found.push(child.utf8_text(source).unwrap_or_default().to_string());
        }
    }
    // Older trees put attributes before the procedure as siblings.
    let mut sibling = procedure.prev_named_sibling();
    while let Some(node) = sibling.filter(|node| node.kind().starts_with("attribute")) {
        found.push(node.utf8_text(source).unwrap_or_default().to_string());
        sibling = node.prev_named_sibling();
    }
    found
}

/// `[EventSubscriber(...)]` → `eventsubscriber`.
fn attribute_name(attribute: &str) -> String {
    attribute
        .trim()
        .trim_start_matches('[')
        .split(['(', ']'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Split attribute arguments at top-level commas, outside quotes.
fn split_arguments(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0;
    for c in text.chars() {
        match (quote, c) {
            (Some(open), _) if c == open => {
                quote = None;
                current.push(c);
            }
            (Some(_), _) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                current.push(c);
            }
            (None, '(') => {
                depth += 1;
                current.push(c);
            }
            (None, ')') => {
                depth -= 1;
                current.push(c);
            }
            (None, ',') if depth == 0 => args.push(std::mem::take(&mut current).trim().to_string()),
            (None, _) => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

fn unquote_literal(text: &str) -> String {
    text.trim().trim_matches('\'').to_string()
}

/// `EventSubscriberInstance = Manual;` on the codeunit.
fn manual_instance(object: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let Some(body) = object.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let manual = body.named_children(&mut cursor).any(|child| {
        child.kind() == "property_assignment"
            && child
                .utf8_text(source)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .split_whitespace()
                .collect::<String>()
                .starts_with("eventsubscriberinstance=manual")
    });
    manual
}

/// Direct children of the object body of `kind`.
fn declarations<'t>(object: tree_sitter::Node<'t>, kind: &str) -> Vec<tree_sitter::Node<'t>> {
    let Some(body) = object.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let found = body
        .named_children(&mut cursor)
        .filter(|child| child.kind() == kind)
        .collect();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_split_outside_quotes_and_parentheses() {
        assert_eq!(
            split_arguments("ObjectType::Codeunit, Codeunit::\"Sales, Post\", 'On, X', '', false"),
            vec![
                "ObjectType::Codeunit",
                "Codeunit::\"Sales, Post\"",
                "'On, X'",
                "''",
                "false"
            ]
        );
    }

    #[test]
    fn attribute_names_ignore_brackets_and_arguments() {
        assert_eq!(
            attribute_name("[IntegrationEvent(false, false)]"),
            "integrationevent"
        );
        assert_eq!(attribute_name("[InternalEvent]"), "internalevent");
    }
}
