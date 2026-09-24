//! Registering workspace and dependency-source objects and procedures as
//! insight-graph nodes.

use super::*;

/// Register workspace objects, procedures, events, and subscribers as
/// InsightGraph nodes.
///
/// Must be called **before** the graph is wrapped in Arc. Detects:
/// - `[EventSubscriber]` attributes → `InsightNode::Subscriber`
/// - `[IntegrationEvent]` / `[BusinessEvent]` attributes → `InsightNode::Event`
/// - All other procedures → `InsightNode::Procedure`
///
/// Also adds `Contains` edges from the parent object to each member.
pub fn register_workspace_nodes(
    file_index: &FileIndex,
    symbols: &SymbolIndex,
    insight: &mut InsightGraph,
) -> Result<(), SourceGraphError> {
    let mut workspace_entries: Vec<al_symbols::SymbolEntry> = Vec::new();

    // Snapshot the (path, info) pairs in one short-lived shard iteration.
    // The body of this loop calls
    // file_index.get_cached_parse(path) which acquires *other* DashMap
    // shards (files / file_trees) and runs a full tree walk per entry —
    // Previously, we held the object_info shard read lock the entire time,
    // blocking concurrent did_change writers to that shard for the
    // duration of the build. Cloning the snapshot is cheap (kB-scale)
    // versus the cost of an N-file tree walk that follows.
    let snapshot: Vec<(std::path::PathBuf, al_source::file_index::CachedObjectInfo)> = file_index
        .object_info
        .iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();

    for (path, info) in snapshot {
        let path = path.as_path();
        let info = &info;

        let ok = indexed_object_kind(path, info)?;
        let id = indexed_object_id(path, info, ok)?;
        let obj_key = NodeKey::Object(ok, info.name.to_lowercase());
        let obj_idx = insight.ensure_node(
            obj_key,
            InsightNode::Object {
                kind: ok,
                id,
                name: info.name.clone(),
                package: "workspace".to_string(),
            },
        );

        let (source, tree) = indexed_parse(file_index, path)?;

        let source_bytes = source.as_bytes();
        register_procedures_from_tree(
            tree.root_node(),
            source_bytes,
            ok,
            &info.name,
            obj_idx,
            insight,
        );

        // Also extract MethodSymbol + FieldSymbol data and add to the
        // SymbolIndex so that parameter lookups (lookup_event_params) find
        // workspace methods and scaffolding (`generate page --table`) finds
        // workspace table fields. Always push the entry — even a
        // member-less object must be resolvable by name/id/composition.
        let methods = extract_methods_from_tree(tree.root_node(), source_bytes);
        let fields = match ok {
            ObjectKind::Table | ObjectKind::TableExtension => {
                extract_fields_from_tree(tree.root_node(), source_bytes)
            }
            _ => Vec::new(),
        };
        workspace_entries.push(al_symbols::SymbolEntry {
            kind: ok,
            id,
            name: info.name.clone(),
            package: "workspace".to_string(),
            methods,
            fields,
            extends: info_extends_from_tree(tree.root_node(), source_bytes),
            // Capture the `implements` clause so interface dispatch
            // resolution can find implementors. This pass is the authoritative
            // source for workspace symbol entries (it clobbers the "workspace"
            // package), so without it `implements` would always be empty.
            implements: info_implements_from_tree(tree.root_node(), source_bytes, &info.name),
            properties: extract_object_properties_from_tree(tree.root_node(), source_bytes),
            ..Default::default()
        });
    }

    symbols.remove_package_entries("workspace");
    if !workspace_entries.is_empty() {
        symbols.add_entries_owned(workspace_entries);
    }

    // connect workspace Subscriber nodes to their target Event nodes.
    // Without this pass the subscribers registered above carried their
    // target on the node but had no SubscribesTo edge, so `trace` showed
    // origins and nothing else.
    insight.resolve_subscriber_edges();
    Ok(())
}

/// Register objects and callable members parsed from dependency package source.
///
/// Loaded `SymbolReference.json` data creates the ordinary package graph first;
/// this source enrichment adds attributes that symbol metadata can omit (most
/// importantly `EventSubscriber`) and makes complete Microsoft/third-party AL
/// bodies available to call-edge resolution. Unlike [`register_workspace_nodes`]
/// it deliberately does not rewrite the shared [`SymbolIndex`]: dependency
/// symbols are already indexed under their real package identities.
pub fn register_dependency_source_nodes(
    file_index: &FileIndex,
    insight: &mut InsightGraph,
) -> Result<(), SourceGraphError> {
    let snapshot: Vec<(std::path::PathBuf, al_source::file_index::CachedObjectInfo)> = file_index
        .object_info
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect();

    for (path, info) in snapshot {
        let object_kind = indexed_object_kind(&path, &info)?;
        let object_id = indexed_object_id(&path, &info, object_kind)?;
        let object_key = NodeKey::Object(object_kind, info.name.to_lowercase());
        let object = insight.ensure_node(
            object_key,
            InsightNode::Object {
                kind: object_kind,
                id: object_id,
                name: info.name.clone(),
                package: "dependency-source".to_string(),
            },
        );
        let (source, tree) = indexed_parse(file_index, &path)?;
        register_procedures_from_tree(
            tree.root_node(),
            source.as_bytes(),
            object_kind,
            &info.name,
            object,
            insight,
        );
    }

    insight.resolve_subscriber_edges();
    Ok(())
}

/// Iteratively walk the AST registering procedure/trigger declarations.
///
/// Uses an explicit stack to avoid unbounded recursion on deeply nested AL.
/// When a procedure
/// or trigger node is found, it is dispatched but its body is NOT pushed
/// onto the stack — nested procedures inside a procedure body are not legal
/// AL anyway.
pub(super) fn register_procedures_from_tree(
    node: tree_sitter::Node,
    source: &[u8],
    object_kind: ObjectKind,
    object_name: &str,
    obj_idx: petgraph::graph::NodeIndex,
    insight: &mut InsightGraph,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            match child.kind() {
                "procedure_declaration" | "trigger_declaration" => {
                    register_single_procedure(
                        child,
                        source,
                        object_kind,
                        object_name,
                        obj_idx,
                        insight,
                    );
                }
                _ => {
                    stack.push(child);
                }
            }
        }
    }
}

pub(super) fn register_single_procedure(
    proc_node: tree_sitter::Node,
    source: &[u8],
    object_kind: ObjectKind,
    object_name: &str,
    obj_idx: petgraph::graph::NodeIndex,
    insight: &mut InsightGraph,
) {
    let name_node = match proc_node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };

    let proc_name = match name_node.utf8_text(source).ok() {
        Some(t) => t.unquote_identifier().into_owned(),
        None => return,
    };

    if proc_name.is_empty() {
        return;
    }

    let attributes = collect_procedure_attributes(proc_node, source);

    // AL attributes are case-insensitive at the language level — `[eventsubscriber(...)]`,
    // `[EventSubscriber(...)]`, and `[EVENTSUBSCRIBER(...)]` are all valid. Attribute
    // names here come from raw tree-sitter text (preserves source case), unlike
    // `AttributeSymbol.name` in .app metadata which is normalised.
    let is_integration_event = attributes
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(crate::attr_names::INTEGRATION_EVENT));
    let is_business_event = attributes
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(crate::attr_names::BUSINESS_EVENT));
    let is_subscriber = attributes
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(crate::attr_names::EVENT_SUBSCRIBER));
    let is_local = has_local_modifier(proc_node, source);

    if is_integration_event || is_business_event {
        let event_type = if is_business_event {
            EventNodeType::Business
        } else {
            EventNodeType::Integration
        };
        let key = NodeKey::Event(
            object_kind,
            object_name.to_lowercase(),
            proc_name.to_lowercase(),
        );
        let evt_idx = insight.ensure_node(
            key,
            InsightNode::Event {
                object_kind,
                object_name: object_name.to_string(),
                name: proc_name,
                event_type,
            },
        );
        insight.add_edge(obj_idx, evt_idx, InsightEdge::Publishes);
    } else if is_subscriber {
        let (target_object, target_event) = parse_subscriber_target_from_attrs(&attributes);
        let target_kind = subscriber_target_kind(&attributes);
        let key = NodeKey::Subscriber(
            object_kind,
            object_name.to_lowercase(),
            proc_name.to_lowercase(),
        );
        let sub_idx = insight.ensure_node(
            key,
            InsightNode::Subscriber {
                object_kind,
                object_name: object_name.to_string(),
                name: proc_name,
                target_kind,
                target_object,
                target_event,
            },
        );
        insight.add_edge(obj_idx, sub_idx, InsightEdge::Contains);
    } else {
        let key = NodeKey::Procedure(
            object_kind,
            object_name.to_lowercase(),
            proc_name.to_lowercase(),
        );
        let proc_idx = insight.ensure_node(
            key,
            InsightNode::Procedure {
                object_kind,
                object_name: object_name.to_string(),
                name: proc_name,
                is_local,
            },
        );
        insight.add_edge(obj_idx, proc_idx, InsightEdge::Contains);
    }
}

pub fn collect_procedure_attributes(
    proc_node: tree_sitter::Node,
    source: &[u8],
) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "attribute" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let args = child
                        .utf8_text(source)
                        .ok()
                        .map(|t| t.to_string())
                        .unwrap_or_default();
                    attrs.push((name_text.to_string(), args));
                }
            }
        }
    }
    attrs
}

pub(super) fn has_local_modifier(proc_node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "member_modifier" {
            if let Ok(text) = child.utf8_text(source) {
                if text.trim().eq_ignore_ascii_case("local") {
                    return true;
                }
            }
        }
    }
    false
}

/// Parse EventSubscriber attribute args to get target object and event names.
///
/// The args text looks like: `[EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]`
/// We extract arg[1] (object name) and arg[2] (event name).
pub(super) fn parse_subscriber_target_from_attrs(attrs: &[(String, String)]) -> (String, String) {
    for (name, args_text) in attrs {
        // Case-insensitive: see the corresponding comment at the procedure-attribute
        // collection site — raw tree-sitter text preserves source case.
        if name.eq_ignore_ascii_case(crate::attr_names::EVENT_SUBSCRIBER) {
            let args = extract_attribute_args(args_text);
            let target_object = args
                .get(1)
                .map(|s| al_syntax::clean_attr_arg(s))
                .unwrap_or_default();
            let target_event = args
                .get(2)
                .map(|s| al_syntax::clean_attr_arg(s))
                .unwrap_or_default();
            return (target_object, target_event);
        }
    }
    (String::new(), String::new())
}

/// The publisher kind an `EventSubscriber` attribute names in its first
/// argument (`ObjectType::Codeunit`, or a bare `Codeunit`).
pub(super) fn subscriber_target_kind(attrs: &[(String, String)]) -> Option<ObjectKind> {
    let (_, args_text) = attrs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(crate::attr_names::EVENT_SUBSCRIBER))?;
    let first = extract_attribute_args(args_text).into_iter().next()?;
    let first = first.trim();
    let kind = first.rsplit_once("::").map_or(first, |(_, kind)| kind);
    kind.trim().parse::<ObjectKind>().ok()
}

/// Extract comma-separated arguments from an attribute text like `[Attr(a, b, c)]`.
pub fn extract_attribute_args(attr_text: &str) -> Vec<String> {
    let start = match attr_text.find('(') {
        Some(i) => i + 1,
        None => return vec![],
    };
    let end = match attr_text.rfind(')') {
        Some(i) => i,
        None => return vec![],
    };
    if end <= start {
        return vec![];
    }

    let inner = &attr_text[start..end];
    // Split by comma, respecting nested parens and quotes (single and double).
    let mut args = Vec::new();
    let mut current = String::new();
    let mut paren_depth = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = inner.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                current.push(ch);
            }
            '"' if !in_single => {
                if in_double {
                    // Check for doubled-quote escape: "" inside double-quoted string
                    if chars.peek() == Some(&'"') {
                        // Escaped quote — consume the second `"` and keep in_double
                        chars.next();
                        current.push('"');
                        current.push('"');
                    } else {
                        in_double = false;
                        current.push(ch);
                    }
                } else {
                    in_double = true;
                    current.push(ch);
                }
            }
            '(' if !in_single && !in_double => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' if !in_single && !in_double => {
                paren_depth -= 1;
                current.push(ch);
            }
            ',' if !in_single && !in_double && paren_depth == 0 => {
                args.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

// `clean_attr_arg` now lives in `al-syntax` (al_syntax::clean_attr_arg).
