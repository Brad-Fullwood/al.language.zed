# Suggest Event Rewrite — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the naive NLP-based `suggest_event` with structured integration point discovery that traces call/event chains from the InsightGraph and CallGraph.

**Architecture:** A new `insight/calls.rs` module extracts `Calls` and `Triggers` edges from workspace tree-sitter ASTs, populating the existing `CallGraph`. The rewritten `suggest_event.rs` accepts structured queries (object+procedure, table, event) and walks the populated graph to find reachable integration points. Call edge extraction runs lazily in the background after symbol indexing, with a tiered strategy: high-fanout objects eagerly, others on-demand.

**Tech Stack:** Rust, tree-sitter (AL grammar), petgraph, serde, clap

**Spec:** `docs/superpowers/specs/2026-03-26-suggest-event-rewrite-design.md`

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/al-core/src/insight/graph.rs` | Modify | Add `Triggers` edge variant, `remove_edges_from()` |
| `crates/al-core/src/insight/index.rs` | Modify | Add `remove_edges_from()`, `add_trigger()`, resolution state tracking |
| `crates/al-core/src/insight/calls.rs` | Create | Call edge extraction from tree-sitter ASTs |
| `crates/al-core/src/insight/mod.rs` | Modify | Export `calls` module |
| `crates/al-core/src/queries/suggest_event.rs` | Rewrite | Structured query types + graph-walking query logic |
| `crates/al-core/src/workspace.rs` | Modify | Add CallGraph storage, background build trigger, invalidation |
| `crates/al-lsp/src/daemon/insight_dispatch.rs` | Modify | New structured JSON params for `suggestEvent` |
| `crates/al-lsp/src/daemon/mod.rs` | No change | Route name `"suggestEvent"` stays the same |
| `crates/al-cli/src/commands/insight.rs` | Modify | New structured CLI flags + output format |
| `crates/al-cli/src/main.rs` | Modify | New clap arg definitions |

---

### Task 1: Add `Triggers` edge variant and edge removal to InsightGraph

**Files:**
- Modify: `crates/al-core/src/insight/graph.rs`

- [ ] **Step 1: Write failing test for `Triggers` edge variant**

In `graph.rs` tests section, add:

```rust
#[test]
fn triggers_edge_display() {
    assert_eq!(format!("{}", InsightEdge::Triggers), "triggers");
}
```

Run: `cargo test -p al-core -- triggers_edge_display`
Expected: FAIL — `Triggers` variant doesn't exist.

- [ ] **Step 2: Add `Triggers` variant to `InsightEdge`**

In `InsightEdge` enum (after `RelatesTo`):

```rust
/// Procedure A triggers table events on Record B (Insert/Modify/Delete/Validate).
Triggers,
```

In `Display` impl, add the arm:

```rust
InsightEdge::Triggers => write!(f, "triggers"),
```

Run: `cargo test -p al-core -- triggers_edge_display`
Expected: PASS

- [ ] **Step 3: Write failing test for `remove_edges_from`**

```rust
#[test]
fn remove_edges_from_clears_outgoing() {
    let mut graph = InsightGraph::new();
    let a = graph.ensure_node(
        NodeKey::Procedure(ObjectKind::Codeunit, "cu".into(), "a".into()),
        InsightNode::Procedure {
            object_kind: ObjectKind::Codeunit,
            object_name: "CU".into(),
            name: "A".into(),
            is_local: false,
        },
    );
    let b = graph.ensure_node(
        NodeKey::Procedure(ObjectKind::Codeunit, "cu".into(), "b".into()),
        InsightNode::Procedure {
            object_kind: ObjectKind::Codeunit,
            object_name: "CU".into(),
            name: "B".into(),
            is_local: false,
        },
    );
    graph.add_edge(a, b, InsightEdge::Calls);
    assert_eq!(graph.edge_count(), 1);

    graph.remove_edges_from(a);
    assert_eq!(graph.edge_count(), 0);
}
```

Run: `cargo test -p al-core -- remove_edges_from_clears_outgoing`
Expected: FAIL — method doesn't exist.

- [ ] **Step 4: Implement `remove_edges_from`**

Add to `impl InsightGraph`:

```rust
/// Remove all outgoing edges from `node`. Used for invalidation when
/// a file changes and its call edges need re-extraction.
pub fn remove_edges_from(&mut self, node: NodeIndex) {
    // petgraph doesn't have a "remove all edges from node" method,
    // so collect and remove individually.
    let to_remove: Vec<_> = self.graph
        .edges(node)
        .map(|e| e.id())
        .collect();
    for edge_id in to_remove {
        self.graph.remove_edge(edge_id);
    }
}
```

Run: `cargo test -p al-core -- remove_edges_from_clears_outgoing`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/al-core/src/insight/graph.rs
git commit -m "feat(insight): add Triggers edge variant and remove_edges_from"
```

---

### Task 2: Add trigger edges and resolution state to CallGraph

**Files:**
- Modify: `crates/al-core/src/insight/index.rs`

- [ ] **Step 1: Write failing test for `add_trigger` and `EdgeKind::RecordTrigger`**

```rust
#[test]
fn record_trigger_edge() {
    let index = SymbolIndex::new();
    index.add_entries(&[
        make_codeunit(1, "PostCU", vec![regular_method("DoPost")]),
        make_codeunit(2, "Events", vec![integration_event("OnBeforeInsertEvent")]),
    ]);

    let mut graph = InsightGraph::new();
    graph.build_from_index(&index);
    let mut cg = CallGraph::build_from_insight(&graph);

    let proc_key = NodeKey::Procedure(ObjectKind::Codeunit, "postcu".into(), "dopost".into());
    let event_key = NodeKey::Event(ObjectKind::Codeunit, "events".into(), "onbeforeinsertevent".into());

    let proc_id = CallGraph::node_id_for(&graph, &proc_key).unwrap();
    let event_id = CallGraph::node_id_for(&graph, &event_key).unwrap();

    cg.add_trigger(proc_id, event_id);

    let callees = cg.callees_of(proc_id);
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].kind, EdgeKind::RecordTrigger);
}
```

Run: `cargo test -p al-core -- record_trigger_edge`
Expected: FAIL — `EdgeKind::RecordTrigger` and `add_trigger` don't exist.

- [ ] **Step 2: Add `RecordTrigger` variant and `add_trigger` method**

Add to `EdgeKind`:

```rust
/// A record operation (Insert/Modify/Delete/Validate) triggers table events.
RecordTrigger,
```

Add to `Display` impl:

```rust
EdgeKind::RecordTrigger => write!(f, "record_trigger"),
```

Add to `impl CallGraph`:

```rust
/// Add a record-trigger edge (procedure triggers table event via Insert/Modify/Delete/Validate).
pub fn add_trigger(&mut self, from: NodeId, to: NodeId) {
    let edge = CallEdge { from, to, kind: EdgeKind::RecordTrigger };
    self.insert_edge(edge);
}
```

Run: `cargo test -p al-core -- record_trigger_edge`
Expected: PASS

- [ ] **Step 3: Write failing test for `remove_edges_from`**

```rust
#[test]
fn remove_edges_from_node() {
    let index = SymbolIndex::new();
    index.add_entries(&[make_codeunit(
        1, "MyCU", vec![regular_method("A"), regular_method("B"), regular_method("C")],
    )]);

    let mut graph = InsightGraph::new();
    graph.build_from_index(&index);
    let mut cg = CallGraph::build_from_insight(&graph);

    let a = CallGraph::node_id_for(&graph, &NodeKey::Procedure(ObjectKind::Codeunit, "mycu".into(), "a".into())).unwrap();
    let b = CallGraph::node_id_for(&graph, &NodeKey::Procedure(ObjectKind::Codeunit, "mycu".into(), "b".into())).unwrap();
    let c = CallGraph::node_id_for(&graph, &NodeKey::Procedure(ObjectKind::Codeunit, "mycu".into(), "c".into())).unwrap();

    cg.add_direct_call(a, b);
    cg.add_direct_call(a, c);
    cg.add_direct_call(b, c);
    assert_eq!(cg.edge_count(), 3);

    cg.remove_edges_from(a);
    // Only the b->c edge should remain.
    assert_eq!(cg.edge_count(), 1);
    assert_eq!(cg.callees_of(a).len(), 0);
    assert_eq!(cg.callers_of(b).len(), 0); // a->b was removed
    assert_eq!(cg.callers_of(c).len(), 1); // b->c remains
}
```

Run: `cargo test -p al-core -- remove_edges_from_node`
Expected: FAIL — method doesn't exist.

- [ ] **Step 4: Implement `remove_edges_from` on CallGraph**

```rust
/// Remove all outgoing edges from `node`. Also removes corresponding
/// entries from `incoming` reverse index. Used for invalidation.
pub fn remove_edges_from(&mut self, node: NodeId) {
    if let Some(edges) = self.outgoing.remove(&node) {
        for edge in &edges {
            if let Some(incoming) = self.incoming.get_mut(&edge.to) {
                incoming.retain(|e| e.from != node);
            }
        }
    }
}
```

Run: `cargo test -p al-core -- remove_edges_from_node`
Expected: PASS

- [ ] **Step 5: Add `EdgeResolutionState` tracking**

Add types and methods:

```rust
/// Tracks whether a procedure's call edges have been extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeResolutionState {
    Unresolved,
    Resolving,
    Resolved,
}

impl CallGraph {
    // Add a new field to the struct:
    // resolution: HashMap<NodeId, EdgeResolutionState>,
    // Initialize it in new() and build_from_insight().

    /// Get the resolution state of a node's outgoing call edges.
    pub fn resolution_state(&self, node: NodeId) -> EdgeResolutionState {
        self.resolution.get(&node).copied().unwrap_or(EdgeResolutionState::Unresolved)
    }

    /// Set the resolution state for a node.
    pub fn set_resolution_state(&mut self, node: NodeId, state: EdgeResolutionState) {
        self.resolution.insert(node, state);
    }
}
```

Add the `resolution: HashMap<NodeId, EdgeResolutionState>` field to the `CallGraph` struct and initialize in `new()` and `build_from_insight`.

Run: `cargo test -p al-core` (all insight tests)
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/al-core/src/insight/index.rs
git commit -m "feat(insight): add RecordTrigger edges, removal, and resolution state to CallGraph"
```

---

### Task 3: Create `insight/calls.rs` — Call edge extraction from ASTs

This is the core new module. It walks tree-sitter ASTs from `file_index.file_trees` to extract direct calls, record operations, and workspace subscriber attributes.

**Files:**
- Create: `crates/al-core/src/insight/calls.rs`
- Modify: `crates/al-core/src/insight/mod.rs`

- [ ] **Step 1: Create module and export it**

Create `crates/al-core/src/insight/calls.rs`:

```rust
//! Call edge extraction from workspace tree-sitter ASTs.
//!
//! Walks parsed AL source files to populate `Calls`, `Triggers`, and
//! `SubscribesTo` edges in the InsightGraph and CallGraph. Three kinds of
//! relationships are extracted:
//!
//! 1. **Direct calls** — `Foo.Bar()`, `Bar()`, `Codeunit.Run(X)` → `Calls` edges
//! 2. **Record operations** — `Rec.Insert(true)` → `Triggers` edges to table events
//! 3. **Workspace subscribers** — `[EventSubscriber(...)]` attributes → `SubscribesTo` edges

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use al_symbols::{ObjectKind, SymbolIndex};

use crate::file_index::FileIndex;
use super::graph::{InsightEdge, InsightGraph, InsightNode, NodeKey};
use super::index::{CallGraph, EdgeResolutionState, NodeId};
```

Add to `crates/al-core/src/insight/mod.rs`:

```rust
pub mod calls;
```

Run: `cargo check -p al-core`
Expected: PASS (empty module compiles)

- [ ] **Step 2: Write failing test for variable type extraction**

In `calls.rs`, add test module and a test for extracting variable-to-record-type mappings from a procedure body:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn parse_al(source: &str) -> tree_sitter::Tree {
        al_syntax::AlParser::new().parse_quick(source)
    }

    #[test]
    fn extract_var_types_from_procedure() {
        let source = r#"
codeunit 50100 "My Codeunit"
{
    procedure DoPost()
    var
        SalesHeader: Record "Sales Header";
        CustNo: Code[20];
        IsHandled: Boolean;
    begin
    end;
}
"#;
        let tree = parse_al(source);
        let var_types = extract_procedure_var_types(&tree, source, "DoPost");
        assert_eq!(var_types.get("salesheader").map(|s| s.as_str()), Some("Sales Header"));
        assert!(var_types.get("custno").is_none()); // not a Record type, skip
    }
```

Run: `cargo test -p al-core -- extract_var_types_from_procedure`
Expected: FAIL — function doesn't exist.

- [ ] **Step 3: Implement `extract_procedure_var_types`**

```rust
/// Extract a mapping of lowercase variable name → table name for all
/// `Record "X"` variables in a given procedure's `var` section and parameters.
fn extract_procedure_var_types(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let root = tree.root_node();
    let proc_lower = procedure_name.to_lowercase();

    // Find the procedure node
    let mut cursor = root.walk();
    find_procedure_vars(&mut cursor, source, &proc_lower, &mut result);
    result
}

fn find_procedure_vars(
    cursor: &mut tree_sitter::TreeCursor,
    source: &str,
    proc_lower: &str,
    result: &mut HashMap<String, String>,
) {
    // Walk tree looking for procedure_declaration nodes
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let node = cursor.node();
        let kind = node.kind();

        if kind == "procedure_declaration" || kind == "event_procedure_declaration" {
            // Check if this is our target procedure
            if let Some(name) = find_procedure_name(node, source) {
                if name.to_lowercase() == *proc_lower {
                    // Extract var section types
                    extract_record_vars_from_node(node, source, result);
                    // Extract parameter types
                    extract_record_params_from_node(node, source, result);
                    return;
                }
            }
        }

        // Recurse into object body etc
        if cursor.goto_first_child() {
            find_procedure_vars(cursor, source, proc_lower, result);
            cursor.goto_parent();
        }

        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn find_procedure_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    // The procedure name is a `name` or `name_or_keyword` child
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let child = cursor.node();
        let kind = child.kind();
        if kind == "name" || kind == "name_or_keyword" {
            return Some(al_syntax::node_text_clean(child, source).unwrap_or_default());
        }
        // Also check for parameter_list — means we passed the name
        if kind == "parameter_list" || kind == "var_section" || kind == "begin_end_block" {
            return None;
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    None
}

fn extract_record_vars_from_node(
    proc_node: tree_sitter::Node,
    source: &str,
    result: &mut HashMap<String, String>,
) {
    // Find var_section child, then iterate variable_declaration children
    let mut cursor = proc_node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "var_section" {
            extract_record_vars_from_var_section(child, source, result);
            return;
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn extract_record_vars_from_var_section(
    var_section: tree_sitter::Node,
    source: &str,
    result: &mut HashMap<String, String>,
) {
    let mut cursor = var_section.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "variable_declaration" || child.kind() == "regular_variable_declaration" {
            extract_single_var(child, source, result);
        }
        // Also recurse into variable_declaration which wraps regular_variable_declaration
        if cursor.goto_first_child() {
            loop {
                let inner = cursor.node();
                if inner.kind() == "regular_variable_declaration" {
                    extract_single_var(inner, source, result);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn extract_single_var(
    node: tree_sitter::Node,
    source: &str,
    result: &mut HashMap<String, String>,
) {
    // regular_variable_declaration has children: name(s), ":", type_reference
    let mut var_names = Vec::new();
    let mut table_name: Option<String> = None;

    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        let kind = child.kind();
        if kind == "name" || kind == "name_or_keyword" {
            if let Some(text) = al_syntax::node_text_clean(child, source) {
                var_names.push(text.to_lowercase());
            }
        } else if kind == "type_reference" {
            table_name = parse_record_type(child, source);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }

    if let Some(table) = table_name {
        for name in var_names {
            result.insert(name, table);
        }
    }
}

/// Parse a `type_reference` node. If it's `Record "Table Name"`, return the table name.
/// Returns `None` for non-Record types.
fn parse_record_type(type_ref: tree_sitter::Node, source: &str) -> Option<String> {
    let mut is_record = false;
    let mut subtype: Option<String> = None;

    let mut cursor = type_ref.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let child = cursor.node();
        let text = child.utf8_text(source.as_bytes()).unwrap_or("");
        let kind = child.kind();

        if !is_record && (kind.starts_with("kw_") || kind == "name" || kind == "identifier") {
            if text.eq_ignore_ascii_case("record") {
                is_record = true;
            }
        } else if is_record && subtype.is_none() {
            // Next meaningful child is the table name
            if kind == "quoted_identifier" || kind == "string" || kind == "name"
                || kind == "name_or_keyword"
            {
                let clean = text.trim_matches('"').trim_matches('\'');
                if !clean.is_empty() {
                    subtype = Some(clean.to_string());
                }
            }
        }

        if !cursor.goto_next_sibling() {
            break;
        }
    }

    if is_record { subtype } else { None }
}

fn extract_record_params_from_node(
    proc_node: tree_sitter::Node,
    source: &str,
    result: &mut HashMap<String, String>,
) {
    let mut cursor = proc_node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "parameter_list" {
            let mut inner = child.walk();
            if inner.goto_first_child() {
                loop {
                    let param = inner.node();
                    if param.kind() == "parameter" {
                        extract_single_param(param, source, result);
                    }
                    if !inner.goto_next_sibling() {
                        break;
                    }
                }
            }
            return;
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn extract_single_param(
    param_node: tree_sitter::Node,
    source: &str,
    result: &mut HashMap<String, String>,
) {
    let mut name: Option<String> = None;
    let mut table: Option<String> = None;

    let mut cursor = param_node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        let kind = child.kind();
        if (kind == "name" || kind == "name_or_keyword") && name.is_none() {
            if let Some(text) = al_syntax::node_text_clean(child, source) {
                name = Some(text.to_lowercase());
            }
        } else if kind == "type_reference" {
            table = parse_record_type(child, source);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }

    if let (Some(n), Some(t)) = (name, table) {
        result.insert(n, t);
    }
}
```

Run: `cargo test -p al-core -- extract_var_types_from_procedure`
Expected: PASS

- [ ] **Step 4: Write failing test for call site extraction**

```rust
#[test]
fn extract_call_sites_from_procedure() {
    let source = r#"
codeunit 50100 "My Codeunit"
{
    procedure DoPost()
    var
        SalesHeader: Record "Sales Header";
    begin
        ValidateHeader(SalesHeader);
        SalesPost.PostDocument(SalesHeader);
        SalesHeader.Insert(true);
        SalesHeader.Modify(false);
    end;
}
"#;
    let tree = parse_al(source);
    let sites = extract_call_sites(&tree, source, "DoPost");

    // Should find: bare call "ValidateHeader", member call "SalesPost.PostDocument",
    // record op "SalesHeader.Insert(true)", record op "SalesHeader.Modify(false)"
    assert!(sites.iter().any(|s| matches!(s, CallSite::BareCall { name, .. } if name == "ValidateHeader")));
    assert!(sites.iter().any(|s| matches!(s, CallSite::MemberCall { object, method, .. }
        if object == "SalesPost" && method == "PostDocument")));
    assert!(sites.iter().any(|s| matches!(s, CallSite::RecordOp { variable, op, run_trigger: true, .. }
        if variable == "SalesHeader" && *op == RecordOp::Insert)));
    assert!(sites.iter().any(|s| matches!(s, CallSite::RecordOp { variable, op, run_trigger: false, .. }
        if variable == "SalesHeader" && *op == RecordOp::Modify)));
}
```

Run: `cargo test -p al-core -- extract_call_sites_from_procedure`
Expected: FAIL — types and function don't exist.

- [ ] **Step 5: Implement call site extraction types and function**

```rust
/// A recognized record operation that can trigger table events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOp {
    Insert,
    Modify,
    Delete,
    Validate,
}

/// A call site found inside a procedure body.
#[derive(Debug, Clone)]
pub enum CallSite {
    /// `ProcedureName(args)` — unqualified call
    BareCall { name: String },
    /// `Object.Method(args)` — qualified member call
    MemberCall { object: String, method: String },
    /// `Rec.Insert(true)` / `Rec.Modify(false)` / `Rec.Validate(Field)`
    RecordOp { variable: String, op: RecordOp, run_trigger: bool },
}

/// Extract all call sites from a procedure body.
fn extract_call_sites(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> Vec<CallSite> {
    let root = tree.root_node();
    let proc_lower = procedure_name.to_lowercase();
    let mut sites = Vec::new();

    // Find the procedure's begin_end_block, then walk it for call expressions
    find_procedure_body(root, source, &proc_lower, &mut |body_node| {
        walk_for_calls(body_node, source, &mut sites);
    });

    sites
}

fn find_procedure_body(
    root: tree_sitter::Node,
    source: &str,
    proc_lower: &str,
    callback: &mut impl FnMut(tree_sitter::Node),
) {
    let mut cursor = root.walk();
    walk_for_procedure(&mut cursor, source, proc_lower, callback);
}

fn walk_for_procedure(
    cursor: &mut tree_sitter::TreeCursor,
    source: &str,
    proc_lower: &str,
    callback: &mut impl FnMut(tree_sitter::Node),
) {
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let node = cursor.node();
        let kind = node.kind();

        if kind == "procedure_declaration" || kind == "event_procedure_declaration" {
            if let Some(name) = find_procedure_name(node, source) {
                if name.to_lowercase() == *proc_lower {
                    // Find begin_end_block child
                    let mut inner = node.walk();
                    if inner.goto_first_child() {
                        loop {
                            if inner.node().kind() == "begin_end_block" {
                                callback(inner.node());
                                return;
                            }
                            if !inner.goto_next_sibling() { break; }
                        }
                    }
                    return;
                }
            }
        }

        if cursor.goto_first_child() {
            walk_for_procedure(cursor, source, proc_lower, callback);
            cursor.goto_parent();
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

const RECORD_OPS: &[(&str, RecordOp)] = &[
    ("insert", RecordOp::Insert),
    ("modify", RecordOp::Modify),
    ("delete", RecordOp::Delete),
    ("validate", RecordOp::Validate),
];

fn walk_for_calls(node: tree_sitter::Node, source: &str, sites: &mut Vec<CallSite>) {
    let kind = node.kind();

    if kind == "postfix_expression" {
        if let Some(site) = parse_postfix_call(node, source) {
            sites.push(site);
            return; // Don't descend into children — we already parsed this call
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_for_calls(cursor.node(), source, sites);
            if !cursor.goto_next_sibling() { break; }
        }
    }
}

fn parse_postfix_call(node: tree_sitter::Node, source: &str) -> Option<CallSite> {
    // postfix_expression = primary_expression + repeat(suffix)
    // We need to find the primary + the suffixes
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return None;
    }

    let primary = cursor.node();
    let primary_text = primary.utf8_text(source.as_bytes()).ok()?;
    let primary_name = primary_text.trim_matches('"');

    // Collect suffixes
    let mut last_member: Option<String> = None;
    let mut has_call = false;
    let mut call_args_text: Option<String> = None;

    while cursor.goto_next_sibling() {
        let suffix = cursor.node();
        let suffix_kind = suffix.kind();

        match suffix_kind {
            "member_call_suffix" => {
                // .Method(args) — this IS the call
                let method = suffix.child_by_field_name("member")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .trim_matches('"')
                    .to_string();
                let args = suffix.child_by_field_name("call")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .to_string();

                // Check if it's a record operation
                let method_lower = method.to_lowercase();
                if let Some(&(_, op)) = RECORD_OPS.iter().find(|(name, _)| *name == method_lower) {
                    let run_trigger = parse_run_trigger(&args, op);
                    return Some(CallSite::RecordOp {
                        variable: last_member.unwrap_or_else(|| primary_name.to_string()),
                        op,
                        run_trigger,
                    });
                }

                let obj = last_member.unwrap_or_else(|| primary_name.to_string());
                return Some(CallSite::MemberCall { object: obj, method });
            }
            "call_suffix" => {
                // Bare call: primary(args) or chained.member(args)
                has_call = true;
                call_args_text = suffix.child_by_field_name("call")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .map(|s| s.to_string());
            }
            "member_suffix" => {
                // .Member (no call yet, might chain further)
                last_member = suffix.child_by_field_name("member")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .map(|s| s.trim_matches('"').to_string());
            }
            "scope_call_suffix" => {
                // Type::Method(args)
                let method = suffix.child_by_field_name("member")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .trim_matches('"')
                    .to_string();
                return Some(CallSite::MemberCall {
                    object: primary_name.to_string(),
                    method,
                });
            }
            _ => {}
        }
    }

    if has_call {
        if let Some(member) = last_member {
            return Some(CallSite::MemberCall {
                object: primary_name.to_string(),
                method: member,
            });
        }
        return Some(CallSite::BareCall { name: primary_name.to_string() });
    }

    None
}

/// Parse whether the RunTrigger argument is true or false.
/// For Insert/Modify/Delete: first arg is RunTrigger, default true.
/// For Validate: always triggers (no RunTrigger param).
fn parse_run_trigger(args_text: &str, op: RecordOp) -> bool {
    if op == RecordOp::Validate {
        return true; // Validate always fires events
    }
    // Strip parens, find first argument
    let inner = args_text.trim_start_matches('(').trim_end_matches(')').trim();
    if inner.is_empty() {
        return true; // No args = default = true
    }
    // First arg before comma
    let first_arg = inner.split(',').next().unwrap_or("").trim().to_lowercase();
    first_arg != "false"
}
```

Run: `cargo test -p al-core -- extract_call_sites_from_procedure`
Expected: PASS

- [ ] **Step 6: Write failing test for the full edge population function**

```rust
#[test]
fn populate_call_edges_for_file() {
    let source = r#"
codeunit 50100 "Sales Handler"
{
    procedure DoPost()
    var
        SalesHeader: Record "Sales Header";
    begin
        ValidateDoc();
        SalesHeader.Insert(true);
    end;

    local procedure ValidateDoc()
    begin
    end;
}
"#;
    let tree = parse_al(source);
    let index = SymbolIndex::new();
    // Add the workspace codeunit to the symbol index so nodes exist
    index.add_entries(&[al_symbols::SymbolEntry {
        kind: ObjectKind::Codeunit,
        id: 50100,
        name: "Sales Handler".to_string(),
        methods: vec![
            al_symbols::MethodSymbol {
                name: "DoPost".to_string(),
                parameters: vec![],
                return_type: None,
                attributes: vec![],
                is_local: false,
            },
            al_symbols::MethodSymbol {
                name: "ValidateDoc".to_string(),
                parameters: vec![],
                return_type: None,
                attributes: vec![],
                is_local: true,
            },
        ],
        ..Default::default()
    }]);
    // Add a table with events for the Insert trigger
    index.add_entries(&[al_symbols::SymbolEntry {
        kind: ObjectKind::Table,
        id: 36,
        name: "Sales Header".to_string(),
        methods: vec![al_symbols::MethodSymbol {
            name: "OnBeforeInsertEvent".to_string(),
            parameters: vec![],
            return_type: None,
            attributes: vec![al_symbols::AttributeSymbol {
                name: "IntegrationEvent".to_string(),
                arguments: vec!["false".into(), "false".into()],
            }],
            is_local: false,
        }],
        ..Default::default()
    }]);

    let mut insight = InsightGraph::new();
    insight.build_from_index(&index);
    let mut call_graph = CallGraph::build_from_insight(&insight);

    let object_name = "Sales Handler";
    let object_kind = ObjectKind::Codeunit;
    populate_call_edges_for_procedure(
        &tree, source, object_kind, object_name,
        "DoPost", &index, &insight, &mut call_graph,
    );

    // DoPost should have a DirectCall to ValidateDoc
    let dopost_key = NodeKey::Procedure(ObjectKind::Codeunit, "sales handler".into(), "dopost".into());
    let dopost_id = CallGraph::node_id_for(&insight, &dopost_key).unwrap();
    let callees = call_graph.callees_of(dopost_id);
    assert!(callees.iter().any(|e| e.kind == EdgeKind::DirectCall), "Should have direct call to ValidateDoc");
    assert!(callees.iter().any(|e| e.kind == EdgeKind::RecordTrigger), "Should have trigger to Sales Header insert event");
}
```

Run: `cargo test -p al-core -- populate_call_edges_for_file`
Expected: FAIL — function doesn't exist.

- [ ] **Step 7: Implement `populate_call_edges_for_procedure`**

```rust
/// Populate call graph edges for a single procedure in a workspace file.
///
/// Extracts direct calls, record operations, and wires them as edges.
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
    let obj_lower = object_name.to_lowercase();
    let proc_lower = procedure_name.to_lowercase();

    let from_key = NodeKey::Procedure(object_kind, obj_lower.clone(), proc_lower.clone());
    let Some(from_id) = CallGraph::node_id_for(insight, &from_key) else {
        return; // Procedure not in the insight graph
    };

    // Get variable types for record operation resolution
    let var_types = extract_procedure_var_types(tree, source, procedure_name);

    // Extract call sites
    let sites = extract_call_sites(tree, source, procedure_name);

    for site in sites {
        match site {
            CallSite::BareCall { name } => {
                // Resolve against current object's methods first
                let target_key = NodeKey::Procedure(object_kind, obj_lower.clone(), name.to_lowercase());
                if let Some(target_id) = CallGraph::node_id_for(insight, &target_key) {
                    call_graph.add_direct_call(from_id, target_id);
                }
            }
            CallSite::MemberCall { object, method } => {
                // Try to resolve as: ObjectName.MethodName
                // First check symbol index for an object with this name
                let entries = symbols.get_by_name(&object);
                for entry in &entries {
                    let key = NodeKey::Procedure(
                        entry.kind,
                        entry.name.to_lowercase(),
                        method.to_lowercase(),
                    );
                    if let Some(target_id) = CallGraph::node_id_for(insight, &key) {
                        call_graph.add_direct_call(from_id, target_id);
                    }
                }
                // Also check if `object` is a variable name referencing a codeunit
                // (would need codeunit variable type tracking — defer for now)
            }
            CallSite::RecordOp { variable, op, run_trigger } => {
                if !run_trigger {
                    continue;
                }
                // Resolve variable to table name
                let var_lower = variable.to_lowercase();
                let table_name = var_types.get(&var_lower);
                let Some(table_name) = table_name else { continue };

                // Find table events matching this operation
                let event_names = match op {
                    RecordOp::Insert => vec!["onbeforeinsertevent", "onafterinsertevent"],
                    RecordOp::Modify => vec!["onbeforemodifyevent", "onaftermodifyevent"],
                    RecordOp::Delete => vec!["onbeforedeleteevent", "onafterdeleteevent"],
                    RecordOp::Validate => vec!["onbeforevalidateevent", "onaftervalidateevent"],
                };

                let table_lower = table_name.to_lowercase();
                for event_name in event_names {
                    let event_key = NodeKey::Event(ObjectKind::Table, table_lower.clone(), event_name.to_string());
                    if let Some(event_id) = CallGraph::node_id_for(insight, &event_key) {
                        call_graph.add_trigger(from_id, event_id);
                    }
                }
            }
        }
    }

    call_graph.set_resolution_state(from_id, EdgeResolutionState::Resolved);
}
```

Run: `cargo test -p al-core -- populate_call_edges_for_file`
Expected: PASS

- [ ] **Step 8: Write and implement fanout scoring**

```rust
/// Score an object by call-site fanout. Higher score = more outgoing calls.
/// Used to prioritize Tier 1 eager resolution.
pub fn fanout_score(tree: &tree_sitter::Tree, source: &str) -> usize {
    let mut count = 0;
    walk_count_calls(tree.root_node(), source, &mut count);
    count
}

fn walk_count_calls(node: tree_sitter::Node, source: &str, count: &mut usize) {
    let kind = node.kind();
    if kind == "member_call_suffix" || kind == "call_suffix" || kind == "scope_call_suffix" {
        *count += 1;
        return;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_count_calls(cursor.node(), source, count);
            if !cursor.goto_next_sibling() { break; }
        }
    }
}

#[test]
fn fanout_score_counts_calls() {
    let source = r#"
codeunit 50100 "Test"
{
    procedure A()
    begin
        B();
        C.D();
        Rec.Insert(true);
    end;
}
"#;
    let tree = parse_al(source);
    assert!(fanout_score(&tree, source) >= 3);
}
```

Run: `cargo test -p al-core -- fanout_score_counts_calls`
Expected: PASS

- [ ] **Step 9: Write and implement `register_workspace_nodes`**

Workspace objects/procedures aren't in the SymbolIndex (only `.app` packages are), so they don't have InsightGraph nodes. Before we can wire call edges, we must register them. The InsightGraph is built mutably before being wrapped in `Arc`, so we add this step to the build pipeline.

```rust
/// Register workspace file objects and their procedures as InsightGraph nodes.
///
/// Must be called BEFORE wrapping InsightGraph in Arc and before populating
/// call edges, so that workspace procedures have node IDs to wire edges to.
/// Also detects [EventSubscriber] attributes on workspace procedures and
/// registers them as Subscriber nodes with SubscribesTo edges.
pub fn register_workspace_nodes(
    file_index: &FileIndex,
    symbols: &SymbolIndex,
    insight: &mut InsightGraph,
) {
    use super::graph::EventNodeType;

    for entry in file_index.object_info.iter() {
        let path = entry.key();
        let obj_info = entry.value();
        let object_name = &obj_info.name;
        let Ok(object_kind) = obj_info.kind.parse::<ObjectKind>() else { continue };
        let obj_lower = object_name.to_lowercase();

        // Register the object node
        let obj_idx = insight.ensure_node(
            NodeKey::Object(object_kind, obj_lower.clone()),
            InsightNode::Object {
                kind: object_kind,
                id: obj_info.id.unwrap_or(0) as i32,
                name: object_name.clone(),
                package: "workspace".to_string(),
            },
        );

        // Get the source + tree to inspect procedure attributes
        let tree_and_source = file_index.file_trees.get(path)
            .and_then(|tree| {
                file_index.files.get(path).map(|text| (tree.clone(), text.clone()))
            });

        // Register each procedure
        if let Some(proc_names) = file_index.path_to_procedures.get(path) {
            for proc_name in proc_names.value().iter() {
                let proc_lower = proc_name.to_lowercase();

                // Check if this procedure has an EventSubscriber attribute
                let subscriber_info = tree_and_source.as_ref().and_then(|(tree, source)| {
                    extract_subscriber_attribute(tree, source, proc_name)
                });

                if let Some((target_obj_type, target_obj_name, target_event)) = subscriber_info {
                    // Register as Subscriber node
                    let sub_idx = insight.ensure_node(
                        NodeKey::Subscriber(object_kind, obj_lower.clone(), proc_lower.clone()),
                        InsightNode::Subscriber {
                            object_kind,
                            object_name: object_name.clone(),
                            name: proc_name.clone(),
                            target_object: target_obj_name.clone(),
                            target_event: target_event.clone(),
                        },
                    );
                    insight.add_edge(obj_idx, sub_idx, InsightEdge::Contains);

                    // Wire SubscribesTo edge to the target event
                    let target_lower = target_obj_name.to_lowercase();
                    let event_lower = target_event.to_lowercase();
                    // Try common object kinds for the target
                    for target_kind in &[ObjectKind::Codeunit, ObjectKind::Table, ObjectKind::Page, ObjectKind::Report] {
                        let event_key = NodeKey::Event(*target_kind, target_lower.clone(), event_lower.clone());
                        if let Some(event_idx) = insight.get_node(&event_key) {
                            insight.add_edge(sub_idx, event_idx, InsightEdge::SubscribesTo);
                            break;
                        }
                    }
                } else {
                    // Check if this procedure has IntegrationEvent/BusinessEvent attribute
                    let event_info = tree_and_source.as_ref().and_then(|(tree, source)| {
                        extract_event_attribute(tree, source, proc_name)
                    });

                    if let Some(event_type) = event_info {
                        let evt_idx = insight.ensure_node(
                            NodeKey::Event(object_kind, obj_lower.clone(), proc_lower.clone()),
                            InsightNode::Event {
                                object_kind,
                                object_name: object_name.clone(),
                                name: proc_name.clone(),
                                event_type,
                            },
                        );
                        insight.add_edge(obj_idx, evt_idx, InsightEdge::Publishes);
                    } else {
                        // Regular procedure
                        let proc_idx = insight.ensure_node(
                            NodeKey::Procedure(object_kind, obj_lower.clone(), proc_lower),
                            InsightNode::Procedure {
                                object_kind,
                                object_name: object_name.clone(),
                                name: proc_name.clone(),
                                is_local: false, // conservative default
                            },
                        );
                        insight.add_edge(obj_idx, proc_idx, InsightEdge::Contains);
                    }
                }
            }
        }
    }
}

/// Extract [EventSubscriber] attribute from a procedure in the AST.
/// Returns (target_object_type, target_object_name, target_event_name) if found.
fn extract_subscriber_attribute(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> Option<(String, String, String)> {
    let proc_lower = procedure_name.to_lowercase();
    let root = tree.root_node();
    let mut result = None;
    find_procedure_attribute(root, source, &proc_lower, "EventSubscriber", &mut |attr_node| {
        result = parse_subscriber_attribute_args(attr_node, source);
    });
    result
}

/// Extract [IntegrationEvent] or [BusinessEvent] attribute from a procedure.
fn extract_event_attribute(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> Option<super::graph::EventNodeType> {
    use super::graph::EventNodeType;
    let proc_lower = procedure_name.to_lowercase();
    let root = tree.root_node();

    let mut result = None;
    find_procedure_attribute(root, source, &proc_lower, "IntegrationEvent", &mut |_| {
        result = Some(EventNodeType::Integration);
    });
    if result.is_some() { return result; }

    find_procedure_attribute(root, source, &proc_lower, "BusinessEvent", &mut |_| {
        result = Some(EventNodeType::Business);
    });
    result
}

fn find_procedure_attribute(
    root: tree_sitter::Node,
    source: &str,
    proc_lower: &str,
    attr_name: &str,
    callback: &mut impl FnMut(tree_sitter::Node),
) {
    // Walk tree looking for procedure_declaration with the given name,
    // then check its attribute children for attr_name
    let mut cursor = root.walk();
    walk_for_attribute(&mut cursor, source, proc_lower, attr_name, callback);
}

fn walk_for_attribute(
    cursor: &mut tree_sitter::TreeCursor,
    source: &str,
    proc_lower: &str,
    attr_name: &str,
    callback: &mut impl FnMut(tree_sitter::Node),
) {
    if !cursor.goto_first_child() { return; }
    loop {
        let node = cursor.node();
        let kind = node.kind();
        if kind == "procedure_declaration" || kind == "event_procedure_declaration" {
            if let Some(name) = find_procedure_name(node, source) {
                if name.to_lowercase() == *proc_lower {
                    // Check attribute children
                    let mut inner = node.walk();
                    if inner.goto_first_child() {
                        loop {
                            let child = inner.node();
                            if child.kind() == "attribute" || child.kind() == "attribute_list" {
                                let text = child.utf8_text(source.as_bytes()).unwrap_or("");
                                if text.to_lowercase().contains(&attr_name.to_lowercase()) {
                                    callback(child);
                                    return;
                                }
                            }
                            if !inner.goto_next_sibling() { break; }
                        }
                    }
                    return;
                }
            }
        }
        if cursor.goto_first_child() {
            walk_for_attribute(cursor, source, proc_lower, attr_name, callback);
            cursor.goto_parent();
        }
        if !cursor.goto_next_sibling() { break; }
    }
}

fn parse_subscriber_attribute_args(
    attr_node: tree_sitter::Node,
    source: &str,
) -> Option<(String, String, String)> {
    // Parse [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
    // Extract the text content and parse argument positions
    let text = attr_node.utf8_text(source.as_bytes()).ok()?;

    // Find the argument list between ( and )
    let start = text.find('(')?;
    let end = text.rfind(')')?;
    let args_str = &text[start + 1..end];

    // Split by comma, handling quoted strings
    let args: Vec<&str> = split_attribute_args(args_str);
    if args.len() < 3 { return None; }

    let obj_type = args[0].trim().to_string();
    // Clean object name: strip "Codeunit::" prefix and quotes
    let obj_name = args[1].trim()
        .split("::")
        .last()
        .unwrap_or(args[1].trim())
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();
    let event_name = args[2].trim()
        .trim_matches('\'')
        .trim_matches('"')
        .trim()
        .to_string();

    if obj_name.is_empty() || event_name.is_empty() { return None; }
    Some((obj_type, obj_name, event_name))
}

fn split_attribute_args(s: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0;
    let mut in_quote = false;
    let mut quote_char = ' ';
    let mut start = 0;

    for (i, ch) in s.char_indices() {
        match ch {
            '\'' | '"' if !in_quote => { in_quote = true; quote_char = ch; }
            c if in_quote && c == quote_char => { in_quote = false; }
            '(' if !in_quote => { depth += 1; }
            ')' if !in_quote => { depth -= 1; }
            ',' if !in_quote && depth == 0 => {
                args.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(&s[start..]);
    args
}
```

Run: `cargo check -p al-core`
Expected: PASS

- [ ] **Step 10: Write and implement `populate_workspace_call_edges`**

This is the top-level function that walks all workspace files. It calls `register_workspace_nodes` first (the InsightGraph is still mutable at this point), then builds the CallGraph and populates edges:

```rust
/// Populate call edges for all procedures in all workspace files.
///
/// **Tiered strategy:**
/// - Tier 1 (eager): Objects scoring in the top 20% by fanout are fully resolved.
/// - Tier 2 (lazy): Remaining objects have their procedures marked `Unresolved`.
///
/// Returns the number of Tier 1 procedures resolved.
pub fn populate_workspace_call_edges(
    file_index: &FileIndex,
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) -> usize {
    // Pass 1: Score all files by fanout
    let mut scored: Vec<(std::path::PathBuf, String, usize)> = Vec::new(); // (path, text, score)
    for entry in file_index.file_trees.iter() {
        let path = entry.key().clone();
        let tree = entry.value().clone();
        if let Some(text_entry) = file_index.files.get(&path) {
            let text = text_entry.value().clone();
            let score = fanout_score(&tree, &text);
            scored.push((path, text, score));
        }
    }

    if scored.is_empty() {
        return 0;
    }

    // Sort descending by score
    scored.sort_by(|a, b| b.2.cmp(&a.2));

    // Tier 1 threshold: top 20% or minimum score of 5
    let tier1_count = (scored.len() / 5).max(1);
    let tier1_threshold = scored.get(tier1_count).map(|s| s.2).unwrap_or(0).max(5);

    let mut resolved = 0;
    for (path, text, score) in &scored {
        if *score < tier1_threshold {
            break; // Rest is Tier 2
        }

        // Get object info for this file
        let Some(obj_info) = file_index.object_info.get(path) else { continue };
        let object_name = &obj_info.name;
        let object_kind = obj_info.kind.parse::<ObjectKind>().unwrap_or_default();

        let Some(tree_entry) = file_index.file_trees.get(path) else { continue };
        let tree = tree_entry.value().clone();

        // Get all procedures in this file
        if let Some(proc_names) = file_index.path_to_procedures.get(path) {
            for proc_name in proc_names.value().iter() {
                populate_call_edges_for_procedure(
                    &tree, text, object_kind, object_name,
                    proc_name, symbols, insight, call_graph,
                );
                resolved += 1;
            }
        }
    }

    resolved
}
```

Run: `cargo test -p al-core` (full crate)
Expected: PASS

- [ ] **Step 11: Commit**

```bash
git add crates/al-core/src/insight/calls.rs crates/al-core/src/insight/mod.rs
git commit -m "feat(insight): call edge extraction from workspace tree-sitter ASTs"
```

---

### Task 4: Add CallGraph to Workspace and wire background build

**Files:**
- Modify: `crates/al-core/src/workspace.rs`

- [ ] **Step 1: Add CallGraph field to Workspace**

Add import:

```rust
use crate::insight::index::CallGraph;
```

Add field to `Workspace` struct (after `insight_graph`):

```rust
/// Cached call graph. Built lazily after insight graph; invalidated with it.
pub call_graph: std::sync::RwLock<Option<CallGraph>>,
```

Initialize in `new()`:

```rust
call_graph: std::sync::RwLock::new(None),
```

- [ ] **Step 2: Add `get_or_build_call_graph` method**

This method builds the InsightGraph with workspace nodes included (not reusing
the basic cached InsightGraph, since it needs workspace object/procedure/subscriber
nodes that only exist in `file_index`), then builds the CallGraph on top.

```rust
/// Get (or lazily build) the cached CallGraph.
///
/// Builds a workspace-enriched InsightGraph (symbol index + workspace file
/// objects/procedures/subscribers), then builds the CallGraph and populates
/// Tier 1 call edges. The enriched InsightGraph replaces the cached one.
pub fn get_or_build_call_graph(&self) -> (Arc<InsightGraph>, std::sync::RwLockReadGuard<'_, Option<CallGraph>>) {
    // Check if call graph already exists
    {
        let cg_guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
        if cg_guard.is_some() {
            let insight = self.get_or_build_insight_graph();
            return (insight, cg_guard);
        }
    }

    // Build enriched InsightGraph: symbols + workspace nodes
    let mut graph = InsightGraph::new();
    graph.build_from_index(&self.symbols);
    // Register workspace objects, procedures, events, and subscribers
    crate::insight::calls::register_workspace_nodes(
        &self.file_index, &self.symbols, &mut graph,
    );
    let insight = Arc::new(graph);

    // Cache the enriched InsightGraph (replaces symbol-only version)
    if let Ok(mut ig_guard) = self.insight_graph.write() {
        *ig_guard = Some(Arc::clone(&insight));
    }

    // Build CallGraph and populate Tier 1 call edges
    let mut cg = CallGraph::build_from_insight(&insight);
    crate::insight::calls::populate_workspace_call_edges(
        &self.file_index, &self.symbols, &insight, &mut cg,
    );

    let mut guard = self.call_graph.write().unwrap_or_else(|e| e.into_inner());
    *guard = Some(cg);
    drop(guard);

    let guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
    (insight, guard)
}

/// Invalidate both the insight graph and call graph caches.
pub fn invalidate_graphs(&self) {
    self.invalidate_insight_graph();
    if let Ok(mut guard) = self.call_graph.write() {
        *guard = None;
    }
}
```

- [ ] **Step 3: Update `invalidate_insight_graph` to also clear CallGraph**

Change `invalidate_insight_graph` to also clear the call graph:

```rust
pub fn invalidate_insight_graph(&self) {
    if let Ok(mut guard) = self.insight_graph.write() {
        *guard = None;
    }
    if let Ok(mut guard) = self.call_graph.write() {
        *guard = None;
    }
}
```

Run: `cargo check -p al-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/al-core/src/workspace.rs
git commit -m "feat(workspace): add CallGraph storage with lazy build and invalidation"
```

---

### Task 5: Rewrite `suggest_event.rs` — Query types

**Files:**
- Rewrite: `crates/al-core/src/queries/suggest_event.rs`

- [ ] **Step 1: Replace module with new types**

Delete all existing content and write the new type definitions:

```rust
//! Structured integration point discovery.
//!
//! Given an object+procedure, table, or event, traces the call/event graph
//! to find all reachable integration points (events that fire along the
//! execution path). Supports filtering results to events that expose a
//! specific table as a `var` parameter.

use std::collections::HashSet;
use std::sync::Arc;

use al_symbols::{ObjectKind, SymbolIndex};
use serde::{Deserialize, Serialize};

use crate::insight::graph::{InsightGraph, InsightNode, NodeKey};
use crate::insight::index::{CallGraph, EdgeKind, EdgeResolutionState, NodeId};
use crate::workspace::Workspace;

/// Input query for integration point discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventQuery {
    /// The entry point to trace from.
    pub source: QuerySource,
    /// Only return events exposing this table as a `var` parameter.
    #[serde(default)]
    pub filter_table: Option<String>,
    /// Only return events exposing this specific field.
    #[serde(default)]
    pub filter_field: Option<String>,
}

/// The entry point for a query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum QuerySource {
    /// Trace all events reachable from this object (or object + procedure).
    Procedure {
        object: String,
        #[serde(default)]
        procedure: Option<String>,
    },
    /// Find all events where this table is exposed as `var`.
    Table {
        table: String,
    },
    /// Trace an event's subscriber chain downstream.
    Event {
        object: String,
        event: String,
    },
}

/// Result of an integration point query.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestEventResult {
    /// The integration points found.
    pub integration_points: Vec<IntegrationPoint>,
    /// True if some call paths weren't fully resolved yet (Tier 2 pending).
    pub partial: bool,
}

/// A single integration point — an event that fires along the traced path.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationPoint {
    /// The event name.
    pub event: String,
    /// The object publishing the event.
    pub object: String,
    /// Event type: "integration", "business", or "trigger".
    pub event_type: String,
    /// Event parameters.
    pub params: Vec<ParamInfo>,
    /// The call chain that leads to this event from the query source.
    pub path: Vec<TraceHop>,
    /// Ready-to-paste `[EventSubscriber(...)]` attribute.
    pub example: String,
}

/// A parameter on an event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamInfo {
    pub name: String,
    pub type_name: String,
    pub is_var: bool,
}

/// One hop in the trace path from query source to integration point.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceHop {
    pub object: String,
    pub procedure: String,
    pub edge_kind: String,
}
```

Run: `cargo check -p al-core`
Expected: PASS (types only, no functions yet)

- [ ] **Step 2: Commit types**

```bash
git add crates/al-core/src/queries/suggest_event.rs
git commit -m "feat(suggest_event): replace NLP module with structured query types"
```

---

### Task 6: Implement query logic

**Files:**
- Modify: `crates/al-core/src/queries/suggest_event.rs`

- [ ] **Step 1: Write failing test for Procedure query**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;

    fn make_workspace_with_events() -> Workspace {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            // A codeunit that publishes an event
            SymbolEntry {
                kind: ObjectKind::Codeunit,
                id: 80,
                name: "Sales-Post".to_string(),
                methods: vec![
                    MethodSymbol {
                        name: "PostSalesDoc".to_string(),
                        parameters: vec![],
                        return_type: None,
                        attributes: vec![],
                        is_local: false,
                    },
                    MethodSymbol {
                        name: "OnAfterPostSalesDoc".to_string(),
                        parameters: vec![
                            ParameterSymbol {
                                name: "SalesHeader".to_string(),
                                type_name: "Record \"Sales Header\"".to_string(),
                                is_var: true,
                            },
                        ],
                        return_type: None,
                        attributes: vec![AttributeSymbol {
                            name: "IntegrationEvent".to_string(),
                            arguments: vec!["false".into(), "false".into()],
                        }],
                        is_local: false,
                    },
                ],
                ..Default::default()
            },
        ]);
        ws
    }

    #[test]
    fn procedure_query_finds_published_events() {
        let ws = make_workspace_with_events();
        let query = EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            filter_table: None,
            filter_field: None,
        };
        let result = suggest_event(&ws, &query);
        assert!(!result.integration_points.is_empty(), "Should find OnAfterPostSalesDoc");
        assert_eq!(result.integration_points[0].event, "OnAfterPostSalesDoc");
        assert_eq!(result.integration_points[0].event_type, "integration");
    }

    #[test]
    fn table_filter_narrows_results() {
        let ws = make_workspace_with_events();
        // Add another event that does NOT have Sales Header
        ws.symbols.add_entries(&[SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            methods: vec![MethodSymbol {
                name: "OnAfterCheckItemAvailability".to_string(),
                parameters: vec![ParameterSymbol {
                    name: "ItemNo".to_string(),
                    type_name: "Code[20]".to_string(),
                    is_var: false,
                }],
                return_type: None,
                attributes: vec![AttributeSymbol {
                    name: "IntegrationEvent".to_string(),
                    arguments: vec!["false".into(), "false".into()],
                }],
                is_local: false,
            }],
            ..Default::default()
        }]);

        let query = EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            filter_table: Some("Sales Header".to_string()),
            filter_field: None,
        };
        let result = suggest_event(&ws, &query);
        // Only OnAfterPostSalesDoc has Sales Header as var
        assert!(result.integration_points.iter().all(|ip| {
            ip.params.iter().any(|p| p.is_var && p.type_name.to_lowercase().contains("sales header"))
        }));
    }
}
```

Run: `cargo test -p al-core -- procedure_query_finds_published_events`
Expected: FAIL — `suggest_event` function doesn't exist.

- [ ] **Step 2: Implement `suggest_event` main entry point**

```rust
/// Query for integration points using the InsightGraph and CallGraph.
pub fn suggest_event(workspace: &Workspace, query: &EventQuery) -> SuggestEventResult {
    match &query.source {
        QuerySource::Procedure { object, procedure } => {
            query_procedure(workspace, object, procedure.as_deref(), &query.filter_table, &query.filter_field)
        }
        QuerySource::Table { table } => {
            query_table(workspace, table, &query.filter_field)
        }
        QuerySource::Event { object, event } => {
            query_event(workspace, object, event, &query.filter_table, &query.filter_field)
        }
    }
}
```

- [ ] **Step 3: Implement `query_procedure`**

```rust
fn query_procedure(
    workspace: &Workspace,
    object_name: &str,
    procedure_name: Option<&str>,
    filter_table: &Option<String>,
    filter_field: &Option<String>,
) -> SuggestEventResult {
    let (insight, cg_guard) = workspace.get_or_build_call_graph();
    let call_graph = match cg_guard.as_ref() {
        Some(cg) => cg,
        None => return SuggestEventResult { integration_points: vec![], partial: true },
    };

    let mut points = Vec::new();
    let mut visited = HashSet::new();
    let mut partial = false;

    // Find the object in the symbol index to get its kind
    let entries = workspace.symbols.get_by_name(object_name);
    let Some(entry) = entries.first() else {
        return SuggestEventResult { integration_points: vec![], partial: false };
    };

    // First, collect events directly published by this object
    collect_published_events(&insight, &workspace.symbols, entry.kind, object_name, &mut points);

    // Then trace call graph edges if we have them
    if let Some(proc_name) = procedure_name {
        let proc_key = NodeKey::Procedure(entry.kind, object_name.to_lowercase(), proc_name.to_lowercase());
        if let Some(proc_id) = CallGraph::node_id_for(&insight, &proc_key) {
            if call_graph.resolution_state(proc_id) == EdgeResolutionState::Unresolved {
                // Tier 2: try lazy resolution
                // For now just mark partial — full lazy resolution requires mutable access
                partial = true;
            }
            trace_from_node(proc_id, &insight, call_graph, &workspace.symbols, &mut points, &mut visited, 0, 10);
        }
    } else {
        // Trace all procedures on this object
        let obj_lower = object_name.to_lowercase();
        for method in &entry.methods {
            let proc_key = NodeKey::Procedure(entry.kind, obj_lower.clone(), method.name.to_lowercase());
            if let Some(proc_id) = CallGraph::node_id_for(&insight, &proc_key) {
                if call_graph.resolution_state(proc_id) == EdgeResolutionState::Unresolved {
                    partial = true;
                }
                trace_from_node(proc_id, &insight, call_graph, &workspace.symbols, &mut points, &mut visited, 0, 10);
            }
        }
    }

    // Deduplicate by event name + object
    dedup_points(&mut points);

    // Apply filters
    if filter_table.is_some() || filter_field.is_some() {
        apply_filters(&mut points, filter_table, filter_field);
    }

    SuggestEventResult { integration_points: points, partial }
}

fn collect_published_events(
    insight: &InsightGraph,
    symbols: &SymbolIndex,
    object_kind: ObjectKind,
    object_name: &str,
    points: &mut Vec<IntegrationPoint>,
) {
    let obj_lower = object_name.to_lowercase();
    // Scan the insight graph index for Event nodes belonging to this object
    for (key, indices) in &insight.index {
        if let NodeKey::Event(kind, obj, event_name) = key {
            if *kind == object_kind && *obj == obj_lower {
                for &idx in indices {
                    if let Some(node) = insight.graph.node_weight(idx) {
                        if let InsightNode::Event { name, event_type, object_name: obj_name, .. } = node {
                            // Look up the method in the symbol index for parameters
                            let params = lookup_event_params(symbols, object_kind, &obj_lower, name);
                            let event_type_str = format!("{:?}", event_type).to_lowercase();
                            let example = format_example(object_kind, obj_name, name);
                            points.push(IntegrationPoint {
                                event: name.clone(),
                                object: obj_name.clone(),
                                event_type: event_type_str,
                                params,
                                path: vec![],
                                example,
                            });
                        }
                    }
                }
            }
        }
    }
}

fn trace_from_node(
    node_id: NodeId,
    insight: &InsightGraph,
    call_graph: &CallGraph,
    symbols: &SymbolIndex,
    points: &mut Vec<IntegrationPoint>,
    visited: &mut HashSet<usize>,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth || !visited.insert(node_id.0) {
        return;
    }

    for edge in call_graph.callees_of(node_id) {
        let target_id = edge.to;
        if let Some(info) = call_graph.node_info(target_id) {
            if info.node_type == "event" {
                // Found an event — collect it as an integration point
                let obj_lower = info.object.to_lowercase();
                let event_lower = info.name.to_lowercase();

                // Try to determine object kind from the insight graph
                let (obj_kind, event_type_str) = resolve_event_type(insight, &obj_lower, &event_lower);
                let params = lookup_event_params(symbols, obj_kind, &obj_lower, &info.name);
                let example = format_example(obj_kind, &info.object, &info.name);

                points.push(IntegrationPoint {
                    event: info.name.clone(),
                    object: info.object.clone(),
                    event_type: event_type_str,
                    params,
                    path: vec![], // TODO: build trace path in later enhancement
                    example,
                });

                // Follow subscribers of this event
                for sub_id in call_graph.subscribers_of(target_id) {
                    trace_from_node(sub_id, insight, call_graph, symbols, points, visited, depth + 1, max_depth);
                }
            } else {
                // Procedure — recurse
                trace_from_node(target_id, insight, call_graph, symbols, points, visited, depth + 1, max_depth);
            }
        }
    }
}

fn resolve_event_type(insight: &InsightGraph, obj_lower: &str, event_lower: &str) -> (ObjectKind, String) {
    // Try common object kinds
    for kind in &[ObjectKind::Codeunit, ObjectKind::Table, ObjectKind::Page, ObjectKind::Report] {
        let key = NodeKey::Event(*kind, obj_lower.to_string(), event_lower.to_string());
        if let Some(idx) = insight.get_node(&key) {
            if let Some(InsightNode::Event { event_type, .. }) = insight.graph.node_weight(idx) {
                return (*kind, format!("{:?}", event_type).to_lowercase());
            }
        }
    }
    (ObjectKind::Codeunit, "integration".to_string())
}

fn lookup_event_params(
    symbols: &SymbolIndex,
    object_kind: ObjectKind,
    obj_lower: &str,
    event_name: &str,
) -> Vec<ParamInfo> {
    let entries = symbols.get_by_name(obj_lower);
    for entry in &entries {
        if entry.kind == object_kind {
            for method in &entry.methods {
                if method.name.eq_ignore_ascii_case(event_name) {
                    return method.parameters.iter().map(|p| ParamInfo {
                        name: p.name.clone(),
                        type_name: p.type_name.clone(),
                        is_var: p.is_var,
                    }).collect();
                }
            }
        }
    }
    // Also try composed object (base + extensions)
    if let Some(composed) = symbols.get_composed_cached(object_kind, obj_lower) {
        for method in &composed.all_methods {
            if method.name.eq_ignore_ascii_case(event_name) {
                return method.parameters.iter().map(|p| ParamInfo {
                    name: p.name.clone(),
                    type_name: p.type_name.clone(),
                    is_var: p.is_var,
                }).collect();
            }
        }
    }
    vec![]
}

fn format_example(object_kind: ObjectKind, object_name: &str, event_name: &str) -> String {
    let kind_str = object_kind.al_keyword();
    format!(
        "[EventSubscriber(ObjectType::{kind_str}, {kind_str}::\"{object_name}\", '{event_name}', '', false, false)]"
    )
}

fn dedup_points(points: &mut Vec<IntegrationPoint>) {
    let mut seen = HashSet::new();
    points.retain(|p| {
        let key = format!("{}::{}", p.object.to_lowercase(), p.event.to_lowercase());
        seen.insert(key)
    });
}

fn apply_filters(
    points: &mut Vec<IntegrationPoint>,
    filter_table: &Option<String>,
    filter_field: &Option<String>,
) {
    let table_lower = filter_table.as_ref().map(|t| t.to_lowercase());
    let field_lower = filter_field.as_ref().map(|f| f.to_lowercase());

    points.retain(|ip| {
        // At least one var parameter must reference the filter table
        let table_match = table_lower.as_ref().map_or(true, |table| {
            ip.params.iter().any(|p| {
                p.is_var && p.type_name.to_lowercase().contains(table.as_str())
            })
        });

        let field_match = field_lower.as_ref().map_or(true, |field| {
            // Check if event name references the field (e.g. OnBeforeValidate for field validation)
            ip.event.to_lowercase().contains(field.as_str())
                || ip.params.iter().any(|p| p.name.to_lowercase().contains(field.as_str()))
        });

        table_match && field_match
    });
}
```

Run: `cargo test -p al-core -- procedure_query_finds_published_events`
Expected: PASS

- [ ] **Step 4: Implement `query_table`**

```rust
fn query_table(
    workspace: &Workspace,
    table_name: &str,
    filter_field: &Option<String>,
) -> SuggestEventResult {
    let insight = workspace.get_or_build_insight_graph();
    let table_lower = table_name.to_lowercase();
    let mut points = Vec::new();

    // 1. Events published directly by this table (OnBeforeInsert, etc.)
    collect_published_events(&insight, &workspace.symbols, ObjectKind::Table, table_name, &mut points);

    // 2. Scan all events in the symbol index where this table appears as a var parameter
    let events = workspace.symbols.get_events("");
    for pub_event in &events.publishers {
        let has_var_record = pub_event.method.parameters.iter().any(|p| {
            p.is_var && is_record_of_table(&p.type_name, &table_lower)
        });
        if has_var_record {
            let event_type_str = format!("{}", pub_event.event_type).to_lowercase()
                .replace("event", ""); // "IntegrationEvent" -> "integration"
            let params: Vec<ParamInfo> = pub_event.method.parameters.iter().map(|p| ParamInfo {
                name: p.name.clone(),
                type_name: p.type_name.clone(),
                is_var: p.is_var,
            }).collect();
            let example = format_example(pub_event.object.kind, &pub_event.object.name, &pub_event.method.name);
            points.push(IntegrationPoint {
                event: pub_event.method.name.clone(),
                object: pub_event.object.name.clone(),
                event_type: event_type_str,
                params,
                path: vec![],
                example,
            });
        }
    }

    dedup_points(&mut points);

    if filter_field.is_some() {
        apply_filters(&mut points, &None, filter_field);
    }

    SuggestEventResult { integration_points: points, partial: false }
}

/// Check if a type string like `Record "Sales Header"` refers to the given table.
fn is_record_of_table(type_name: &str, table_lower: &str) -> bool {
    let tl = type_name.to_lowercase();
    if !tl.starts_with("record ") {
        return false;
    }
    let remainder = &tl["record ".len()..];
    let cleaned = remainder.trim_matches('"').trim();
    cleaned == table_lower
}
```

- [ ] **Step 5: Implement `query_event`**

```rust
fn query_event(
    workspace: &Workspace,
    object_name: &str,
    event_name: &str,
    filter_table: &Option<String>,
    filter_field: &Option<String>,
) -> SuggestEventResult {
    let (insight, cg_guard) = workspace.get_or_build_call_graph();
    let call_graph = match cg_guard.as_ref() {
        Some(cg) => cg,
        None => return SuggestEventResult { integration_points: vec![], partial: true },
    };

    let obj_lower = object_name.to_lowercase();
    let event_lower = event_name.to_lowercase();
    let mut points = Vec::new();
    let mut visited = HashSet::new();

    // Find the event node
    for kind in &[ObjectKind::Codeunit, ObjectKind::Table, ObjectKind::Page, ObjectKind::Report] {
        let key = NodeKey::Event(*kind, obj_lower.clone(), event_lower.clone());
        if let Some(event_id) = CallGraph::node_id_for(&insight, &key) {
            // The event itself is an integration point
            let params = lookup_event_params(&workspace.symbols, *kind, &obj_lower, event_name);
            let event_type_str = resolve_event_type(&insight, &obj_lower, &event_lower).1;
            let example = format_example(*kind, object_name, event_name);
            points.push(IntegrationPoint {
                event: event_name.to_string(),
                object: object_name.to_string(),
                event_type: event_type_str,
                params,
                path: vec![],
                example,
            });

            // Trace downstream: subscribers and what they trigger
            for sub_id in call_graph.subscribers_of(event_id) {
                trace_from_node(sub_id, &insight, call_graph, &workspace.symbols, &mut points, &mut visited, 0, 10);
            }
            break;
        }
    }

    dedup_points(&mut points);
    if filter_table.is_some() || filter_field.is_some() {
        apply_filters(&mut points, filter_table, filter_field);
    }

    SuggestEventResult { integration_points: points, partial: false }
}
```

- [ ] **Step 6: Write test for Table query and run all tests**

```rust
#[test]
fn table_query_finds_var_params() {
    let ws = Workspace::new();
    ws.symbols.add_entries(&[SymbolEntry {
        kind: ObjectKind::Codeunit,
        id: 80,
        name: "Sales-Post".to_string(),
        methods: vec![MethodSymbol {
            name: "OnAfterPostSalesDoc".to_string(),
            parameters: vec![
                ParameterSymbol {
                    name: "SalesHeader".to_string(),
                    type_name: "Record \"Sales Header\"".to_string(),
                    is_var: true,
                },
            ],
            return_type: None,
            attributes: vec![AttributeSymbol {
                name: "IntegrationEvent".to_string(),
                arguments: vec!["false".into(), "false".into()],
            }],
            is_local: false,
        }],
        ..Default::default()
    }]);

    let query = EventQuery {
        source: QuerySource::Table { table: "Sales Header".to_string() },
        filter_table: None,
        filter_field: None,
    };
    let result = suggest_event(&ws, &query);
    assert!(!result.integration_points.is_empty(), "Should find events with Sales Header as var");
    assert!(result.integration_points.iter().any(|ip| ip.event == "OnAfterPostSalesDoc"));
}

#[test]
fn empty_results_for_unknown_object() {
    let ws = Workspace::new();
    let query = EventQuery {
        source: QuerySource::Procedure {
            object: "NonExistent".to_string(),
            procedure: None,
        },
        filter_table: None,
        filter_field: None,
    };
    let result = suggest_event(&ws, &query);
    assert!(result.integration_points.is_empty());
}
```

Run: `cargo test -p al-core -- suggest_event`
Expected: PASS (all tests in module)

- [ ] **Step 7: Commit**

```bash
git add crates/al-core/src/queries/suggest_event.rs
git commit -m "feat(suggest_event): implement structured query logic over InsightGraph + CallGraph"
```

---

### Task 7: Update daemon dispatch

**Files:**
- Modify: `crates/al-lsp/src/daemon/insight_dispatch.rs`

- [ ] **Step 1: Rewrite `dispatch_suggest_event`**

Replace the existing `dispatch_suggest_event` function:

```rust
pub(super) fn dispatch_suggest_event(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(query_value) = params.get("query") else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "Missing 'query' parameter. Expected: { query: { source: { type: 'procedure', object: '...', procedure: '...' } } }".to_string(),
            }),
        };
    };

    let query: al_core::queries::suggest_event::EventQuery = match serde_json::from_value(query_value.clone()) {
        Ok(q) => q,
        Err(e) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: format!("Invalid query format: {e}"),
                }),
            };
        }
    };

    let result = al_core::queries::suggest_event::suggest_event(workspace, &query);
    Response {
        id,
        result: Some(serde_json::to_value(&result).unwrap_or_default()),
        error: None,
    }
}
```

Run: `cargo check -p al-lsp`
Expected: PASS

- [ ] **Step 2: Commit**

```bash
git add crates/al-lsp/src/daemon/insight_dispatch.rs
git commit -m "feat(daemon): update suggestEvent dispatch for structured query params"
```

---

### Task 8: Update CLI

**Files:**
- Modify: `crates/al-cli/src/main.rs`
- Modify: `crates/al-cli/src/commands/insight.rs`

- [ ] **Step 1: Update clap command definition**

In `main.rs`, replace the `SuggestEvent` variant:

```rust
/// Find integration points (events) for an object, table, or event
#[command(name = "suggest-event")]
SuggestEvent {
    /// Object name to trace (e.g. "Sales-Post")
    #[arg(long)]
    object: Option<String>,
    /// Procedure name within the object
    #[arg(long)]
    procedure: Option<String>,
    /// Table name — find events exposing this table as var
    #[arg(long)]
    table: Option<String>,
    /// Field name filter
    #[arg(long)]
    field: Option<String>,
    /// Event name to trace downstream
    #[arg(long)]
    event: Option<String>,
},
```

Update the match arm:

```rust
Commands::SuggestEvent { object, procedure, table, field, event } => {
    insight::cmd_suggest_event(object, procedure, table, field, event, cli.json)
}
```

- [ ] **Step 2: Rewrite `cmd_suggest_event`**

In `insight.rs`, replace:

```rust
pub fn cmd_suggest_event(
    object: Option<String>,
    procedure: Option<String>,
    table: Option<String>,
    field: Option<String>,
    event: Option<String>,
    json: bool,
) -> ExitCode {
    // Build the query source from CLI args
    let source = if let Some(ref evt) = event {
        let obj = object.as_deref().unwrap_or("");
        if obj.is_empty() {
            eprintln!("Error: --event requires --object");
            return ExitCode::FAILURE;
        }
        serde_json::json!({ "type": "event", "object": obj, "event": evt })
    } else if let Some(ref obj) = object {
        let mut src = serde_json::json!({ "type": "procedure", "object": obj });
        if let Some(ref proc_name) = procedure {
            src.as_object_mut().unwrap().insert("procedure".to_string(), serde_json::json!(proc_name));
        }
        src
    } else if let Some(ref tbl) = table {
        serde_json::json!({ "type": "table", "table": tbl })
    } else {
        eprintln!("Error: specify --object, --table, or --event");
        return ExitCode::FAILURE;
    };

    let mut query = serde_json::json!({ "source": source });
    // If --table is provided alongside --object, it becomes a filter
    if object.is_some() && table.is_some() {
        query.as_object_mut().unwrap().insert("filterTable".to_string(), serde_json::json!(table));
    }
    if let Some(ref f) = field {
        query.as_object_mut().unwrap().insert("filterField".to_string(), serde_json::json!(f));
    }

    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("suggestEvent", Some(serde_json::json!({ "query": query }))) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let points = result
                    .get("integrationPoints")
                    .and_then(|v| v.as_array())
                    .map(|v| &v[..])
                    .unwrap_or(&[]);
                let partial = result.get("partial").and_then(|v| v.as_bool()).unwrap_or(false);

                if points.is_empty() {
                    println!("No integration points found.");
                } else {
                    println!("Integration points ({} found):\n", points.len());
                    for (i, ip) in points.iter().enumerate() {
                        let evt = ip.get("event").and_then(|v| v.as_str()).unwrap_or("?");
                        let obj = ip.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                        let etype = ip.get("eventType").and_then(|v| v.as_str()).unwrap_or("?");
                        let example = ip.get("example").and_then(|v| v.as_str()).unwrap_or("");

                        println!("{}. {} ({}) — {}", i + 1, evt, etype, obj);

                        // Show var params
                        if let Some(params) = ip.get("params").and_then(|v| v.as_array()) {
                            let var_params: Vec<_> = params.iter()
                                .filter(|p| p.get("isVar").and_then(|v| v.as_bool()).unwrap_or(false))
                                .collect();
                            if !var_params.is_empty() {
                                let param_strs: Vec<String> = var_params.iter().map(|p| {
                                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let typ = p.get("typeName").and_then(|v| v.as_str()).unwrap_or("?");
                                    format!("var {name}: {typ}")
                                }).collect();
                                println!("   Var params: {}", param_strs.join(", "));
                            }
                        }

                        println!("   {example}\n");
                    }
                }
                if partial {
                    eprintln!("Note: Some call paths are still being analyzed. Results may be incomplete.");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}
```

Run: `cargo build -p al-cli`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/al-cli/src/main.rs crates/al-cli/src/commands/insight.rs
git commit -m "feat(cli): update suggest-event command with structured --object/--table/--event flags"
```

---

### Task 9: Integration test

**Files:**
- Modify: `crates/al-lsp/tests/integration.rs` (or create new test)

- [ ] **Step 1: Write integration test**

Add a test that exercises the full pipeline: symbol index → insight graph → call edges → suggest_event query.

```rust
#[test]
fn suggest_event_integration_procedure_query() {
    let ws = al_core::workspace::Workspace::new();

    // Add symbols: a codeunit with a method and events, plus a table with trigger events
    ws.symbols.add_entries(&[
        al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            methods: vec![
                al_symbols::MethodSymbol {
                    name: "PostSalesDoc".to_string(),
                    parameters: vec![al_symbols::ParameterSymbol {
                        name: "SalesHeader".to_string(),
                        type_name: "Record \"Sales Header\"".to_string(),
                        is_var: true,
                    }],
                    return_type: None,
                    attributes: vec![],
                    is_local: false,
                },
                al_symbols::MethodSymbol {
                    name: "OnAfterPostSalesDoc".to_string(),
                    parameters: vec![al_symbols::ParameterSymbol {
                        name: "SalesHeader".to_string(),
                        type_name: "Record \"Sales Header\"".to_string(),
                        is_var: true,
                    }],
                    return_type: None,
                    attributes: vec![al_symbols::AttributeSymbol {
                        name: "IntegrationEvent".to_string(),
                        arguments: vec!["false".into(), "false".into()],
                    }],
                    is_local: false,
                },
            ],
            ..Default::default()
        },
    ]);

    use al_core::queries::suggest_event::*;

    // Test 1: Procedure query finds published events
    let result = suggest_event(&ws, &EventQuery {
        source: QuerySource::Procedure {
            object: "Sales-Post".to_string(),
            procedure: None,
        },
        filter_table: None,
        filter_field: None,
    });
    assert!(!result.integration_points.is_empty());
    assert!(result.integration_points.iter().any(|ip| ip.event == "OnAfterPostSalesDoc"));

    // Test 2: Table query finds var params
    let result = suggest_event(&ws, &EventQuery {
        source: QuerySource::Table { table: "Sales Header".to_string() },
        filter_table: None,
        filter_field: None,
    });
    assert!(result.integration_points.iter().any(|ip|
        ip.params.iter().any(|p| p.is_var && p.type_name.contains("Sales Header"))
    ));

    // Test 3: Combined query (procedure + table filter)
    let result = suggest_event(&ws, &EventQuery {
        source: QuerySource::Procedure {
            object: "Sales-Post".to_string(),
            procedure: None,
        },
        filter_table: Some("Sales Header".to_string()),
        filter_field: None,
    });
    assert!(result.integration_points.iter().all(|ip|
        ip.params.iter().any(|p| p.is_var && p.type_name.to_lowercase().contains("sales header"))
    ));
}
```

Run: `cargo test -p al-lsp -- suggest_event_integration`
Expected: PASS

- [ ] **Step 2: Run full test suite**

```bash
cargo test --workspace --exclude zed-al
```

Expected: All tests PASS

- [ ] **Step 3: Commit**

```bash
git add crates/al-lsp/tests/integration.rs
git commit -m "test: add integration tests for structured suggest_event queries"
```

---

### Task 10: Cleanup and final verification

- [ ] **Step 1: Run clippy**

```bash
cargo clippy --workspace --exclude zed-al -- -D warnings
```

Expected: No warnings

- [ ] **Step 2: Run formatter**

```bash
cargo fmt --all
```

- [ ] **Step 3: Final full test run**

```bash
cargo test --workspace --exclude zed-al
```

Expected: All PASS

- [ ] **Step 4: Final commit if any formatting changes**

```bash
git add -A && git commit -m "chore: clippy + fmt cleanup"
```
