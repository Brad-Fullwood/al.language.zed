//! Call edge extraction from workspace AL source ASTs.
//!
//! Walks tree-sitter parse trees from workspace files to extract three kinds
//! of directed edges for the insight call graph:
//!
//! | Kind | Grammar pattern | Edge |
//! |------|----------------|------|
//! | Direct call | `Foo()` / `Obj.Method()` | `Calls` |
//! | Record op (run_trigger=true) | `Rec.Insert(true)` | `Triggers` |
//! | EventSubscriber attribute | `[EventSubscriber(...)]` | `SubscribesTo` |
//!
//! The top-level entry points are:
//! - [`register_workspace_nodes`] — register nodes in InsightGraph before
//!   wrapping in Arc (must be called before [`populate_workspace_call_edges`]).
//! - [`populate_workspace_call_edges`] — score + resolve call edges across all
//!   workspace files.
//! - [`fanout_score`] — count call-suffix nodes in a tree (used for tier ranking).

use std::collections::HashMap;
use std::sync::Arc;

use al_symbols::{ObjectKind, SymbolEntry, SymbolIndex};

use super::graph::{EventNodeType, InsightEdge, InsightGraph, InsightNode, NodeKey};
use super::index::{CallGraph, EdgeResolutionState, NodeId};
use al_source::file_index::FileIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOp {
    Insert,
    Modify,
    Delete,
    Validate,
}

impl RecordOp {
    /// Parse from a method name (case-insensitive).
    ///
    /// The four operations are the stable AL record-runtime tokens since
    /// NAV 2.0 — they're part of the BC record ABI (each fires OnBefore/OnAfter
    /// table events), not AL *language* keywords or built-in functions. This
    /// set is fixed by Microsoft and has not changed in
    /// 20+ years. Locked in here rather than fetched from `LanguageData` so
    /// the call-graph builder has no runtime dependency on language data load
    /// order.
    pub fn from_method_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "insert" => Some(RecordOp::Insert),
            "modify" => Some(RecordOp::Modify),
            "delete" => Some(RecordOp::Delete),
            "validate" => Some(RecordOp::Validate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CallSite {
    BareCall {
        name: String,
    },
    MemberCall {
        object: String,
        method: String,
    },
    RecordOp {
        variable: String,
        op: RecordOp,
        run_trigger: bool,
    },
    /// `Codeunit.Run(Codeunit::"X")` / `Codeunit.RunModal(Codeunit::X)` with a
    /// literal codeunit reference as the first argument. The dispatch
    /// target is `X`'s `OnRun` trigger. Only the *literal* form is captured;
    /// `Codeunit.Run(SomeVariable)` is left unresolved (no sound static target).
    CodeunitRun {
        target: String,
    },
}

/// Extract a mapping of `lowercase_variable_name -> table_name` for all
/// `Record "X"` variables in a procedure's `var` section and parameters.
///
/// Returns only `Record`-typed variables since those are what trigger table events.
pub fn extract_procedure_var_types(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> HashMap<String, String> {
    let source_bytes = source.as_bytes();
    let mut result = HashMap::new();

    let proc_node = find_procedure_node(tree, source_bytes, procedure_name);
    let Some(proc_node) = proc_node else {
        return result;
    };

    collect_record_vars_from_procedure_node(proc_node, source_bytes, &mut result);

    result
}

fn find_procedure_node<'a>(
    tree: &'a tree_sitter::Tree,
    source: &[u8],
    procedure_name: &str,
) -> Option<tree_sitter::Node<'a>> {
    let proc_name_lower = procedure_name.to_lowercase();
    let root = tree.root_node();

    find_procedure_in_node(root, source, &proc_name_lower)
}

fn find_procedure_in_node<'a>(
    root: tree_sitter::Node<'a>,
    source: &[u8],
    proc_name_lower: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if kind == "procedure_declaration" || kind == "trigger_declaration" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let clean = name_text.trim_matches('"').trim();
                    if clean.to_lowercase() == proc_name_lower {
                        return Some(node);
                    }
                }
            }
            // Do not descend further into this procedure's body
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    None
}

fn collect_record_vars_from_procedure_node(
    proc_node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        match child.kind() {
            "var_section" => {
                collect_record_vars_from_var_section(child, source, result);
            }
            "parameter_list" => {
                collect_record_vars_from_parameter_list(child, source, result);
            }
            _ => {}
        }
    }
}

fn collect_record_vars_from_var_section(
    section: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = section.walk();
    for child in section.children(&mut cursor) {
        if child.kind() == "variable_declaration" {
            collect_record_from_variable_declaration(child, source, result);
        }
    }
}

fn collect_record_vars_from_parameter_list(
    param_list: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = param_list.walk();
    for child in param_list.children(&mut cursor) {
        if child.kind() == "parameter" {
            collect_record_from_parameter(child, source, result);
        }
    }
}

fn collect_record_from_variable_declaration(
    container: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = container.walk();
    for child in container.children(&mut cursor) {
        if child.kind() == "regular_variable_declaration" {
            collect_record_from_regular_var_decl(child, source, result);
        }
    }
    if container.kind() == "regular_variable_declaration" {
        collect_record_from_regular_var_decl(container, source, result);
    }
}

fn collect_record_from_regular_var_decl(
    node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let type_node = match node.child_by_field_name("type") {
        Some(n) => n,
        None => return,
    };

    let name = match name_node.utf8_text(source).ok() {
        Some(t) => t.trim_matches('"').trim().to_string(),
        None => return,
    };

    let (type_kw, subtype) = parse_type_reference_for_record(type_node, source);
    if type_kw.eq_ignore_ascii_case("record") {
        if let Some(table_name) = subtype {
            result.insert(name.to_lowercase(), table_name);
        }
    }
}

fn collect_record_from_parameter(
    node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let type_node = match node.child_by_field_name("type") {
        Some(n) => n,
        None => return,
    };

    let name = match name_node.utf8_text(source).ok() {
        Some(t) => t.trim_matches('"').trim().to_string(),
        None => return,
    };

    let (type_kw, subtype) = parse_type_reference_for_record(type_node, source);
    if type_kw.eq_ignore_ascii_case("record") {
        if let Some(table_name) = subtype {
            result.insert(name.to_lowercase(), table_name);
        }
    }
}

/// Extract object-typed (codeunit / page / report / xmlport / query /
/// interface) variable declarations from a procedure's `var` section and
/// parameters. Returns a map of lowercase var-name → object name.
///
/// Companion to `extract_procedure_var_types` (which handles only `Record`);
/// used by member-call resolution to translate `MyVar.Method()` →
/// `<ObjectName>.Method()` when the variable's declared type is an object
/// reference. Excludes `Record` because those don't act
/// as method-call receivers in the same sense (their methods live on the
/// table object, but the call-graph already routes those via the
/// `RecordOp` trigger path).
pub fn extract_procedure_object_var_types(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> HashMap<String, String> {
    let source_bytes = source.as_bytes();
    let mut result = HashMap::new();

    let Some(proc_node) = find_procedure_node(tree, source_bytes, procedure_name) else {
        return result;
    };

    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        match child.kind() {
            "var_section" => collect_object_vars_from_var_section(child, source_bytes, &mut result),
            "parameter_list" => {
                collect_object_vars_from_parameter_list(child, source_bytes, &mut result)
            }
            _ => {}
        }
    }
    result
}

fn collect_object_vars_from_var_section(
    section: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = section.walk();
    for child in section.children(&mut cursor) {
        if child.kind() == "variable_declaration" {
            let mut inner_cursor = child.walk();
            for inner in child.children(&mut inner_cursor) {
                if inner.kind() == "regular_variable_declaration" {
                    collect_object_var_from_regular_decl(inner, source, result);
                }
            }
            if child.kind() == "regular_variable_declaration" {
                collect_object_var_from_regular_decl(child, source, result);
            }
        }
    }
}

fn collect_object_vars_from_parameter_list(
    param_list: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = param_list.walk();
    for child in param_list.children(&mut cursor) {
        if child.kind() == "parameter" {
            let Some(name_node) = child.child_by_field_name("name") else {
                continue;
            };
            let Some(type_node) = child.child_by_field_name("type") else {
                continue;
            };
            extract_object_subtype(name_node, type_node, source, result);
        }
    }
}

fn collect_object_var_from_regular_decl(
    node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    extract_object_subtype(name_node, type_node, source, result);
}

fn extract_object_subtype(
    name_node: tree_sitter::Node,
    type_node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let Some(name) = name_node
        .utf8_text(source)
        .ok()
        .map(|t| t.trim_matches('"').trim().to_string())
    else {
        return;
    };
    if name.is_empty() {
        return;
    }
    let (type_kw, subtype) = parse_type_reference_for_record(type_node, source);
    // Lowercased so the match handles "Codeunit"/"codeunit"/"CODEUNIT".
    let kw_lower = type_kw.to_ascii_lowercase();
    let is_object_var = matches!(
        kw_lower.as_str(),
        "codeunit" | "page" | "report" | "xmlport" | "query" | "interface"
    );
    if is_object_var {
        if let Some(obj_name) = subtype {
            result.insert(name.to_lowercase(), obj_name);
        }
    }
}

fn parse_type_reference_for_record(
    node: tree_sitter::Node,
    source: &[u8],
) -> (String, Option<String>) {
    let mut type_keyword = String::new();
    let mut subtype = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();

        if type_keyword.is_empty() && kind.starts_with("kw_") {
            if let Ok(text) = child.utf8_text(source) {
                type_keyword = text.to_string();
            }
        } else if type_keyword.is_empty()
            && (kind == "identifier" || kind == "name" || kind == "name_or_keyword")
        {
            if let Ok(text) = child.utf8_text(source) {
                type_keyword = text.trim_matches('"').to_string();
            }
        } else if !type_keyword.is_empty()
            && matches!(
                kind,
                "name_or_keyword" | "name" | "quoted_identifier" | "identifier" | "string"
            )
        {
            if let Ok(text) = child.utf8_text(source) {
                let clean = text.trim_matches('"').trim_matches('\'').to_string();
                if !clean.is_empty() {
                    subtype = Some(clean);
                }
            }
        }
    }

    (type_keyword, subtype)
}

/// Extract all call sites from a named procedure in the given tree.
///
/// Walks the procedure's `begin_end_block` looking for `postfix_expression`
/// nodes that end with a call suffix.
pub fn extract_call_sites(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> Vec<CallSite> {
    let source_bytes = source.as_bytes();
    let mut sites = Vec::new();

    let proc_node = match find_procedure_node(tree, source_bytes, procedure_name) {
        Some(n) => n,
        None => return sites,
    };

    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "begin_end_block" {
            collect_call_sites_from_block(child, source_bytes, &mut sites);
        }
    }

    sites
}

/// Iteratively collect call sites from a `begin_end_block` or any child node.
///
/// Uses an explicit stack to avoid unbounded recursion on deeply nested AL.
fn collect_call_sites_from_block(
    node: tree_sitter::Node,
    source: &[u8],
    sites: &mut Vec<CallSite>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            if child.kind() == "postfix_expression" {
                if let Some(site) = parse_postfix_expression(child, source) {
                    sites.push(site);
                }
            } else {
                stack.push(child);
            }
        }
    }
}

fn parse_postfix_expression(node: tree_sitter::Node, source: &[u8]) -> Option<CallSite> {
    let children: Vec<tree_sitter::Node> = {
        let mut cursor = node.walk();
        node.children(&mut cursor).collect()
    };

    if children.is_empty() {
        return None;
    }

    // The last child must be a call-type suffix for this to be a call site.
    let last = children.last()?;
    let last_kind = last.kind();

    match last_kind {
        "call_suffix" => {
            let primary = children.first()?;
            let name = extract_primary_expression_name(*primary, source)?;
            Some(CallSite::BareCall { name })
        }
        "member_call_suffix" => {
            let primary = children.first()?;
            let object_name = extract_primary_expression_name(*primary, source)?;

            let method_node = last.child_by_field_name("member")?;
            let method_name = method_node
                .utf8_text(source)
                .ok()?
                .trim_matches('"')
                .to_string();

            if let Some(op) = RecordOp::from_method_name(&method_name) {
                let run_trigger = parse_run_trigger_arg(*last, source, op);
                Some(CallSite::RecordOp {
                    variable: object_name,
                    op,
                    run_trigger,
                })
            } else if is_codeunit_run_method(&method_name) {
                // `Codeunit.Run(Codeunit::"X")` / `RunModal(...)` — resolve the
                // literal codeunit reference. Falls back to a plain
                // member call when the argument is not a `Codeunit::<name>`
                // literal (e.g. a variable), which keeps the dispatch sound.
                if let Some(target) = extract_codeunit_run_target(*last, source) {
                    Some(CallSite::CodeunitRun { target })
                } else {
                    Some(CallSite::MemberCall {
                        object: object_name,
                        method: method_name,
                    })
                }
            } else {
                Some(CallSite::MemberCall {
                    object: object_name,
                    method: method_name,
                })
            }
        }
        "scope_call_suffix" => {
            let primary = children.first()?;
            let object_name = extract_primary_expression_name(*primary, source)?;

            let method_node = last.child_by_field_name("member")?;
            let method_name = method_node
                .utf8_text(source)
                .ok()?
                .trim_matches('"')
                .to_string();

            Some(CallSite::MemberCall {
                object: object_name,
                method: method_name,
            })
        }
        _ => {
            // Not a call-ending expression — check if any child is a call we missed
            // by recursing into nested postfix expressions inside suffixes.
            // (No further action here; the caller recurses the block.)
            None
        }
    }
}

fn extract_primary_expression_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();

    let inner = if node.kind() == "primary_expression" && !children.is_empty() {
        children[0]
    } else {
        node
    };

    match inner.kind() {
        "name" | "name_or_keyword" | "identifier" | "quoted_identifier" => inner
            .utf8_text(source)
            .ok()
            .map(|t| t.trim_matches('"').to_string()),
        _ => inner
            .utf8_text(source)
            .ok()
            .map(|t| t.trim_matches('"').to_string()),
    }
}

/// Parse the `RunTrigger` argument from a record-operation call suffix.
///
/// - `Insert(RunTrigger)`: first arg is `true`/`false`; default = `true` if absent.
/// - `Modify(RunTrigger)`: same as Insert.
/// - `Delete(RunTrigger)`: same as Insert.
/// - `Validate(...)`: always fires trigger (no RunTrigger param), returns `true`.
fn parse_run_trigger_arg(
    member_call_suffix: tree_sitter::Node,
    source: &[u8],
    op: RecordOp,
) -> bool {
    if op == RecordOp::Validate {
        return true;
    }

    let arg_list = match member_call_suffix.child_by_field_name("call") {
        Some(n) => n,
        None => return true, // no arg list → default true
    };

    let mut cursor = arg_list.walk();
    for child in arg_list.children(&mut cursor) {
        // argument_list: '(' [expression (',' expression)*] ')'
        // We look for the first expression-like child (not '(' or ')')
        let kind = child.kind();
        if kind != "(" && kind != ")" && kind != "," {
            if let Ok(text) = child.utf8_text(source) {
                let trimmed = text.trim().to_lowercase();
                if trimmed == "false" {
                    return false;
                }
                if trimmed == "true" {
                    return true;
                }
                // Complex expression — we can't evaluate it statically. AL's
                // documented default is "trigger fires", so producing a
                // trigger edge is the safe over-approximation: false-positive
                // edges show up as extra entries in deadcode/impact, not
                // missed dependencies. Logged at debug so the false-positive
                // rate is observable when investigating dead-code reports.
                tracing::debug!(
                    expr = %text.trim(),
                    op = ?op,
                    "parse_run_trigger_arg: non-literal RunTrigger expression — assuming true (default)"
                );
                return true;
            }
        }
    }

    true // default: no args → RunTrigger=true
}

/// True for the BC built-ins that launch a codeunit by reference: `Run` and
/// `RunModal`. These are the AL `Codeunit.Run`/`Codeunit.RunModal` system
/// methods whose effect is to invoke the target codeunit's `OnRun` trigger.
/// Stable ABI names (unchanged for 20+ years), locked in here for the same
/// reason as [`RecordOp::from_method_name`].
fn is_codeunit_run_method(method: &str) -> bool {
    method.eq_ignore_ascii_case("Run") || method.eq_ignore_ascii_case("RunModal")
}

/// Extract the literal codeunit target of a `Codeunit.Run(Codeunit::"X")` /
/// `RunModal(Codeunit::X)` call from its `member_call_suffix` node.
///
/// Returns the bare object name (`X`) only when the first argument is a
/// `Codeunit::<name>` literal; returns `None` for any other first argument
/// (e.g. a variable, an expression, or a missing argument), because guessing
/// a target there would be unsound.
fn extract_codeunit_run_target(
    member_call_suffix: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let arg_list = member_call_suffix.child_by_field_name("call")?;
    let text = arg_list.utf8_text(source).ok()?;
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?.trim();
    // First top-level argument (the codeunit reference). The literal form has
    // no nested commas, so a plain split on ',' is sufficient here.
    let first = inner.split(',').next()?.trim();
    parse_codeunit_ref(first)
}

/// Parse a `Codeunit::<name>` reference into the bare object name.
///
/// `Codeunit::"Sales-Post"` → `Sales-Post`; `Codeunit::Worker` → `Worker`.
/// Returns `None` when the text is not a `Codeunit::` reference.
fn parse_codeunit_ref(text: &str) -> Option<String> {
    if !text.trim().to_lowercase().starts_with("codeunit::") {
        return None;
    }
    let cleaned = al_syntax::clean_attr_arg(text);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Populate call graph edges for a single procedure.
///
/// Extracts call sites from the procedure body, then:
/// - `BareCall` → resolves against the same object's methods in `insight`.
/// - `MemberCall` → resolves object against `symbols`, then finds method in `insight`.
/// - `RecordOp` (run_trigger=true) → resolves variable to table via `var_types`,
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
    let call_sites = extract_call_sites(tree, source, procedure_name);
    let var_types = extract_procedure_var_types(tree, source, procedure_name);
    // Collect object-typed variable declarations (codeunit / page /
    // report / xmlport / query / interface), not just Record. Used to resolve
    // `MyVar.Method()` where `MyVar` is e.g. `Codeunit "Sales-Post"` — the
    // prior code looked up `MyVar` itself in the symbol index, only matching
    // when the variable name happened to equal a real object name.
    let object_var_types = extract_procedure_object_var_types(tree, source, procedure_name);

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
            CallSite::RecordOp {
                variable,
                op,
                run_trigger: true,
            } => {
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
            CallSite::RecordOp {
                run_trigger: false, ..
            } => {}
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
fn callable_node_id(
    insight: &InsightGraph,
    object_kind: ObjectKind,
    object_name: &str,
    procedure_name: &str,
) -> Option<NodeId> {
    let object = object_name.to_lowercase();
    let member = procedure_name.to_lowercase();
    [
        NodeKey::Procedure(object_kind, object.clone(), member.clone()),
        NodeKey::Subscriber(object_kind, object.clone(), member.clone()),
        NodeKey::Event(object_kind, object, member),
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
fn link_event_subscribers(caller_id: NodeId, event_id: NodeId, call_graph: &mut CallGraph) {
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
fn find_interface_implementors(
    symbols: &SymbolIndex,
    interface_name: &str,
) -> Vec<Arc<SymbolEntry>> {
    let target = interface_name.trim_matches('"');
    symbols
        .get_by_kind(ObjectKind::Codeunit)
        .into_iter()
        .filter(|entry| {
            entry
                .implements
                .iter()
                .any(|iface| iface.trim_matches('"').eq_ignore_ascii_case(target))
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
fn record_op_event_names(op: RecordOp) -> (String, String) {
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

fn count_call_suffixes(root: tree_sitter::Node, count: &mut usize) {
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
) {
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

        let ok: ObjectKind = match info.kind.parse() {
            Ok(k) => k,
            Err(_) => continue,
        };

        let id = info.id.unwrap_or(0) as i32;
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

        let (source, tree) = match file_index.get_cached_parse(path) {
            Some(pair) => pair,
            None => continue,
        };

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
}

/// Extract `FieldSymbol` data from table/tableextension field sections.
///
/// Fields parse as `object_section` nodes with keyword `field` and a
/// parenthesized `(ID; Name; Type)` triplet. Used by the workspace
/// enrichment pass so scaffolding (`generate page --table`) works against
/// the user's own tables.
fn extract_fields_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
) -> Vec<al_symbols::FieldSymbol> {
    let mut fields = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_section" {
            if let Some(kw) = node.child_by_field_name("keyword") {
                if kw
                    .utf8_text(source)
                    .is_ok_and(|t| t.eq_ignore_ascii_case("field"))
                {
                    if let Some(f) = field_symbol_from_section(node, source) {
                        fields.push(f);
                    }
                    // Field bodies hold properties/triggers, not nested fields.
                    continue;
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    // The explicit stack visits siblings in reverse order; sort for stable,
    // declaration-order output.
    fields.sort_by_key(|f| f.id);
    fields
}

/// Extract the `extends` target from an object declaration, if any.
///
/// The grammar emits `extends X` either as `object_modifier` (with
/// `modifier`/`target` fields) or — what real headers actually produce —
/// as `implements_clause` (positional `metadata_keyword` + `name` children,
/// shared between `implements` and `extends`). Needed so workspace extension
/// objects participate in composition (`composed table <base>`) once
/// registered in the SymbolIndex.
fn info_extends_from_tree(root: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "object_modifier" | "implements_clause") {
            let mut kw_cursor = node.walk();
            let keyword_node = node.child_by_field_name("modifier").or_else(|| {
                node.children(&mut kw_cursor)
                    .find(|c| c.kind() == "metadata_keyword")
            });
            let keyword = keyword_node
                .and_then(|m| m.utf8_text(source).ok())
                .unwrap_or("");
            if keyword.trim().eq_ignore_ascii_case("extends") {
                let mut tgt_cursor = node.walk();
                let target_node = node
                    .child_by_field_name("target")
                    .or_else(|| node.children(&mut tgt_cursor).find(|c| c.kind() == "name"));
                return target_node
                    .and_then(|t| t.utf8_text(source).ok())
                    .map(|t| t.trim().trim_matches('"').to_string());
            }
        }
        // The extends clause lives in the object header — don't descend into
        // object bodies (procedure code can't contain these clause nodes).
        if node.kind() != "object_body" {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    None
}

/// Extract the interface names from an object's `implements` clause.
///
/// The grammar emits only the *first* interface inside `implements_clause`
/// (`metadata_keyword` + `name`); any further comma-separated interfaces appear
/// as sibling `identifier`/`quoted_identifier` tokens of the
/// `object_declaration`. We collect both. Names are returned unquoted.
///
/// `object_name` selects the matching object when a file declares more than one
/// (falls back to the first object). Mirrors [`info_extends_from_tree`]; kept
/// separate because `extends` and `implements` share the `implements_clause`
/// node but carry different keywords.
fn info_implements_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
    object_name: &str,
) -> Vec<String> {
    let want = object_name.trim().trim_matches('"').to_lowercase();
    let mut cursor = root.walk();
    let objects: Vec<tree_sitter::Node> = root
        .children(&mut cursor)
        .filter(|c| c.kind() == "object_declaration")
        .collect();

    let chosen = objects
        .iter()
        .find(|o| {
            object_decl_name(**o, source)
                .map(|n| n.to_lowercase() == want)
                .unwrap_or(false)
        })
        .or_else(|| objects.first());

    match chosen {
        Some(obj) => collect_implements_from_object(*obj, source),
        None => Vec::new(),
    }
}

/// Best-effort name of an `object_declaration` node (unquoted).
fn object_decl_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    if let Some(n) = node.child_by_field_name("name") {
        if let Ok(t) = n.utf8_text(source) {
            return Some(t.trim().trim_matches('"').to_string());
        }
    }
    // Fallback: first identifier-like child (the integer id is skipped — not
    // identifier-like — so the first match is the object name).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "quoted_identifier" | "identifier" | "name_or_keyword" | "name"
        ) {
            if let Ok(t) = child.utf8_text(source) {
                return Some(t.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn collect_implements_from_object(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut result = Vec::new();
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();

    let mut i = 0;
    while i < children.len() {
        let child = children[i];
        if child.kind() == "implements_clause" {
            let mut kc = child.walk();
            let keyword = child
                .children(&mut kc)
                .find(|c| c.kind() == "metadata_keyword")
                .and_then(|m| m.utf8_text(source).ok())
                .unwrap_or("");
            if keyword.trim().eq_ignore_ascii_case("implements") {
                // The single name inside the clause...
                let mut nc = child.walk();
                if let Some(name_node) = child.children(&mut nc).find(|c| {
                    matches!(
                        c.kind(),
                        "name" | "name_or_keyword" | "quoted_identifier" | "identifier"
                    )
                }) {
                    if let Ok(t) = name_node.utf8_text(source) {
                        push_interface(&mut result, t);
                    }
                }
                // Include any trailing `, IBar` siblings the grammar leaves at
                // the object_declaration level.
                let mut j = i + 1;
                while j < children.len() {
                    match children[j].kind() {
                        "comma" => j += 1,
                        "identifier" | "quoted_identifier" | "name" | "name_or_keyword" => {
                            if let Ok(t) = children[j].utf8_text(source) {
                                push_interface(&mut result, t);
                            }
                            j += 1;
                        }
                        _ => break,
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    result
}

fn push_interface(result: &mut Vec<String>, raw: &str) {
    let clean = raw.trim().trim_matches('"').to_string();
    if !clean.is_empty() {
        result.push(clean);
    }
}

/// Parse one `field(ID; Name; Type)` section header into a `FieldSymbol`.
fn field_symbol_from_section(
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<al_symbols::FieldSymbol> {
    let mut cursor = node.walk();
    let paren = node
        .children(&mut cursor)
        .find(|c| c.kind() == "parenthesized_block")?;
    let text = paren.utf8_text(source).ok()?;
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?;
    let mut parts = inner.splitn(3, ';');
    let id: i32 = parts.next()?.trim().parse().ok()?;
    let name = parts.next()?.trim().trim_matches('"').to_string();
    let type_name = parts.next()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(al_symbols::FieldSymbol {
        id,
        name,
        type_name,
        properties: Vec::new(),
    })
}

fn extract_methods_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
) -> Vec<al_symbols::MethodSymbol> {
    let mut methods = Vec::new();
    collect_methods_recursive(root, source, &mut methods);
    methods
}

fn collect_methods_recursive(
    root: tree_sitter::Node,
    source: &[u8],
    methods: &mut Vec<al_symbols::MethodSymbol>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "procedure_declaration" | "trigger_declaration" => {
                if let Some(method) = extract_method_symbol(node, source) {
                    methods.push(method);
                }
                // Do not recurse into procedure body
            }
            _ => {
                let mut cursor = node.walk();
                stack.extend(node.children(&mut cursor));
            }
        }
    }
}

fn extract_method_symbol(
    proc_node: tree_sitter::Node,
    source: &[u8],
) -> Option<al_symbols::MethodSymbol> {
    let name_node = proc_node.child_by_field_name("name")?;
    let proc_name = name_node
        .utf8_text(source)
        .ok()?
        .trim_matches('"')
        .trim()
        .to_string();
    if proc_name.is_empty() {
        return None;
    }

    let is_local = has_local_modifier(proc_node, source);
    let attributes = collect_procedure_attributes(proc_node, source);
    let al_attrs: Vec<al_symbols::AttributeSymbol> = attributes
        .iter()
        .map(|(name, args_text)| {
            let arguments = parse_attr_args_from_text(args_text);
            al_symbols::AttributeSymbol {
                name: name.clone(),
                arguments,
            }
        })
        .collect();

    let parameters = extract_parameters_from_proc(proc_node, source);

    let return_type = extract_return_type(proc_node, source);

    Some(al_symbols::MethodSymbol {
        name: proc_name,
        parameters,
        return_type,
        attributes: al_attrs,
        is_local,
    })
}

fn extract_parameters_from_proc(
    proc_node: tree_sitter::Node,
    source: &[u8],
) -> Vec<al_symbols::ParameterSymbol> {
    let mut params = Vec::new();
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "parameter_list" {
            let mut inner = child.walk();
            for param_node in child.children(&mut inner) {
                if param_node.kind() == "parameter" {
                    if let Some(p) = extract_single_parameter(param_node, source) {
                        params.push(p);
                    }
                }
            }
            break;
        }
    }
    params
}

fn extract_single_parameter(
    param_node: tree_sitter::Node,
    source: &[u8],
) -> Option<al_symbols::ParameterSymbol> {
    let mut name: Option<String> = None;
    let mut type_name = String::new();
    let mut is_var = false;

    let mut cursor = param_node.walk();
    for child in param_node.children(&mut cursor) {
        let kind = child.kind();
        if kind.starts_with("kw_var") || kind == "kw_var" {
            is_var = true;
        } else if (kind == "name" || kind == "name_or_keyword") && name.is_none() {
            name = child
                .utf8_text(source)
                .ok()
                .map(|s| s.trim_matches('"').to_string());
        } else if kind == "type_reference" {
            type_name = child.utf8_text(source).ok().unwrap_or("").to_string();
        }
    }

    Some(al_symbols::ParameterSymbol {
        name: name?,
        type_name,
        is_var,
    })
}

/// Extract return type from a procedure declaration.
///
/// Relies on the AL grammar putting the parameter list ahead of the return
/// type as named children — by the time a `type_reference` named child
/// appears, parameter `type_reference` nodes have already been consumed via
/// the `parameter_list` parent. The previous comment ("Check if preceded by
/// `:`") was aspirational and not implemented; the grammar's child ordering
/// makes that check unnecessary in practice.
fn extract_return_type(proc_node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        // `return_type` is a dedicated grammar node when present; the legacy
        // `type_reference` fallback exists for grammars that emitted a bare
        // type reference without the wrapper. Either path is the return type
        // because parameter type references are nested under `parameter_list`,
        // not direct children of the procedure node.
        if child.kind() == "return_type" || child.kind() == "type_reference" {
            let text = child.utf8_text(source).ok()?.trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Parse attribute arguments from the raw attribute text.
/// Input: `[IntegrationEvent(false, false)]` → `["false", "false"]`
fn parse_attr_args_from_text(attr_text: &str) -> Vec<String> {
    let start = match attr_text.find('(') {
        Some(i) => i + 1,
        None => return Vec::new(),
    };
    let end = match attr_text.rfind(')') {
        Some(i) => i,
        None => return Vec::new(),
    };
    if start >= end {
        return Vec::new();
    }
    let inner = &attr_text[start..end];
    inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Iteratively walk the AST registering procedure/trigger declarations.
///
/// Uses an explicit stack to avoid unbounded recursion on deeply nested AL.
/// When a procedure
/// or trigger node is found, it is dispatched but its body is NOT pushed
/// onto the stack — nested procedures inside a procedure body are not legal
/// AL anyway.
fn register_procedures_from_tree(
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

fn register_single_procedure(
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
        Some(t) => t.trim_matches('"').trim().to_string(),
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
        .any(|(name, _)| name.eq_ignore_ascii_case(super::attr_names::INTEGRATION_EVENT));
    let is_business_event = attributes
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(super::attr_names::BUSINESS_EVENT));
    let is_subscriber = attributes
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(super::attr_names::EVENT_SUBSCRIBER));
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

fn has_local_modifier(proc_node: tree_sitter::Node, source: &[u8]) -> bool {
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
fn parse_subscriber_target_from_attrs(attrs: &[(String, String)]) -> (String, String) {
    for (name, args_text) in attrs {
        // Case-insensitive: see the corresponding comment at the procedure-attribute
        // collection site — raw tree-sitter text preserves source case.
        if name.eq_ignore_ascii_case(super::attr_names::EVENT_SUBSCRIBER) {
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

/// Populate call edges across all workspace files.
///
/// Algorithm:
/// 1. Score each file by fanout (call count).
/// 2. Resolve Tier 1 (score >= 5 or top 20%) eagerly.
/// 3. Return the number of procedures resolved.
///
/// # Panics
/// Does not panic. Missing symbols or nodes are silently skipped.
pub fn populate_workspace_call_edges(
    file_index: &FileIndex,
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) -> usize {
    let mut file_scores: Vec<(
        std::path::PathBuf,
        String,
        tree_sitter::Tree,
        al_source::file_index::CachedObjectInfo,
        usize,
    )> = Vec::new();

    for entry in file_index.object_info.iter() {
        let path = entry.key().clone();
        let info = entry.value().clone();

        let (source, tree) = match file_index.get_cached_parse(&path) {
            Some(pair) => pair,
            None => continue,
        };

        let score = fanout_score(&tree);
        file_scores.push((path, source, tree, info, score));
    }

    if file_scores.is_empty() {
        return 0;
    }

    let threshold = tier1_threshold(&file_scores);

    let mut resolved = 0;

    for (_, source, tree, info, score) in &file_scores {
        if *score < threshold {
            continue;
        }

        let ok: ObjectKind = match info.kind.parse() {
            Ok(k) => k,
            Err(_) => continue,
        };

        let procedures = collect_procedure_names_from_tree(tree.root_node(), source.as_bytes());
        for proc_name in procedures {
            if let Some(proc_id) = callable_node_id(insight, ok, &info.name, &proc_name) {
                if call_graph.resolution_state(proc_id) == EdgeResolutionState::Unresolved {
                    call_graph.set_resolution_state(proc_id, EdgeResolutionState::Resolving);

                    // We need a mutable insight... but we're working with immutable here.
                    // populate_call_edges_for_procedure takes &InsightGraph + &mut CallGraph.
                    populate_call_edges_for_procedure(
                        tree, source, ok, &info.name, &proc_name, symbols, insight, call_graph,
                    );

                    call_graph.set_resolution_state(proc_id, EdgeResolutionState::Resolved);
                    resolved += 1;
                }
            }
        }
    }

    resolved
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
/// # Panics
/// Does not panic. Missing symbols or nodes are silently skipped.
pub fn resolve_all_workspace_call_edges(
    file_index: &FileIndex,
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) -> usize {
    let mut resolved = 0;

    for entry in file_index.object_info.iter() {
        let path = entry.key().clone();
        let info = entry.value().clone();

        let (source, tree) = match file_index.get_cached_parse(&path) {
            Some(pair) => pair,
            None => continue,
        };

        let ok: ObjectKind = match info.kind.parse() {
            Ok(k) => k,
            Err(_) => continue,
        };

        let procedures = collect_procedure_names_from_tree(tree.root_node(), source.as_bytes());
        for proc_name in procedures {
            if let Some(proc_id) = callable_node_id(insight, ok, &info.name, &proc_name) {
                if call_graph.resolution_state(proc_id) != EdgeResolutionState::Resolved {
                    populate_call_edges_for_procedure(
                        &tree, &source, ok, &info.name, &proc_name, symbols, insight, call_graph,
                    );
                    call_graph.set_resolution_state(proc_id, EdgeResolutionState::Resolved);
                    resolved += 1;
                }
            }
        }
    }

    resolved
}

/// Compute the Tier 1 fanout threshold.
///
/// A file is Tier 1 if its score >= 5 or its score is in the top 20%.
fn tier1_threshold(
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

fn collect_procedure_names_from_tree(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    collect_procedure_names_from_node(node, source, &mut names);
    names
}

fn collect_procedure_names_from_node(
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
                            let name = text.trim_matches('"').trim().to_string();
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

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{AttributeSymbol, MethodSymbol, ObjectKind, SymbolEntry, SymbolIndex};

    #[test]
    fn extract_methods_captures_preceding_sibling_attributes() {
        let source = r#"codeunit 50101 "Test Event Publisher"
{
    [IntegrationEvent(false, false)]
    procedure OnBeforeProcess(var InputValue: Text; var IsHandled: Boolean)
    begin
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
    local procedure HandlePost()
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let methods = extract_methods_from_tree(result.tree.root_node(), source.as_bytes());

        let publisher = methods
            .iter()
            .find(|m| m.name == "OnBeforeProcess")
            .expect("OnBeforeProcess method must be extracted");
        assert!(
            publisher
                .attributes
                .iter()
                .any(|a| a.name.eq_ignore_ascii_case("IntegrationEvent")),
            "must capture the [IntegrationEvent] attribute (preceding sibling); got: {:?}",
            publisher.attributes
        );

        let subscriber = methods
            .iter()
            .find(|m| m.name == "HandlePost")
            .expect("HandlePost method must be extracted");
        assert!(
            subscriber
                .attributes
                .iter()
                .any(|a| a.name.eq_ignore_ascii_case("EventSubscriber")),
            "must capture the [EventSubscriber] attribute; got: {:?}",
            subscriber.attributes
        );
    }

    #[test]
    fn workspace_enrichment_extracts_table_fields() {
        let source = r#"table 50100 "Test Customer"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            Caption = 'No.';
        }
        field(2; Name; Text[100])
        {
        }
    }
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let fields = extract_fields_from_tree(result.tree.root_node(), source.as_bytes());
        assert_eq!(fields.len(), 2, "both fields must be extracted: {fields:?}");
        assert_eq!(fields[0].id, 1);
        assert_eq!(fields[0].name, "No.");
        assert_eq!(fields[0].type_name, "Code[20]");
        assert_eq!(fields[1].id, 2);
        assert_eq!(fields[1].name, "Name");
        assert_eq!(fields[1].type_name, "Text[100]");
    }

    #[test]
    fn extends_target_extracted_from_extension_header() {
        let source = r#"tableextension 50100 "Test Customer Ext" extends "Test Customer"
{
    fields
    {
        field(50100; "Custom Field"; Text[50])
        {
        }
    }
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let target = info_extends_from_tree(result.tree.root_node(), source.as_bytes());
        assert_eq!(
            target.as_deref(),
            Some("Test Customer"),
            "extends target must be extracted from the object header"
        );
    }

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

    fn make_table(id: i32, name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Table,
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
    fn extract_var_types_from_procedure() {
        let source = r#"codeunit 50100 "Test CU"
{
    procedure DoWork()
    var
        Cust: Record "Customer";
        SalesHdr: Record "Sales Header";
        Counter: Integer;
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let types = extract_procedure_var_types(&result.tree, source, "DoWork");

        assert!(types.contains_key("cust"), "Should find 'cust' variable");
        assert_eq!(types.get("cust").map(|s| s.as_str()), Some("Customer"));

        assert!(
            types.contains_key("saleshdr"),
            "Should find 'saleshdr' variable"
        );
        assert_eq!(
            types.get("saleshdr").map(|s| s.as_str()),
            Some("Sales Header")
        );

        assert!(
            !types.contains_key("counter"),
            "Integer vars should not appear"
        );
    }

    #[test]
    fn extract_object_var_types_finds_codeunit_and_page_and_report_vars() {
        let source = r#"codeunit 50100 "Test CU"
{
    procedure DoWork()
    var
        SalesPost: Codeunit "Sales-Post";
        MyPage: Page "Customer List";
        Rep: Report "Sales Order";
        Counter: Integer;
        Cust: Record "Customer";
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let types = extract_procedure_object_var_types(&result.tree, source, "DoWork");

        assert_eq!(
            types.get("salespost").map(|s| s.as_str()),
            Some("Sales-Post")
        );
        assert_eq!(
            types.get("mypage").map(|s| s.as_str()),
            Some("Customer List")
        );
        assert_eq!(types.get("rep").map(|s| s.as_str()), Some("Sales Order"));
        // Integer not an object kind — excluded.
        assert!(!types.contains_key("counter"));
        // Record is intentionally NOT here (use extract_procedure_var_types
        // for that — the trigger path handles record method calls separately).
        assert!(!types.contains_key("cust"));
    }

    #[test]
    fn extract_object_var_types_picks_up_parameters() {
        // Codeunit-typed parameters should be captured too — calling
        // `SalesPostParam.Method()` inside the body needs the same lookup.
        let source = r#"codeunit 50100 "Test CU"
{
    procedure DoWork(SalesPostParam: Codeunit "Sales-Post"; var Cust: Record "Customer")
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let types = extract_procedure_object_var_types(&result.tree, source, "DoWork");
        assert_eq!(
            types.get("salespostparam").map(|s| s.as_str()),
            Some("Sales-Post")
        );
        // Record param not captured here (separate path).
        assert!(!types.contains_key("cust"));
    }

    #[test]
    fn extract_var_types_returns_empty_for_unknown_procedure() {
        let source = r#"codeunit 50100 "Test CU"
{
    procedure DoWork()
    var
        Cust: Record "Customer";
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let types = extract_procedure_var_types(&result.tree, source, "NonExistentProc");
        assert!(types.is_empty());
    }

    #[test]
    fn extract_call_sites_from_procedure() {
        let source = r#"codeunit 50100 "Test CU"
{
    procedure DoWork()
    var
        Cust: Record "Customer";
        SalesPost: Codeunit "Sales-Post";
    begin
        SalesPost.Post();
        Cust.Insert(true);
        Cust.Modify();
        Cust.Delete(false);
        DoSomething();
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let sites = extract_call_sites(&result.tree, source, "DoWork");

        assert!(!sites.is_empty(), "Should find call sites");

        let bare_calls: Vec<_> = sites
            .iter()
            .filter_map(|s| {
                if let CallSite::BareCall { name } = s {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            bare_calls
                .iter()
                .any(|&n| n.eq_ignore_ascii_case("DoSomething")),
            "Should find DoSomething() bare call"
        );

        let record_ops: Vec<_> = sites
            .iter()
            .filter_map(|s| {
                if let CallSite::RecordOp {
                    variable,
                    op,
                    run_trigger,
                } = s
                {
                    Some((variable.as_str(), *op, *run_trigger))
                } else {
                    None
                }
            })
            .collect();

        assert!(
            record_ops
                .iter()
                .any(|(v, op, rt)| v.eq_ignore_ascii_case("Cust")
                    && *op == RecordOp::Insert
                    && *rt),
            "Should find Cust.Insert(true)"
        );
        assert!(
            record_ops
                .iter()
                .any(|(v, op, _rt)| v.eq_ignore_ascii_case("Cust") && *op == RecordOp::Modify),
            "Should find Cust.Modify()"
        );
        assert!(
            record_ops
                .iter()
                .any(|(v, op, rt)| v.eq_ignore_ascii_case("Cust")
                    && *op == RecordOp::Delete
                    && !*rt),
            "Should find Cust.Delete(false) with run_trigger=false"
        );

        let member_calls: Vec<_> = sites
            .iter()
            .filter_map(|s| {
                if let CallSite::MemberCall { object, method } = s {
                    Some((object.as_str(), method.as_str()))
                } else {
                    None
                }
            })
            .collect();
        assert!(
            member_calls.iter().any(|(o, m)| o.eq_ignore_ascii_case("SalesPost") && m.eq_ignore_ascii_case("Post")),
            "Should find SalesPost.Post() member call"
        );
    }

    #[test]
    fn populate_call_edges_for_file() {
        let source = r#"codeunit 50100 "My CU"
{
    procedure DoPost()
    var
        Cust: Record "Customer";
    begin
        Cust.Insert(true);
        CheckHeader();
    end;

    procedure CheckHeader()
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);

        let index = SymbolIndex::new();
        index.add_entries(&[
            make_codeunit(
                50100,
                "My CU",
                vec![regular_method("DoPost"), regular_method("CheckHeader")],
            ),
            make_table(
                18,
                "Customer",
                vec![
                    integration_event("OnBeforeInsertEvent"),
                    integration_event("OnAfterInsertEvent"),
                ],
            ),
        ]);

        let mut insight = InsightGraph::new();
        insight.build_from_index(&index);
        let mut call_graph = CallGraph::build_from_insight(&insight);

        populate_call_edges_for_procedure(
            &result.tree,
            source,
            ObjectKind::Codeunit,
            "My CU",
            "DoPost",
            &index,
            &insight,
            &mut call_graph,
        );

        let caller_key = NodeKey::Procedure(
            ObjectKind::Codeunit,
            "my cu".to_string(),
            "dopost".to_string(),
        );
        let caller_id =
            CallGraph::node_id_for(&insight, &caller_key).expect("caller node should exist");

        let callees = call_graph.callees_of(caller_id);
        assert!(!callees.is_empty(), "DoPost should have outgoing edges");

        let direct_calls: Vec<_> = callees
            .iter()
            .filter(|e| e.kind == super::super::index::EdgeKind::DirectCall)
            .collect();
        assert!(
            !direct_calls.is_empty(),
            "Should have at least one direct call (CheckHeader)"
        );

        let triggers: Vec<_> = callees
            .iter()
            .filter(|e| e.kind == super::super::index::EdgeKind::RecordTrigger)
            .collect();
        assert!(
            !triggers.is_empty(),
            "Should have RecordTrigger edges from Cust.Insert(true)"
        );
    }

    #[test]
    fn fanout_score_counts_calls() {
        let source = r#"codeunit 50100 "Test CU"
{
    procedure DoThree()
    var
        Obj: Codeunit "Other";
    begin
        Obj.Method1();
        Obj.Method2();
        DoLocal();
    end;

    local procedure DoLocal()
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let score = fanout_score(&result.tree);
        assert!(score >= 3, "Score should be at least 3 (found {score})");
    }

    #[test]
    fn fanout_score_empty_codeunit() {
        let source = r#"codeunit 50100 "Empty CU" { }"#;
        let result = al_syntax::AlParser::parse_quick(source);
        let score = fanout_score(&result.tree);
        assert_eq!(score, 0, "Empty codeunit should have fanout score 0");
    }

    #[test]
    fn workspace_subscriber_appears_in_event_trace() {
        let publisher = r#"codeunit 50100 "Trace Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterDoThing(var Done: Boolean)
    begin
    end;
}
"#;
        let subscriber = r#"codeunit 50101 "Trace Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Trace Publisher", OnAfterDoThing, '', false, false)]
    local procedure HandleDoThing(var Done: Boolean)
    begin
    end;
}
"#;
        let file_index = al_source::file_index::FileIndex::new();
        file_index.add_file(
            std::path::PathBuf::from("/ws/Publisher.Codeunit.al"),
            publisher.to_string(),
        );
        file_index.add_file(
            std::path::PathBuf::from("/ws/Subscriber.Codeunit.al"),
            subscriber.to_string(),
        );

        let symbols = SymbolIndex::new();
        let mut insight = InsightGraph::new();
        register_workspace_nodes(&file_index, &symbols, &mut insight);

        let steps = crate::search::trace_event(&insight, "OnAfterDoThing", 10);
        assert!(
            steps
                .iter()
                .any(|s| s.edge_type == "origin" && s.object == "Trace Publisher"),
            "trace must find the publishing origin, got: {steps:?}"
        );
        assert!(
            steps.iter().any(|s| s.edge_type == "subscribes_to"
                && s.object == "Trace Subscriber"
                && s.name == "HandleDoThing"),
            "trace must descend to the workspace subscriber, got: {steps:?}"
        );
    }

    #[test]
    fn implicit_table_event_subscriber_is_traceable() {
        let table = r#"table 50100 "Trace Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
}
"#;
        let subscriber = r#"codeunit 50102 "Table Event Subs"
{
    [EventSubscriber(ObjectType::Table, Database::"Trace Table", OnAfterInsertEvent, '', false, false)]
    local procedure OnAfterInsert(var Rec: Record "Trace Table"; RunTrigger: Boolean)
    begin
    end;
}
"#;
        let file_index = al_source::file_index::FileIndex::new();
        file_index.add_file(
            std::path::PathBuf::from("/ws/TraceTable.Table.al"),
            table.to_string(),
        );
        file_index.add_file(
            std::path::PathBuf::from("/ws/TableEventSubs.Codeunit.al"),
            subscriber.to_string(),
        );

        let symbols = SymbolIndex::new();
        let mut insight = InsightGraph::new();
        register_workspace_nodes(&file_index, &symbols, &mut insight);

        let steps = crate::search::trace_event(&insight, "OnAfterInsertEvent", 10);
        assert!(
            steps
                .iter()
                .any(|s| s.edge_type == "subscribes_to" && s.object == "Table Event Subs"),
            "implicit table event must trace to its subscriber, got: {steps:?}"
        );
    }

    #[test]
    fn register_workspace_nodes_detects_events_and_subscribers() {
        let source = r#"codeunit 50100 "Test Publisher"
{
    [IntegrationEvent(false, false)]
    procedure OnBeforeTest(var Handled: Boolean)
    begin
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
    procedure HandleAfterPost()
    begin
    end;

    procedure NormalProcedure()
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(source);

        let _index = SymbolIndex::new();
        let mut insight = InsightGraph::new();

        let source_bytes = source.as_bytes();
        let obj_key = NodeKey::Object(ObjectKind::Codeunit, "test publisher".to_string());
        let obj_idx = insight.ensure_node(
            obj_key,
            InsightNode::Object {
                kind: ObjectKind::Codeunit,
                id: 50100,
                name: "Test Publisher".to_string(),
                package: "workspace".to_string(),
            },
        );

        register_procedures_from_tree(
            result.tree.root_node(),
            source_bytes,
            ObjectKind::Codeunit,
            "Test Publisher",
            obj_idx,
            &mut insight,
        );

        let event_key = NodeKey::Event(
            ObjectKind::Codeunit,
            "test publisher".to_string(),
            "onbeforetest".to_string(),
        );
        assert!(
            insight.get_node(&event_key).is_some(),
            "OnBeforeTest should be registered as an Event node"
        );

        let sub_key = NodeKey::Subscriber(
            ObjectKind::Codeunit,
            "test publisher".to_string(),
            "handleafterpost".to_string(),
        );
        assert!(
            insight.get_node(&sub_key).is_some(),
            "HandleAfterPost should be registered as a Subscriber node"
        );

        let proc_key = NodeKey::Procedure(
            ObjectKind::Codeunit,
            "test publisher".to_string(),
            "normalprocedure".to_string(),
        );
        assert!(
            insight.get_node(&proc_key).is_some(),
            "NormalProcedure should be registered as a Procedure node"
        );
    }

    #[test]
    fn extract_attribute_args_basic() {
        let text = "[EventSubscriber(ObjectType::Codeunit, Codeunit::\"Sales-Post\", 'OnAfterPost', '', false, false)]";
        let args = extract_attribute_args(text);
        assert_eq!(args.len(), 6);
        assert_eq!(args[0], "ObjectType::Codeunit");
        assert_eq!(args[2], "'OnAfterPost'");
    }

    #[test]
    fn clean_attr_arg_strips_prefix_and_quotes() {
        assert_eq!(
            al_syntax::clean_attr_arg("Codeunit::\"Sales-Post\""),
            "Sales-Post"
        );
        assert_eq!(al_syntax::clean_attr_arg("'OnAfterPost'"), "OnAfterPost");
        assert_eq!(al_syntax::clean_attr_arg("  \"My Object\"  "), "My Object");
    }

    #[test]
    fn record_op_from_method_name() {
        assert_eq!(RecordOp::from_method_name("Insert"), Some(RecordOp::Insert));
        assert_eq!(RecordOp::from_method_name("insert"), Some(RecordOp::Insert));
        assert_eq!(RecordOp::from_method_name("MODIFY"), Some(RecordOp::Modify));
        assert_eq!(RecordOp::from_method_name("delete"), Some(RecordOp::Delete));
        assert_eq!(
            RecordOp::from_method_name("Validate"),
            Some(RecordOp::Validate)
        );
        assert_eq!(RecordOp::from_method_name("Post"), None);
    }

    #[test]
    fn parse_subscriber_target_is_case_insensitive() {
        // Regression: prior code used `name == "EventSubscriber"` which silently
        // dropped lower/mixed-case attribute spellings — AL is case-insensitive.
        let attrs = vec![(
            "eventsubscriber".to_string(),
            r#"(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost')"#.to_string(),
        )];
        let (obj, ev) = parse_subscriber_target_from_attrs(&attrs);
        assert_eq!(obj, "Sales-Post");
        assert_eq!(ev, "OnAfterPost");

        let attrs = vec![(
            "EVENTSUBSCRIBER".to_string(),
            r#"(ObjectType::Codeunit, Codeunit::"Foo", 'OnX')"#.to_string(),
        )];
        let (obj, ev) = parse_subscriber_target_from_attrs(&attrs);
        assert_eq!(obj, "Foo");
        assert_eq!(ev, "OnX");
    }

    /// Build a (path, source, tree, info, score) tuple with the given score —
    /// the other fields are placeholder values, only `score` matters for
    /// `tier1_threshold`.
    fn mk_scored_file(
        score: usize,
    ) -> (
        std::path::PathBuf,
        String,
        tree_sitter::Tree,
        al_source::file_index::CachedObjectInfo,
        usize,
    ) {
        let result = al_syntax::AlParser::parse_quick("");
        (
            std::path::PathBuf::from("x"),
            String::new(),
            result.tree,
            al_source::file_index::CachedObjectInfo {
                kind: "codeunit".to_string(),
                id: Some(0),
                name: "X".to_string(),
                range: tree_sitter::Range {
                    start_byte: 0,
                    end_byte: 0,
                    start_point: tree_sitter::Point { row: 0, column: 0 },
                    end_point: tree_sitter::Point { row: 0, column: 0 },
                },
            },
            score,
        )
    }

    #[test]
    fn tier1_threshold_admits_score_five_in_busy_workspace() {
        // Regression: doc said "score >= 5 OR top 20%" but code only honoured
        // the percentile gate. A workspace where the 80th percentile is 20
        // would drop files with scores 5-19 even though they crossed the 5
        // floor. With the fix, threshold = min(percentile, 5) so the 5 floor
        // is always honoured.
        let files: Vec<_> = [0, 0, 0, 0, 0, 5, 8, 10, 20, 40]
            .into_iter()
            .map(mk_scored_file)
            .collect();
        assert_eq!(tier1_threshold(&files), 5);
    }

    #[test]
    fn tier1_threshold_uses_percentile_when_below_five() {
        // Small/quiet workspace where 80th percentile is below 5 — we use
        // the percentile so we don't gate out everything.
        let files: Vec<_> = [0, 1, 2, 2, 3, 3, 3, 3, 4, 4]
            .into_iter()
            .map(mk_scored_file)
            .collect();
        // 80th percentile index is 8 (len * 8 / 10), value is 4. min(4, 5) = 4.
        assert_eq!(tier1_threshold(&files), 4);
    }

    // Indirect and polymorphic dispatch resolution.

    /// Build a fully-resolved (node-complete, all-edges) workspace call graph
    /// from in-memory AL files, exactly as the daemon's affected-test /
    /// coverage paths do.
    fn build_resolved_call_graph(files: &[(&str, &str)]) -> (InsightGraph, CallGraph) {
        let file_index = al_source::file_index::FileIndex::new();
        for (name, content) in files {
            file_index.add_file(std::path::PathBuf::from(name), content.to_string());
        }
        let symbols = SymbolIndex::new();
        let mut insight = InsightGraph::new();
        register_workspace_nodes(&file_index, &symbols, &mut insight);
        let mut cg = CallGraph::build_from_insight(&insight);
        resolve_all_workspace_call_edges(&file_index, &symbols, &insight, &mut cg);
        (insight, cg)
    }

    fn proc_node(insight: &InsightGraph, kind: ObjectKind, object: &str, method: &str) -> NodeId {
        let key = NodeKey::Procedure(kind, object.to_lowercase(), method.to_lowercase());
        CallGraph::node_id_for(insight, &key)
            .unwrap_or_else(|| panic!("missing procedure node {object}.{method}"))
    }

    #[test]
    fn interface_call_reaches_all_implementors() {
        let iface = r#"interface IFoo
{
    procedure Bar()
}
"#;
        let impl_a = r#"codeunit 50101 "Impl A" implements "IFoo"
{
    procedure Bar()
    begin
    end;
}
"#;
        let impl_b = r#"codeunit 50102 "Impl B" implements "IFoo"
{
    procedure Bar()
    begin
    end;
}
"#;
        let caller = r#"codeunit 50100 "Caller CU"
{
    procedure Dispatch()
    var
        Foo: Interface "IFoo";
    begin
        Foo.Bar();
    end;
}
"#;
        let unrelated = r#"codeunit 50103 "Unrelated CU"
{
    procedure Untouched()
    begin
    end;
}
"#;
        let (insight, cg) = build_resolved_call_graph(&[
            ("/ws/IFoo.Interface.al", iface),
            ("/ws/ImplA.Codeunit.al", impl_a),
            ("/ws/ImplB.Codeunit.al", impl_b),
            ("/ws/Caller.Codeunit.al", caller),
            ("/ws/Unrelated.Codeunit.al", unrelated),
        ]);

        let dispatch = proc_node(&insight, ObjectKind::Codeunit, "Caller CU", "Dispatch");
        let bar_a = proc_node(&insight, ObjectKind::Codeunit, "Impl A", "Bar");
        let bar_b = proc_node(&insight, ObjectKind::Codeunit, "Impl B", "Bar");
        let untouched = proc_node(&insight, ObjectKind::Codeunit, "Unrelated CU", "Untouched");

        // Reverse reachability (affected-test semantics): both implementors'
        // Bar are reached by the dispatching procedure.
        assert!(
            cg.reachable_callers([bar_a]).contains(&dispatch),
            "interface call must reach Impl A.Bar"
        );
        assert!(
            cg.reachable_callers([bar_b]).contains(&dispatch),
            "interface call must reach Impl B.Bar (ALL implementors)"
        );
        // The unrelated codeunit must NOT be pulled in.
        assert!(
            !cg.reachable_callers([bar_a]).contains(&untouched),
            "unrelated codeunit must not be reachable"
        );

        // Forward (coverage semantics): the dispatch site has indirect edges to
        // both implementors and none to the unrelated procedure.
        let callees: Vec<NodeId> = cg
            .callees_of(dispatch)
            .iter()
            .filter(|e| e.kind == super::super::index::EdgeKind::IndirectCall)
            .map(|e| e.to)
            .collect();
        assert!(callees.contains(&bar_a) && callees.contains(&bar_b));
        assert!(!callees.contains(&untouched));
    }

    #[test]
    fn codeunit_run_reaches_onrun() {
        let worker = r#"codeunit 50201 "Worker CU"
{
    trigger OnRun()
    begin
    end;
}
"#;
        let runner = r#"codeunit 50200 "Runner CU"
{
    procedure Kick()
    begin
        Codeunit.Run(Codeunit::"Worker CU");
    end;
}
"#;
        let unrelated = r#"codeunit 50202 "Other CU"
{
    procedure Idle()
    begin
    end;
}
"#;
        let (insight, cg) = build_resolved_call_graph(&[
            ("/ws/Worker.Codeunit.al", worker),
            ("/ws/Runner.Codeunit.al", runner),
            ("/ws/Other.Codeunit.al", unrelated),
        ]);

        let kick = proc_node(&insight, ObjectKind::Codeunit, "Runner CU", "Kick");
        let onrun = proc_node(&insight, ObjectKind::Codeunit, "Worker CU", "OnRun");
        let idle = proc_node(&insight, ObjectKind::Codeunit, "Other CU", "Idle");

        assert!(
            cg.reachable_callers([onrun]).contains(&kick),
            "Codeunit.Run(Codeunit::\"Worker CU\") must reach Worker CU.OnRun"
        );
        assert!(
            !cg.reachable_callers([onrun]).contains(&idle),
            "an unrelated codeunit must not be reachable from OnRun"
        );
    }

    #[test]
    fn published_event_reaches_subscriber() {
        let publisher = r#"codeunit 50300 "Publisher CU"
{
    procedure DoWork()
    begin
        OnAfterDoWork();
    end;

    [IntegrationEvent(false, false)]
    local procedure OnAfterDoWork()
    begin
    end;
}
"#;
        let subscriber = r#"codeunit 50301 "Subscriber CU"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Publisher CU", OnAfterDoWork, '', false, false)]
    local procedure HandleAfterDoWork()
    begin
    end;
}
"#;
        let unrelated = r#"codeunit 50302 "Bystander CU"
{
    procedure Watch()
    begin
    end;
}
"#;
        let (insight, cg) = build_resolved_call_graph(&[
            ("/ws/Publisher.Codeunit.al", publisher),
            ("/ws/Subscriber.Codeunit.al", subscriber),
            ("/ws/Bystander.Codeunit.al", unrelated),
        ]);

        let do_work = proc_node(&insight, ObjectKind::Codeunit, "Publisher CU", "DoWork");
        let handler = CallGraph::node_id_for(
            &insight,
            &NodeKey::Subscriber(
                ObjectKind::Codeunit,
                "subscriber cu".to_string(),
                "handleafterdowork".to_string(),
            ),
        )
        .expect("subscriber node");
        let watch = proc_node(&insight, ObjectKind::Codeunit, "Bystander CU", "Watch");

        // Firing the event from DoWork reaches the subscriber handler.
        assert!(
            cg.reachable_callers([handler]).contains(&do_work),
            "publishing the event must reach its [EventSubscriber] handler"
        );
        // The bystander is untouched.
        assert!(
            !cg.reachable_callers([handler]).contains(&watch),
            "unrelated codeunit must not reach the subscriber"
        );

        // Forward: DoWork has an indirect edge to the subscriber handler.
        let reaches_handler = cg
            .callees_of(do_work)
            .iter()
            .any(|e| e.to == handler && e.kind == super::super::index::EdgeKind::IndirectCall);
        assert!(
            reaches_handler,
            "DoWork → subscriber indirect edge expected"
        );
    }

    #[test]
    fn implements_clause_extracted_from_header() {
        let src = r#"codeunit 50100 "Impl A" implements "IFoo", IBar
{
    procedure Bar()
    begin
    end;
}
"#;
        let result = al_syntax::AlParser::parse_quick(src);
        let ifaces = info_implements_from_tree(result.tree.root_node(), src.as_bytes(), "Impl A");
        assert!(
            ifaces.iter().any(|i| i.eq_ignore_ascii_case("IFoo")),
            "first interface must be captured: {ifaces:?}"
        );
        assert!(
            ifaces.iter().any(|i| i.eq_ignore_ascii_case("IBar")),
            "trailing comma-separated interface must be captured: {ifaces:?}"
        );
    }

    #[test]
    fn codeunit_run_target_parsed() {
        // Literal forms resolve; a variable argument does not.
        assert_eq!(
            parse_codeunit_ref("Codeunit::\"Sales-Post\"").as_deref(),
            Some("Sales-Post")
        );
        assert_eq!(
            parse_codeunit_ref("Codeunit::Worker").as_deref(),
            Some("Worker")
        );
        assert_eq!(parse_codeunit_ref("SomeVariable"), None);
        assert!(is_codeunit_run_method("Run"));
        assert!(is_codeunit_run_method("runmodal"));
        assert!(!is_codeunit_run_method("Post"));
    }
}
