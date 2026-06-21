# Analysis & Insight Engine

**Modules:** `crates/al-core/src/insight/` (graph engine) + analysis queries in
`crates/al-core/src/queries/` · **Status:** ✅ shipped (a few items phase-gated, noted inline)

This is the largest source of "things Microsoft's extension doesn't ship." It is a graph-based code
analysis engine plus a suite of specialized analyses, all available from the CLI (with `--json`), the
daemon, Zed tasks, and (a curated subset) MCP. Everything is deterministic — results are sorted so CI
output is stable across runs.

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

Key correctness work baked in: record operations like `Rec.Insert(true)` are parsed into
`OnBefore/OnAfterInsertEvent` trigger edges (non-literal args over-approximate to "fires",
F-OPEN-087); member calls resolve the variable's declared object type before lookup (F-OPEN-084);
workspace subscribers are wired to events post-build so traces can descend (FB-8); event-chain
traversal has cycle detection plus a 10,000-node global bound; and results are sorted by node id for
determinism.

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
| **Permission audit** | `queries/audit.rs` | Permission-set coverage: which tables/pages/codeunits/reports are (un)covered by the workspace's permission sets, and by which set. |
| **Dependency graph** | `queries/deps.rs` | Full transitive dependency tree from `app.json` + packages; version-conflict and missing-dependency detection; DOT export. |
| **Duplicates** | (daemon `duplicates`) | Repeated AL code blocks (configurable min tokens/similarity, clamped to safe bounds). |
| **Profiler hints** | `queries/profiler_hints.rs` | Map `.alcpuprofile` (Chrome DevTools) hotspots to AL procedure declaration lines. |
| **Complexity metrics** | `syntax/complexity.rs` | Cyclomatic + cognitive complexity per procedure with thresholds. |
| **Bulk fixes** | `queries/bulk_fix.rs` | Project-wide: add `ApplicationArea`, add `ToolTip` (from base-app field data), add `DataClassification` (skips FlowFields). Idempotent, dry-run supported. |

### How a few of these work (highlights)

- **Dead code** builds workspace-global call-name / member-access sets once (O(F), fixing an earlier
  O(F²·P)), then checks each file in parallel (rayon). It is quote- and comment-aware so
  `Message('FindFirst()')` and fields inside `/* */` don't create false positives, and it excludes
  event publishers (they're entry points).
- **SQL scan** is a quote-aware text state machine tracking loop nesting and per-loop `begin..end`
  depth (ISSUE-145), clearing the "unfiltered" flag on `SetRange`/`SetFilter` before a `FindSet`.
- **Breaking changes** diffs `(kind, name)` maps of a baseline vs current `SymbolEntry` set, comparing
  public methods, fields, and enum values.
- **Profiler hints** parse the Chrome profile, skip synthetic nodes (`(root)`/`(idle)`/GC), and map
  function names to workspace procedure declaration lines (≈1 ms per sample).

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
or "no SQL anti-patterns" in CI, and an AI agent can call the same analysis through MCP. Microsoft's
equivalents, where they exist at all, are locked inside the VS Code extension and not scriptable.
Determinism (sorted output) makes them safe to diff in CI.

## How to use

CLI (all support `--json`; most have a matching *AL: …* Zed task):

```
al-explorer impact <symbol> [--table]      al-explorer dead-code
al-explorer trace <event> [--depth N] [--tree]   al-explorer subscribers <event>
al-explorer suggest-event --object|--procedure|--table|--field|--event <x>
al-explorer entrypoints      al-explorer intercept      al-explorer graph --format json|dot
al-explorer sql-scan         al-explorer arch-lint      al-explorer duplicates
al-explorer breaking         al-explorer upgrade        al-explorer obsolete
al-explorer audit-data       al-explorer permission-audit
al-explorer deps             al-explorer deps-graph     al-explorer metrics [--all]
al-explorer profiler-hints <file>
al-explorer add-application-area | add-tooltips | add-data-classification [--dry-run]
```

MCP exposes a curated subset: `al_impact`, `al_deadcode`, `al_sqlscan`, `al_entrypoints`,
`al_trace_event` (see [ai-mcp](./ai-mcp.md)). The TUI surfaces event chains, call-graph/impact, and
profiler views interactively (see [cli-and-tui](./cli-and-tui.md)).

## Limitations & roadmap

- 🟡 **Breaking-change/upgrade baseline is not wired** — `breaking`/`upgrade` run against an *empty*
  baseline today and therefore report no changes; the previous-version diff source still needs
  connecting.
- 🟡 **Permission audit** reports coverage (covered/uncovered by set), not yet over-broad permissions
  vs. actual usage.
- Package-only call sites cannot be recovered (no source bodies in `.app` symbols).
- `arch_lint` rule patterns are intentionally narrow (no regex) and `builtin_rules()` is currently
  empty/extensible.
- Profiler self-time is approximate (hit-count ≈ ms; `timeDeltas` aggregation is a TODO).
- `ROADMAP.md` directions: wire baselines for breaking/upgrade, make affected-test detection
  graph-based, upgrade coverage toward dynamic statement/branch coverage, and add MCP tools for
  suggest-event/test-classify/coverage/xliff/deps.
