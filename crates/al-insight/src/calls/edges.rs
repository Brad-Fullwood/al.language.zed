//! Resolving call sites to call-graph edges.

use super::*;

/// Populate call graph edges for a single procedure.
///
/// Extracts call sites from the procedure body, then:
/// - `BareCall` → resolves against the same object's methods in `insight`.
/// - `MemberCall` → resolves object against `symbols`, then finds method in `insight`.
/// - `RecordOp` (any `RunTrigger`) → resolves variable to table via `var_types`,
///   then finds the table's `OnBefore{Op}Event` / `OnAfter{Op}Event` in `insight`.
// Tree-walk inputs (tree / source / current object) plus three lookup tables
// (symbols / insight graph / call graph) plus variable-type map plus the
// procedure node ID. All independent. A bundling struct doesn't shrink the
// call sites; it just splits the type's lifetime in two.
#[allow(clippy::too_many_arguments)]
pub fn populate_call_edges_for_procedure(
    tree: &tree_sitter::Tree,
    source: &str,
    object_kind: ObjectKind,
    object_name: &str,
    procedure_name: &str,
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) {
    populate_call_edges_in_object(
        tree.root_node(),
        source,
        object_kind,
        object_name,
        procedure_name,
        symbols,
        insight,
        call_graph,
    );
}

/// [`populate_call_edges_for_procedure`] for the procedure declared inside
/// `object_node`. In a file holding several objects, two of them can declare
/// the same procedure name (an interface and its implementation); a
/// whole-tree lookup found the first.
#[allow(clippy::too_many_arguments)]
pub fn populate_call_edges_in_object(
    object_node: tree_sitter::Node<'_>,
    source: &str,
    object_kind: ObjectKind,
    object_name: &str,
    procedure_name: &str,
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) {
    let Some(proc_node) = find_procedure_in_node(
        object_node,
        source.as_bytes(),
        &procedure_name.to_lowercase(),
    ) else {
        return;
    };
    let call_sites = call_sites_in_node(proc_node, source);
    let var_types = procedure_var_types_in_node(proc_node, source);
    // Collect object-typed variable declarations (codeunit / page /
    // report / xmlport / query / interface), not just Record. Used to resolve
    // `MyVar.Method()` where `MyVar` is e.g. `Codeunit "Sales-Post"` — the
    // prior code looked up `MyVar` itself in the symbol index, only matching
    // when the variable name happened to equal a real object name.
    let object_var_types = procedure_object_var_types_in_node(proc_node, source);

    // A callable workspace member can be represented by three graph node
    // variants. Event publishers and subscribers used to be registered as
    // `Event` / `Subscriber`, but this resolver looked only for `Procedure`.
    // Their bodies consequently had no outgoing edges: calls made by an event
    // subscriber disappeared from every transitive analysis. Resolve the
    // actual callable node so transaction lint, affected tests, coverage, and
    // event tracing all see the complete stack.
    let caller_id = match callable_node_id(insight, object_kind, object_name, procedure_name) {
        Some(id) => id,
        None => return,
    };

    for site in &call_sites {
        match site {
            CallSite::BareCall { name } => {
                let name_lower = name.to_lowercase();
                let obj_lower = object_name.to_lowercase();
                let callee_key =
                    NodeKey::Procedure(object_kind, obj_lower.clone(), name_lower.clone());
                if let Some(callee_id) = CallGraph::node_id_for(insight, &callee_key) {
                    call_graph.add_direct_call(caller_id, callee_id);
                } else {
                    // Also check if the call target is an event publisher on the same object
                    // (e.g. DoProcess() calling OnBeforeProcess() which is an IntegrationEvent)
                    let event_key = NodeKey::Event(object_kind, obj_lower, name_lower);
                    if let Some(event_id) = CallGraph::node_id_for(insight, &event_key) {
                        call_graph.add_direct_call(caller_id, event_id);
                        // Firing the event also runs every subscriber.
                        link_event_subscribers(caller_id, event_id, call_graph);
                    }
                }
            }
            CallSite::MemberCall { object, method } => {
                // prefer the variable's declared type when known.
                // `MyVar.Method()` where `MyVar: Codeunit "Sales-Post"` should
                // resolve against "Sales-Post" methods, not against a hypothetical
                // object literally named "MyVar". Fall back to the bare name
                // for the (common) case where the call's left side is itself
                // an object reference like `Customer.Get()`.
                let resolved_object = object_var_types
                    .get(&object.to_lowercase())
                    .map(String::as_str)
                    .unwrap_or(object.as_str());
                let method_lower = method.to_lowercase();
                let entries = symbols.get_by_name(resolved_object);
                let mut saw_interface = false;
                for entry in &entries {
                    if entry.kind == ObjectKind::Interface {
                        saw_interface = true;
                    }
                    let callee_key = NodeKey::Procedure(
                        entry.kind,
                        entry.name.to_lowercase(),
                        method_lower.clone(),
                    );
                    if let Some(callee_id) = CallGraph::node_id_for(insight, &callee_key) {
                        call_graph.add_direct_call(caller_id, callee_id);
                    }
                    // A member call may target an event publisher on
                    // another object (e.g. `PublisherVar.OnSomeEvent()`).
                    // Firing it reaches the event node and every subscriber.
                    let event_key =
                        NodeKey::Event(entry.kind, entry.name.to_lowercase(), method_lower.clone());
                    if let Some(event_id) = CallGraph::node_id_for(insight, &event_key) {
                        call_graph.add_direct_call(caller_id, event_id);
                        link_event_subscribers(caller_id, event_id, call_graph);
                    }
                }
                // Interface dispatch: when the receiver is `Interface "IFoo"`,
                // the concrete callee is unknown statically, so over-approximate
                // to `<method>` in every codeunit that `implements IFoo`.
                if saw_interface {
                    for impl_entry in find_interface_implementors(symbols, resolved_object) {
                        let impl_key = NodeKey::Procedure(
                            impl_entry.kind,
                            impl_entry.name.to_lowercase(),
                            method_lower.clone(),
                        );
                        if let Some(impl_id) = CallGraph::node_id_for(insight, &impl_key) {
                            call_graph.add_indirect_call(caller_id, impl_id);
                        }
                    }
                }
            }
            // The table's OnBefore/OnAfter{Op}Event are raised whatever
            // `RunTrigger` is: it decides only whether the table's own
            // OnInsert/OnModify/OnDelete code runs, which is why subscribers
            // test `if not RunTrigger then exit`. `Cust.Modify()` reaches
            // them as surely as `Cust.Modify(true)`.
            CallSite::RecordOp { variable, op, .. } => {
                let table_name = match var_types.get(&variable.to_lowercase()) {
                    Some(t) => t.clone(),
                    None => continue,
                };

                let (before_event, after_event) = record_op_event_names(*op);

                for event_name in &[before_event, after_event] {
                    let event_key = NodeKey::Event(
                        ObjectKind::Table,
                        table_name.to_lowercase(),
                        event_name.to_lowercase(),
                    );
                    if let Some(event_id) = CallGraph::node_id_for(insight, &event_key) {
                        call_graph.add_trigger(caller_id, event_id);
                        // Record-trigger events execute subscribers just like
                        // explicitly published events. Previously the graph
                        // stopped at the implicit OnBefore/OnAfter event node,
                        // dropping the rest of the event stack.
                        link_event_subscribers(caller_id, event_id, call_graph);
                    }
                }
            }
            CallSite::CodeunitRun { target } => {
                // `Codeunit.Run(Codeunit::"X")` dispatches to X.OnRun.
                let onrun_key = NodeKey::Procedure(
                    ObjectKind::Codeunit,
                    target.to_lowercase(),
                    "onrun".to_string(),
                );
                if let Some(onrun_id) = CallGraph::node_id_for(insight, &onrun_key) {
                    call_graph.add_indirect_call(caller_id, onrun_id);
                }
            }
        }
    }
}

/// Resolve the graph node that owns a callable AL member body.
///
/// Regular procedures/triggers, event publishers, and event subscribers are
/// deliberately distinct insight nodes, but all three can contain executable
/// AL and therefore need call edges.
pub(super) fn callable_node_id(
    insight: &InsightGraph,
    object_kind: ObjectKind,
    object_name: &str,
    procedure_name: &str,
) -> Option<NodeId> {
    let object = object_name.to_lowercase();
    let member = procedure_name.to_lowercase();
    [
        NodeKey::Subscriber(object_kind, object.clone(), member.clone()),
        NodeKey::Event(object_kind, object.clone(), member.clone()),
        NodeKey::Procedure(object_kind, object, member),
    ]
    .iter()
    .find_map(|key| CallGraph::node_id_for(insight, key))
}

/// Add over-approximated `caller → subscriber` edges for an event publish site.
///
/// Firing an event runs every `[EventSubscriber]` bound to it, so for
/// reachability the publishing procedure can reach each subscriber handler
/// The subscriber → event `EventSubscription` edges are already in
/// `call_graph` (added by [`CallGraph::build_from_insight`]); we read them via
/// [`CallGraph::subscribers_of`] and add the forward indirect edges.
pub(super) fn link_event_subscribers(
    caller_id: NodeId,
    event_id: NodeId,
    call_graph: &mut CallGraph,
) {
    let subscribers = call_graph.subscribers_of(event_id);
    for sub_id in subscribers {
        call_graph.add_indirect_call(caller_id, sub_id);
    }
}

/// Find every codeunit whose `implements` clause names `interface_name`.
///
/// A call through an
/// `Interface "IFoo"`-typed variable can land in any implementor at runtime,
/// so all of them are returned (the over-approximation). Interface names are
/// compared case-insensitively after stripping the quotes that the symbol
/// extractor preserves verbatim.
pub(super) fn find_interface_implementors(
    symbols: &SymbolIndex,
    interface_name: &str,
) -> Vec<Arc<SymbolEntry>> {
    let target = interface_name.unquote_identifier();
    symbols
        .get_by_kind(ObjectKind::Codeunit)
        .into_iter()
        .filter(|entry| {
            entry
                .implements
                .iter()
                .any(|iface| iface.unquote_identifier().eq_ignore_ascii_case(&target))
        })
        .collect()
}

/// Return the event names for a record operation.
///
/// BC table events follow the pattern: `OnBefore{Op}Event` / `OnAfter{Op}Event`.
///
/// **Hardcoded naming convention:** the `OnBefore{Op}Event` /
/// `OnAfter{Op}Event` pattern is part of the BC record runtime contract,
/// not AL language surface — Microsoft has not changed the convention since
/// the introduction of `IntegrationEvent` on tables. This is a stable ABI
/// string format. If a future BC
/// release introduces a new table-event naming scheme (e.g.
/// `OnValidateField{Op}`) this function will need extending — at which
/// point the right move is to derive the patterns from a symbol scan of
/// real `IntegrationEvent` attributes on Table objects, not to chase
/// per-release additions here.
pub(super) fn record_op_event_names(op: RecordOp) -> (String, String) {
    let op_str = match op {
        RecordOp::Insert => "Insert",
        RecordOp::Modify => "Modify",
        RecordOp::Delete => "Delete",
        RecordOp::Validate => "Validate",
    };
    (
        format!("OnBefore{}Event", op_str),
        format!("OnAfter{}Event", op_str),
    )
}

/// Count the number of call-suffix nodes in a tree.
///
/// Used to rank files by complexity: high-fanout files (many calls) are resolved
/// eagerly in Tier 1.
pub fn fanout_score(tree: &tree_sitter::Tree) -> usize {
    let mut count = 0;
    count_call_suffixes(tree.root_node(), &mut count);
    count
}

pub(super) fn count_call_suffixes(root: tree_sitter::Node, count: &mut usize) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "call_suffix" | "member_call_suffix" | "scope_call_suffix" => {
                *count += 1;
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

/// Populate call edges across all workspace files.
///
/// Algorithm:
/// 1. Score each file by fanout (call count).
/// 2. Resolve Tier 1 (score >= 5 or top 20%) eagerly.
/// 3. Return the number of procedures resolved.
///
pub fn populate_workspace_call_edges(
    file_index: &FileIndex,
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) -> Result<usize, SourceGraphError> {
    let mut file_scores: Vec<(
        std::path::PathBuf,
        String,
        tree_sitter::Tree,
        al_source::file_index::CachedObjectInfo,
        usize,
    )> = Vec::new();

    // Every object of every file, not only a file's first one: in a file
    // holding a table and then a codeunit, the codeunit's procedures were
    // never resolved (and were looked up under the table).
    for (path, info) in indexed_objects(file_index) {
        let (source, tree) = indexed_parse(file_index, &path)?;
        let mut score = 0;
        count_call_suffixes(object_node(&tree, &info), &mut score);
        file_scores.push((path, source, tree, info, score));
    }

    if file_scores.is_empty() {
        return Ok(0);
    }

    let threshold = tier1_threshold(&file_scores);

    let mut resolved = 0;

    for (path, source, tree, info, score) in &file_scores {
        if *score < threshold {
            continue;
        }

        let ok = indexed_object_kind(path, info)?;
        let node = object_node(tree, info);

        let procedures = collect_procedure_names_from_tree(node, source.as_bytes());
        for proc_name in procedures {
            let proc_id =
                callable_node_id(insight, ok, &info.name, &proc_name).ok_or_else(|| {
                    SourceGraphError::MissingCallableNode {
                        path: path.clone(),
                        kind: ok,
                        object: info.name.clone(),
                        member: proc_name.clone(),
                    }
                })?;
            if call_graph.resolution_state(proc_id) == EdgeResolutionState::Unresolved {
                call_graph.set_resolution_state(proc_id, EdgeResolutionState::Resolving);

                populate_call_edges_in_object(
                    node, source, ok, &info.name, &proc_name, symbols, insight, call_graph,
                );

                call_graph.set_resolution_state(proc_id, EdgeResolutionState::Resolved);
                resolved += 1;
            }
        }
    }

    Ok(resolved)
}

/// Resolve direct-call / trigger edges for **every** workspace procedure,
/// ignoring the fanout tiering used by [`populate_workspace_call_edges`].
///
/// The tiered builder only resolves high-fanout ("Tier 1") files eagerly, so a
/// procedure in a low-fanout file can have unresolved outgoing edges. That is
/// fine for the daemon's interactive queries (which resolve on demand) but
/// **not** for call-graph-based affected-test detection: a missing
/// edge there is a false negative — a test that depends on a changed procedure
/// would be silently skipped. This function walks all files and resolves any
/// procedure not already marked [`EdgeResolutionState::Resolved`], so the
/// resulting graph is complete for a backward reachability walk.
///
/// Returns the number of procedures whose edges were resolved by this call.
///
pub fn resolve_all_workspace_call_edges(
    file_index: &FileIndex,
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) -> Result<usize, SourceGraphError> {
    let mut resolved = 0;

    for (path, info) in indexed_objects(file_index) {
        let (source, tree) = indexed_parse(file_index, &path)?;
        let ok = indexed_object_kind(&path, &info)?;
        let node = object_node(&tree, &info);

        let procedures = collect_procedure_names_from_tree(node, source.as_bytes());
        for proc_name in procedures {
            let proc_id =
                callable_node_id(insight, ok, &info.name, &proc_name).ok_or_else(|| {
                    SourceGraphError::MissingCallableNode {
                        path: path.clone(),
                        kind: ok,
                        object: info.name.clone(),
                        member: proc_name.clone(),
                    }
                })?;
            if call_graph.resolution_state(proc_id) != EdgeResolutionState::Resolved {
                populate_call_edges_in_object(
                    node, &source, ok, &info.name, &proc_name, symbols, insight, call_graph,
                );
                call_graph.set_resolution_state(proc_id, EdgeResolutionState::Resolved);
                resolved += 1;
            }
        }
    }

    Ok(resolved)
}

/// Compute the Tier 1 fanout threshold.
///
/// A file is Tier 1 if its score >= 5 or its score is in the top 20%.
pub(super) fn tier1_threshold(
    files: &[(
        std::path::PathBuf,
        String,
        tree_sitter::Tree,
        al_source::file_index::CachedObjectInfo,
        usize,
    )],
) -> usize {
    if files.is_empty() {
        return 5;
    }

    let mut scores: Vec<usize> = files.iter().map(|(_, _, _, _, s)| *s).collect();
    scores.sort_unstable();
    let cutoff_idx = scores.len() * 8 / 10; // 80th percentile index
    let percentile_threshold = scores.get(cutoff_idx).copied().unwrap_or(5);

    // Tier 1 rule per the doc-comment: include a file if score >= 5 OR it is in
    // the top 20%. The gate downstream is `score >= threshold`; to express the
    // union we take the SMALLER of the two so either condition admits the file.
    // `.max(1)` keeps the threshold positive so a workspace of all-zero scores
    // still excludes everything.
    percentile_threshold.clamp(1, 5)
}

pub(super) fn collect_procedure_names_from_tree(
    node: tree_sitter::Node,
    source: &[u8],
) -> Vec<String> {
    let mut names = Vec::new();
    collect_procedure_names_from_node(node, source, &mut names);
    names
}

pub(super) fn collect_procedure_names_from_node(
    node: tree_sitter::Node,
    source: &[u8],
    names: &mut Vec<String>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            match child.kind() {
                "procedure_declaration" | "trigger_declaration" => {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        if let Ok(text) = name_node.utf8_text(source) {
                            let name = text.unquote_identifier().into_owned();
                            if !name.is_empty() {
                                names.push(name);
                            }
                        }
                    }
                    // Don't push procedure children — we only want top-level names
                }
                _ => {
                    stack.push(child);
                }
            }
        }
    }
}
