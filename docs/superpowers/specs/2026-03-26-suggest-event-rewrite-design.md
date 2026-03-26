# Suggest Event Rewrite — Structured Integration Point Discovery

**Date:** 2026-03-26
**Status:** Approved design
**Scope:** `al-core/src/insight/calls.rs` (new), `al-core/src/queries/suggest_event.rs` (rewrite), daemon/CLI API changes

## Problem

The current `suggest_event` module accepts a natural-language description and does naive keyword matching against event names. This is wrong. The actual need is:

1. **"What can I hook into during this process?"** — Given an object or procedure (e.g. Sales-Post, or Sales-Post.PostSalesDoc), trace the full execution path and return every event that fires — directly published events, events triggered by record operations, and events reachable through the subscriber chain.
2. **"Where can I get my hands on this record?"** — Given a table name (e.g. Sales Header), find every event where that table is exposed as a `var` parameter so a subscriber can modify it.
3. **Combined** — Trace a process AND filter to events exposing a specific table.

## Architecture

### Existing Infrastructure

The InsightGraph (`al-core/src/insight/graph.rs`) and CallGraph (`al-core/src/insight/index.rs`) already provide the graph structure. Key existing capabilities:

- `InsightNode::Event` and `InsightNode::Subscriber` nodes built from `.app` symbol data
- `InsightEdge::SubscribesTo` edges linking subscribers to events (from `.app` packages)
- `InsightEdge::Publishes` edges linking objects to their events
- `CallGraph::subscribers_of(event_id)` — O(1) reverse lookup via `incoming` HashMap
- `trace_event_chain()` — tree-shaped event chain tracing with cycle detection
- `table_impact()` — finds objects that reference a table as var parameter

**What's missing:**
- `Calls` edges (direct procedure-to-procedure calls) — the variant exists but is never populated
- Record operation awareness (`.Insert(true)` triggering table events)
- Workspace subscriber detection (subscribers in the user's `.al` files, not in `.app` packages)
- A query layer that accepts structured input

### New Module: `insight/calls.rs` — Call Edge Extraction

Walks the tree-sitter ASTs already cached in `file_index.file_trees` to extract three kinds of edges per procedure:

#### 1. Direct Calls → `InsightEdge::Calls`

Patterns to detect:
- `ObjectName.ProcedureName(...)` — qualified call, resolve object against symbol index
- `ProcedureName(...)` — bare call, resolve against current object's methods first, then symbol index
- `Codeunit.Run(ObjectName)` — indirect codeunit invocation, maps to target's `OnRun` trigger

Resolution is best-effort. Unresolvable targets (e.g. method call on a variable whose type can't be determined) are skipped. No false positives, possible false negatives.

#### 2. Record Operations → `InsightEdge::Triggers` (new variant)

Track variable types from `var` sections and parameters (`HashMap<String, String>` — variable name → record type). Detect:

| Pattern | RunTrigger | Generated edges |
|---|---|---|
| `Rec.Insert(true)` or `Rec.Insert()` (default true) | yes | → Table's `OnBeforeInsertEvent`, `OnAfterInsertEvent` |
| `Rec.Insert(false)` | no | No edge |
| `Rec.Modify(true)` or `Rec.Modify()` | yes | → Table's `OnBeforeModifyEvent`, `OnAfterModifyEvent` |
| `Rec.Modify(false)` | no | No edge |
| `Rec.Delete(true)` or `Rec.Delete()` | yes | → Table's `OnBeforeDeleteEvent`, `OnAfterDeleteEvent` |
| `Rec.Delete(false)` | no | No edge |
| `Rec.Validate(Field, Value)` | always | → Field's `OnBeforeValidateEvent`, `OnAfterValidateEvent` |

Variable type resolution: look up the variable name in the procedure's local `var` section, then the object's global `variables`, then the procedure's `parameters`. The type string (e.g. `Record "Sales Header"`) is parsed to extract the table name, then the table's events are looked up in the symbol index.

#### 3. Workspace Subscribers → `InsightEdge::SubscribesTo`

During the same AST walk, detect `[EventSubscriber(...)]` attributes on procedures. Parse the attribute arguments (same format as `.app` symbol data) and register:
- A `Subscriber` node in the InsightGraph
- A `SubscribesTo` edge from the subscriber to the target event

This ensures workspace-local subscribers are part of the graph alongside `.app` package subscribers, and are found by `subscribers_of()` at query time.

### Tiered Loading Strategy

**Goal:** Users can work immediately after symbol indexing. Call edge extraction runs in the background.

#### Tier 1 — Eager (background, after symbol index ready)

1. Walk all workspace files doing a lightweight **fanout scan**: count call sites + record operations per procedure (no target resolution yet — just counting AST nodes). This is fast — one pass, no symbol lookups.
2. Score each object: `total_call_sites + total_record_ops` across all its procedures.
3. Fully resolve the top-scoring objects (configurable threshold, e.g. top 20% or score > N). These are the posting codeunits, journal handlers, document management — the procedures users actually need to trace.
4. **Transitive hot-path detection:** When resolving a Tier 1 object's calls, if a callout target is in another workspace object, promote that object to Tier 1 as well (breadth-first expansion, capped to prevent runaway).

#### Tier 2 — Lazy (on-demand, cached)

- When a query hits a procedure whose call edges aren't resolved, walk that procedure's AST on the spot.
- Cache the result — edges persist in the CallGraph once added.
- Subsequent queries for the same procedure are instant.

#### State Tracking

`DashMap<NodeId, EdgeResolutionState>` where:
```rust
enum EdgeResolutionState {
    Unresolved,
    Resolving,   // prevents duplicate work from concurrent queries
    Resolved,
}
```

#### Staleness

When a file changes (`didSave` / `didChange`):
- Invalidate all edges originating from procedures in that file (remove from CallGraph, reset state to `Unresolved`)
- If the object was Tier 1, re-resolve eagerly in the background
- If Tier 2, leave for lazy re-resolution

### Rewritten `suggest_event.rs` — Query Layer

#### Input

```rust
pub struct EventQuery {
    pub source: QuerySource,
    pub filter_table: Option<String>,
    pub filter_field: Option<String>,
}

pub enum QuerySource {
    /// Trace all events reachable from this object (or object + procedure)
    Procedure {
        object_name: String,
        procedure_name: Option<String>,
    },
    /// Find all events where this table is exposed as var or triggered
    Table {
        table_name: String,
    },
    /// Trace an event's subscriber chain downstream
    Event {
        object_name: String,
        event_name: String,
    },
}
```

If `procedure_name` is `None` for a `Procedure` source, trace the entire object — all its procedures' call paths.

`filter_table` and `filter_field` are post-filters: only return integration points where a `var` parameter's type resolves to that table (and optionally references that field).

#### Output

```rust
pub struct SuggestEventResult {
    pub query: EventQuery,
    pub integration_points: Vec<IntegrationPoint>,
    pub partial: bool,  // true if some paths weren't fully resolved (Tier 2 pending)
}

pub struct IntegrationPoint {
    pub event: String,
    pub object: String,
    pub event_type: String,         // "integration" | "business" | "trigger"
    pub params: Vec<ParamInfo>,
    pub path: Vec<TraceHop>,        // how we reached this event from the query source
    pub example: String,            // ready-to-paste [EventSubscriber] attribute
}

pub struct ParamInfo {
    pub name: String,
    pub type_name: String,
    pub is_var: bool,
}

pub struct TraceHop {
    pub object: String,
    pub procedure: String,
    pub edge_kind: String,          // "call" | "trigger" | "subscription"
}
```

#### Query Behavior

**Procedure source:**
1. Look up the procedure's `NodeId` in the InsightGraph
2. Walk `Calls` + `Triggers` edges transitively (lazy-resolve Tier 2 procedures on demand)
3. At each `Event` node: collect as integration point, then follow `SubscribesTo` edges (reverse) to find all subscribers, recurse into each subscriber's procedure
4. Cap recursion depth (default 10), detect cycles via visited set
5. Apply `filter_table` / `filter_field` post-filters

**Table source:**
1. Find all events published by the table itself (OnBeforeInsert, OnAfterInsert, OnBeforeModify, etc.)
2. Scan the symbol index for events where this table appears as a `var Record "Table Name"` parameter (reuse pattern from `analysis.rs::is_record_of`)
3. For each found event, include its subscribers
4. If `filter_field` is set, narrow to field-level validate events

**Event source:**
1. Look up the event's `NodeId`
2. Delegate to existing `trace_event_chain` logic
3. Reformat into `IntegrationPoint` results with trace paths

**Partial results:** If any procedure in the trace has `EdgeResolutionState::Unresolved` and lazy resolution would be too expensive (e.g. mid-Tier-1 build), set `partial: true` on the response.

### API Changes

#### Daemon Dispatch

Current: `{ "description": "post sales document" }`

New:
```json
{ "query": { "source": { "type": "procedure", "object": "Sales-Post", "procedure": "PostSalesDoc" }, "filterTable": "Sales Header" } }
```

Or:
```json
{ "query": { "source": { "type": "table", "table": "Sales Header" } } }
```

#### CLI

Current: `al suggest-event "post sales document"`

New:
```
al suggest-event --object "Sales-Post"
al suggest-event --object "Sales-Post" --procedure "PostSalesDoc"
al suggest-event --object "Sales-Post" --table "Sales Header"
al suggest-event --table "Sales Header"
al suggest-event --table "Sales Header" --field "No."
al suggest-event --event "Sales-Post" "OnAfterPostSalesDoc"
```

The `--object`/`--procedure` flags set the `Procedure` source. `--table` alone sets the `Table` source. `--table` combined with `--object` sets `Procedure` source with `filter_table`. `--event` sets the `Event` source.

## Files Changed

| File | Change |
|---|---|
| `al-core/src/insight/calls.rs` | **New.** Call edge extraction from tree-sitter ASTs. Fanout scoring. Tiered resolution. |
| `al-core/src/insight/graph.rs` | Add `InsightEdge::Triggers` variant. Add method to remove edges by source node (for invalidation). |
| `al-core/src/insight/index.rs` | Add `add_trigger_edge()`. Add `EdgeResolutionState` tracking. Add `remove_edges_from()` for invalidation. |
| `al-core/src/insight/mod.rs` | Export `calls` module. |
| `al-core/src/queries/suggest_event.rs` | **Full rewrite.** `EventQuery`/`IntegrationPoint` types, query logic delegating to InsightGraph + CallGraph. |
| `al-core/src/workspace.rs` | Trigger background call-edge extraction after `get_or_build_insight_graph()`. Invalidation on file change. |
| `al-lsp/src/daemon/insight_dispatch.rs` | Update `dispatch_suggest_event` to accept structured JSON input. |
| `al-lsp/src/daemon/mod.rs` | Route unchanged (`"suggestEvent"`), params shape changes. |
| `al-cli/src/commands/insight.rs` | Update `cmd_suggest_event` for new CLI flags. |
| `al-cli/src/main.rs` | Update clap argument definitions. |

## Testing Strategy

- **Unit tests in `calls.rs`:** Parse small AL snippets with tree-sitter, verify correct edge extraction for direct calls, record operations (true/false), subscriber detection.
- **Unit tests in `suggest_event.rs`:** Build a Workspace with known symbols and pre-populated call edges, verify each query source type returns correct integration points with correct trace paths.
- **Integration test:** Use the test fixture project (`al-test-harness/data/test_al_project/`), run the full pipeline (index → build InsightGraph → extract call edges → query), verify end-to-end results.
- **Tiered loading test:** Verify Tier 1 resolves high-fanout objects first, Tier 2 resolves lazily on query, staleness invalidation works on file change.

## Non-Goals

- Natural-language input (removed entirely)
- Call tracing inside `.app` package procedure bodies (no source available — only their published events and subscriber attributes are indexed)
- Real-time incremental re-parsing on every keystroke (invalidation is on save, not on edit)
