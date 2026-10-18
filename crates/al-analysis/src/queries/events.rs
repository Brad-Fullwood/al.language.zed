//! Events around a procedure: the subscribers of an event publisher and the
//! publisher behind an event subscriber.
//!
//! These back the event code lenses, so a developer reaches the other side of
//! an event from the line they are on.

use al_workspace::Workspace;
use url::Url;

use super::{Location, Position, Range};

/// What a procedure is in the event model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventRole {
    /// Declares an event with `[IntegrationEvent]`, `[BusinessEvent]` or
    /// `[InternalEvent]`.
    Publisher { object: String, event: String },
    /// Handles an event with `[EventSubscriber]`.
    Subscriber {
        object_type: String,
        object: String,
        event: String,
    },
}

/// The event role of the procedure whose declaration contains `position`.
pub fn event_role(text: &str, tree: &tree_sitter::Tree, position: Position) -> Option<EventRole> {
    let point = tree_sitter::Point::new(position.line as usize, position.character as usize);
    let mut node = tree.root_node().descendant_for_point_range(point, point)?;
    while node.kind() != "procedure_declaration" {
        node = node.parent()?;
    }
    procedure_role(node, text.as_bytes())
}

/// The event role of a `procedure_declaration` node.
fn procedure_role(procedure: tree_sitter::Node<'_>, source: &[u8]) -> Option<EventRole> {
    let attributes = al_insight::calls::collect_procedure_attributes(procedure, source);

    if let Some((_, args)) = attributes
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("EventSubscriber"))
    {
        let args = al_insight::calls::extract_attribute_args(args);
        let object_type = args.first().map(|arg| {
            arg.trim()
                .rsplit("::")
                .next()
                .unwrap_or(arg)
                .trim()
                .to_string()
        })?;
        let object = args.get(1).map(|arg| al_syntax::clean_attr_arg(arg))?;
        let event = args.get(2).map(|arg| al_syntax::clean_attr_arg(arg))?;
        return Some(EventRole::Subscriber {
            object_type,
            object,
            event,
        });
    }

    let is_publisher = attributes.iter().any(|(name, _)| {
        ["IntegrationEvent", "BusinessEvent", "InternalEvent"]
            .iter()
            .any(|kind| name.eq_ignore_ascii_case(kind))
    });
    if !is_publisher {
        return None;
    }
    let event = procedure
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok())
        .map(clean_name)?;
    let mut object_node = procedure.parent();
    while let Some(candidate) = object_node {
        if candidate.kind() == "object_declaration" {
            break;
        }
        object_node = candidate.parent();
    }
    let object = object_node?
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok())
        .map(clean_name)?;
    Some(EventRole::Publisher { object, event })
}

/// A name without its quotes.
fn clean_name(name: &str) -> String {
    al_syntax::clean_attr_arg(name)
}

/// Every event subscriber in the workspace's files, by the (object, event)
/// it handles in lower case, each with the name of its procedure. Files
/// without the word `EventSubscriber` are not parsed.
pub fn workspace_subscribers(
    workspace: &Workspace,
) -> std::collections::HashMap<(String, String), Vec<Location>> {
    let candidates: Vec<std::path::PathBuf> = workspace
        .file_index
        .files
        .iter()
        .filter(|entry| {
            entry
                .value()
                .to_ascii_lowercase()
                .contains("eventsubscriber")
        })
        .map(|entry| entry.key().clone())
        .collect();
    let mut subscribers: std::collections::HashMap<(String, String), Vec<Location>> =
        std::collections::HashMap::new();
    for path in candidates {
        let Some((text, tree)) = workspace.file_index.get_cached_parse(&path) else {
            continue;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            continue;
        };
        let source = text.as_bytes();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "procedure_declaration" {
                if let Some(EventRole::Subscriber { object, event, .. }) =
                    procedure_role(node, source)
                {
                    if let Some(name) = node.child_by_field_name("name") {
                        subscribers
                            .entry((object.to_lowercase(), event.to_lowercase()))
                            .or_default()
                            .push(Location {
                                uri: uri.clone(),
                                range: node_range(name),
                            });
                    }
                }
                continue;
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    subscribers
}

/// Where each workspace subscriber of `object`'s `event` is declared: the
/// name of its procedure.
pub fn subscriber_locations(workspace: &Workspace, object: &str, event: &str) -> Vec<Location> {
    workspace_subscribers(workspace)
        .remove(&(object.to_lowercase(), event.to_lowercase()))
        .unwrap_or_default()
}

fn node_range(node: tree_sitter::Node<'_>) -> Range {
    let start = node.start_position();
    let end = node.end_position();
    Range {
        start: Position {
            line: start.row as u32,
            character: start.column as u32,
        },
        end: Position {
            line: end.row as u32,
            character: end.column as u32,
        },
    }
}

/// The declaration of the event that the subscriber at `position` in `uri`
/// handles, in the workspace or in a symbol package's extracted source.
pub fn publisher_location(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Option<Location> {
    let path = uri.to_file_path().ok()?;
    let source = super::source::event_source(workspace, &path, position.line + 1).ok()?;
    let target = Url::from_file_path(source.path?).ok()?;
    let line = source.line?.saturating_sub(1);
    Some(Location {
        uri: target,
        range: Range {
            start: Position { line, character: 0 },
            end: Position { line, character: 0 },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLISHER: &str = "codeunit 50100 \"Pallet Events\"\n{\n    [IntegrationEvent(false, false)]\n    procedure OnAfterPost(Qty: Decimal)\n    begin\n    end;\n\n    procedure Helper()\n    begin\n    end;\n}\n";

    const SUBSCRIBER: &str = "codeunit 50101 \"Pallet Handler\"\n{\n    [EventSubscriber(ObjectType::Codeunit, Codeunit::\"Pallet Events\", 'OnAfterPost', '', false, false)]\n    local procedure HandleAfterPost(Qty: Decimal)\n    begin\n    end;\n}\n";

    fn parse(text: &str) -> tree_sitter::Tree {
        al_syntax::AlParser::parse_quick(text).tree
    }

    #[test]
    fn a_procedure_with_an_event_attribute_is_a_publisher() {
        let tree = parse(PUBLISHER);
        assert_eq!(
            event_role(
                PUBLISHER,
                &tree,
                Position {
                    line: 3,
                    character: 16
                }
            ),
            Some(EventRole::Publisher {
                object: "Pallet Events".to_string(),
                event: "OnAfterPost".to_string(),
            })
        );
        assert_eq!(
            event_role(
                PUBLISHER,
                &tree,
                Position {
                    line: 7,
                    character: 16
                }
            ),
            None,
            "a plain procedure has no event role"
        );
    }

    #[test]
    fn a_procedure_with_an_event_subscriber_attribute_names_its_event() {
        let tree = parse(SUBSCRIBER);
        assert_eq!(
            event_role(
                SUBSCRIBER,
                &tree,
                Position {
                    line: 3,
                    character: 22
                }
            ),
            Some(EventRole::Subscriber {
                object_type: "Codeunit".to_string(),
                object: "Pallet Events".to_string(),
                event: "OnAfterPost".to_string(),
            })
        );
    }

    /// Subscribers are found in the workspace's files, open or not, and
    /// matched to their event regardless of case.
    #[test]
    fn subscribers_are_found_in_workspace_files() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Handler.Codeunit.al"),
            SUBSCRIBER.to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Events.Codeunit.al"),
            PUBLISHER.to_string(),
        );

        let found = subscriber_locations(&ws, "pallet events", "onafterpost");

        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].uri.path().ends_with("Handler.Codeunit.al"));
        assert_eq!(
            found[0].range.start,
            Position {
                line: 3,
                character: 20
            }
        );
    }
}
