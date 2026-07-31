# Analysis & Insight Engine

**Modules:** `crates/al-insight/src/` (graph engine) + analysis queries in
`crates/al-analysis/src/queries/` · **Status:** ✅ shipped

The graph-based analysis engine is available through the shared daemon, `al-explorer --json`, MCP,
and checkout-local contributor tasks. Results are sorted so CI output remains stable across runs.

## The insight graph (`insight/`)

The foundation is a directed graph built from the symbol index **and** workspace source, merged so
both package symbols and your code contribute.

| Component | File | Role |
| --- | --- | --- |
| Graph model | `graph.rs` | petgraph `DiGraph` of `InsightNode` (Object/Procedure/Event/Subscriber) and `InsightEdge` (Extends/Calls/Publishes/SubscribesTo/Contains/RelatesTo/Triggers); O(1) node lookup and edge dedup |
| Call graph | `calls.rs`, `index.rs` | adjacency lists with **both** outgoing and incoming edges (so "callers of X" is O(deg) not O(\|E\|)); edge kinds DirectCall/EventSubscription/TriggerInvocation/RecordTrigger |
| Traversal/search | `search.rs` | event tracing, entry-point discovery, DOT/JSON export |
| Helpers | `analysis.rs` | table-impact, TableRelation parsing, record-type matching |
| Discovery | `discovery.rs` | full publisher/subscriber map + orphan subscribers |

Record operations such as `Rec.Insert(true)` produce `OnBefore/OnAfterInsertEvent` trigger edges;
non-literal trigger arguments are conservatively treated as firing the trigger. Member calls resolve
the receiver's declared object type before lookup, and workspace subscribers are connected after the
graph is built. Event traversal detects cycles and enforces a 10,000-node global bound.

## Analysis catalog

| Analysis | File | Question it answers |
| --- | --- | --- |
| **Impact** | `queries/impact.rs` | "If I change this object/member, what breaks?" — extensions, pages/reports sourced from a table, record variables/parameters, callers, TableRelation filters, event subscribers. `--table` groups all consumers of a table. |
| **Table impact** | `insight/analysis.rs` | Record variables, parameters, relations, extensions touching a table, grouped by object. |
| **Event tracing** | `insight/search.rs` | `trace` lists all subscribers of an event; `trace --tree` follows the full multi-hop publisher→subscriber→call chain (diamonds, cycles marked). |
| **Subscriber/source resolution** | `symbols/events.rs`, `insight/discovery.rs` | Find subscribers of an event; resolve the publisher behind an `[EventSubscriber]`; full interception map incl. orphan subscribers. |
| **Suggest event** | `queries/suggest_event.rs` | "What integration events can I subscribe to along this path?" with ready-to-paste `[EventSubscriber(...)]` examples; flags `partial` when source is unindexed. |
| **Entry points** | `insight/search.rs` | Procedures with no incoming calls (test/root-cause candidates). |
| **Dead code** | `queries/dead_code.rs` | Unused procedures, unreferenced fields, orphaned subscribers — with **confidence levels** (high for provably-unreachable locals; medium for public symbols extensions might call). |
| **SQL anti-patterns** | `queries/sql_patterns.rs` | `FindFirst`/`Get`/`CalcFields` in loops, unfiltered `FindSet` — the classic N+1 and table-scan patterns. |
| **Architecture lint** | `queries/arch_lint.rs` | Project rules from `.alarch.json`: naming conventions, forbidden patterns, required properties, max complexity. |
| **Breaking changes** | `queries/breaking_changes.rs` | Cross-version public-surface diff: removed objects/procedures/fields/enum values, signature/return-type changes. |
| **Upgrade report** | `queries/upgrade.rs` | Breaking changes + data-migration hints + obsolete-symbol warnings, with guidance. |
| **Obsolescence** | `queries/obsolescence.rs` | Inventory of `[Obsolete]` symbols with state/reason/tag and caller counts. |
| **Data-classification audit** | `queries/audit.rs` | GDPR posture: every table field's `DataClassification` and a risk level. |
| **Permission audit** | `queries/audit.rs` | Permission-set coverage plus unused object grants (`overBroad`) and granted I/M/D rights without a corresponding observed write (`overGrantedRights`). |
| **Dependency graph** | `queries/deps.rs` | GUID-keyed transitive tree from the typed current `app.json` and loaded `.app` manifests; implicit dependencies, missing packages, duplicate versions, unsatisfied minimum versions, deterministic JSON/DOT export. |
| **Duplicates** | (daemon `duplicates`) | Repeated AL code blocks (configurable min tokens/similarity, clamped to safe bounds). |
| **Profiler hints** | `queries/profiler_hints.rs` | Map `.alcpuprofile` (Chrome DevTools) hotspots to AL procedure declaration lines. |
| **Complexity metrics** | `syntax/complexity.rs` | Cyclomatic + cognitive complexity per procedure with thresholds. |
| **Bulk fixes** | `queries/bulk_fix.rs` | Project-wide: add `ApplicationArea`, add `ToolTip` (from base-app field data), add `DataClassification` (skips FlowFields). Idempotent, dry-run supported. |

### How a few of these work (highlights)

- **Dead code** builds workspace-global call-name and member-access sets once, then checks each file
  in parallel. It is quote- and comment-aware so
  `Message('FindFirst()')` and fields inside `/* */` don't create false positives, and it excludes
  event publishers (they're entry points).
- **SQL scan** is a quote-aware text state machine tracking loop nesting and per-loop `begin..end`
  depth. It clears the "unfiltered" flag on `SetRange`/`SetFilter` before a `FindSet`.
- **Breaking changes** diffs `(kind, name)` maps of a baseline vs current `SymbolEntry` set, comparing
  public methods, fields, and enum values.
- **Profiler analysis** parses Chrome profiles, skips synthetic nodes (`(root)`/`(idle)`/GC),
  aggregates sampled `timeDeltas`, and rolls total time up the call tree. The `profiler-hints`
  command maps supplied procedure hotspots to workspace declarations.
- **Architecture lint** validates `.alarch.json` before running. Naming conventions accept one Rust
  regular expression. Forbidden patterns are case-insensitive literals by default and become Rust
  regular expressions when `"regex": true`; invalid expressions fail configuration loading instead
  of silently disabling a rule. `pattern` remains an exact, case-insensitive object-kind scope.
  [`schemas/alarch.json`](../../schemas/alarch.json) is the editor schema.

## Microsoft comparison

| Analysis | This project | Microsoft AL extension |
| --- | --- | --- |
| Dead code (with confidence) | ✅ | ❌ |
| Multi-hop event chain trace | ✅ (`trace --tree`) | ❌ |
| SQL anti-pattern scan | ✅ | ❌ (some via 3rd-party analyzers like LinterCop) |
| Impact / table impact | ✅ detailed | partial (find-references only) |
| Breaking-change / upgrade report | ✅ | ❌ |
| Architecture lint (`.alarch.json`) | ✅ | ❌ |
| Obsolescence timeline | ✅ | ❌ |
| Data-classification audit | ✅ | ❌ |
| Dependency graph + conflicts | ✅ (DOT) | ❌ |
| Profiler hotspot → source | ✅ | partial (profiler exists; not this mapping) |
| Bulk property fixes | ✅ (3 ops, project-wide) | partial (per-file quick fixes) |

## Why this approach

These analyses share one graph and one set of parse trees, so adding a new question is cheap and
every answer is fast. Because they live in `queries/`/`insight/` (transport-agnostic), each is a CLI
subcommand with `--json` and a daemon method — which means you can gate a build on "no new dead code"
or "no SQL anti-patterns" in CI, and MCP clients can call the same analyses. Deterministic output
makes results safe to diff in CI.

## How to use

CLI (all support `--json`; most have a matching *AL: …* Zed task):

```
al-explorer impact <symbol> [--table]      al-explorer dead-code
al-explorer trace <event> [--depth N] [--tree]   al-explorer subscribers <event>
al-explorer suggest-event --object|--procedure|--table|--field|--event <x>
al-explorer entrypoints      al-explorer intercept      al-explorer graph --format json|dot
al-explorer sql-scan         al-explorer arch-lint      al-explorer duplicates
al-explorer breaking --baseline-app <old.app>
al-explorer upgrade --baseline-app <old.app>             al-explorer obsolete
al-explorer audit-data       al-explorer permission-audit
al-explorer deps             al-explorer deps-graph     al-explorer metrics [--all]
al-explorer profiler-hints [Object.Procedure ...]
al-explorer add-application-area | add-tooltips | add-data-classification [--dry-run]
```

MCP exposes the complete shared dispatcher through `al_call`, including every analysis above. Common
agent workflows also have descriptive aliases such as `al_impact`, `al_deadcode`, `al_sqlscan`,
`al_entrypoints`, and `al_trace_event` (see [ai-mcp](./ai-mcp.md)). The aliases are conveniences, not
an MCP allow-list. The TUI surfaces event chains, call-graph/impact, and profiler views interactively
(see [cli-and-tui](./cli-and-tui.md)).

## Compatibility boundaries

- `breaking` and `upgrade` require `--baseline-app <old.app>` for a cross-version result. Without a
  baseline they explicitly report that the comparison was not evaluated.
- Package-only call sites cannot be recovered because `.app` symbols do not contain method bodies.
- Four conservative table/page layering rules are enabled by default; `.alarch.json` adds
  project-specific literal or regex-backed rules.
- Permission over-grant analysis cannot prove dynamic `RecordRef`/`FieldRef` writes, unresolved
  interface dispatch, or writes inside dependency packages without source bodies.
- Profiler data without `samples` and `timeDeltas` falls back to the legacy hit-count estimate.
