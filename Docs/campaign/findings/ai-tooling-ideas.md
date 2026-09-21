# AI tooling: making Claude Code fast and accurate on Business Central

Workstream goal: an agent working on a BC extension should get answers from the
symbol index, call and event graphs and impact analysis in one tool call,
instead of decompiling `.app` files and grepping.

Measured 2026-09-21 against `al-explorer` / `al-lsp` 0.4.0 built from
`campaign/2026-09-21`, on a private customer workspace (8 packages, 12,336
package symbols, Base Application 28.3).

## 1. Surface inventory

Three layers, one engine. The daemon (`al-lsp daemon --project <dir>`) owns a
`Workspace` with the symbol index, semantic cache and insight graph. Everything
else is a front end onto `dispatch_request`:

- `al-explorer <subcommand>` spawns or reuses a project daemon over a unix socket.
- `al-lsp mcp --project <dir>` speaks MCP on stdio and forwards tool calls to the
  same dispatcher (`crates/al-lsp/src/server/mcp.rs:1325`).
- `al-lsp` with no mode argument is the LSP server for the editor.

An agent therefore sees the same capability under three names. The MCP tool list
is a curated subset; `al_call` is the escape hatch onto the full daemon catalog.

### 1a. MCP tools

Defined in `crates/al-lsp/src/server/mcp.rs`, function `tools()` at line 414.
Nineteen tools. Column `method` is the daemon method the tool forwards to.

| Tool | mcp.rs line | method | Input | Purpose |
| --- | --- | --- | --- | --- |
| `al_call` | 416 | (any) | `method` (string, required), `params` (object) | Escape hatch onto every daemon method below. |
| `al_debug` | 440 | `debug` | `cmd` enum of 12 (start/breakpoint/state/stack/variables/globals/expand/eval/continue/step/history/stop) plus ~30 optional fields | Drive a live BC native debug session. |
| `al_build` | 611 | `compile` | none | Compile and verify the project; returns success, diagnostics, `.app` path. |
| `al_downloadsymbols` | 621 | `downloadSymbols` | none | Pull dependency `.app` packages from NuGet into `.alpackages`. |
| `al_symbolsearch` | 628 | `search` | `query` (required), `limit` (1..500000, default 20) | Fuzzy object search across packages and workspace source. |
| `al_getdiagnostics` | 648 | `lint` | `file` (required) | Native lint diagnostics for one file. |
| `al_runtests` | 654 | `tests.run_auto` | none | Discover and run tests, routing each to interpreter or live BC. |
| `al_deadcode` | 663 | `deadCode` | none | Unused procedures, unreferenced fields, orphaned subscribers. |
| `al_sqlscan` | 670 | `sqlPatterns` | none | SQL anti-patterns across the workspace. |
| `al_entrypoints` | 677 | `entrypoints` | none | Procedures with no incoming calls. |
| `al_trace_event` | 684 | `trace` | `event` (required), `depth` (1..50, default 10) | Event propagation chain, publisher to subscribers. |
| `al_impact` | 704 | `impact` | `symbol` (required) | Who consumes this symbol. |
| `al_suggestevent` | 716 | `suggestEvent` | `query.source` discriminated by `type` (procedure/table/event), optional `filterTable`, `filterField` | Which integration event to subscribe to for a goal. |
| `al_testclassify` | 761 | `tests.classify` | none | Per-test routing decision (`interp`, `interpRecord`, `liveBc`, `snapshot`) plus reasons. |
| `al_testcoverage` | 775 | `tests.coverage` | none | Static coverage: which objects and procedures the tests reach. |
| `al_testsnapshot` | 783 | `tests.snapshot_capture` | `codeunitId`, `codeunitName`, `methodName`, `bcVersion`, `breakpoints[]`, `outputPath` required | Capture breakpoint-sampled variables on live BC. |
| `al_testsnapshotreplay` | 826 | `tests.snapshot_replay` | `snapshotPath`, `bcVersion` required | Re-run a recorded test and diff sampled state. |
| `al_depgraph` | 845 | `deps.graph` | `format`: `json` or `dot` | GUID-keyed dependency graph including transitive edges and version gaps. |

Tool-call plumbing, including the `al_call` routing and the agent-diagnostic
wrapper that appends `code`/`severity`/`reason`/`actions` hints to a result, is
at `crates/al-lsp/src/server/mcp.rs:1100` and `:891` onward.

### 1b. Daemon catalog (`al_call` methods)

The complete match arm list is the `match req.method.as_str()` in
`crates/al-lsp/src/server/daemon/mod.rs:661`. 92 distinct method names, counted
from the dispatcher itself; the earlier figure of 119 counted string literals
inside the arm bodies as well. `daemon_reference_names_every_dispatched_method`
in the same file pins `Docs/reference/daemon-methods.md` to that list.
Implementations live in four dispatch modules.

Symbol and language queries (`daemon/lsp_dispatch.rs`):

| Method | Impl | Input | Purpose |
| --- | --- | --- | --- |
| `hover` | lsp_dispatch.rs:55 | file, line, col | Type info at a position. |
| `definition` | :82 | file, line, col | Jump to definition. |
| `references` | :113 | file, line, col | All references. |
| `implementations` | :149 | file, line, col | Interface implementations. |
| `completions` | :168 | file, line, col | Completion list. |
| `signatureHelp` | :199 | file, line, col | Parameter help. |
| `rename` | :224 | file, line, col, newName | Workspace rename edits. |
| `documentSymbols` | :258 | file | File outline. |
| `foldingRanges` | :275 | file | Fold regions. |
| `semanticTokens` | :290 | file | Token classification. |
| `inlayHints` | :305 | file, optional line range | Inlay hints. |
| `codeActions` | :377 | file, range | Quick fixes. |
| `search` | :399 | query, limit | Fuzzy object search. |
| `object` | :571 | kind, name | Object by kind and name, with members. |
| `byId` | :725 | kind, id | Object by kind and numeric ID. |
| `events` | :811 | name | Event publishers matching a name. |
| `subscribers` | :857 | event | Subscribers to an event. |
| `composed` | :897 | kind, name | Base object merged with every extension. |
| `packages` | :1025 | none | Loaded `.app` packages with object counts. |
| `deps` | :1075 | none | Declared dependency list. |

Insight graph (`daemon/insight_dispatch.rs`):

| Method | Impl | Input | Purpose |
| --- | --- | --- | --- |
| `trace` | insight_dispatch.rs:30 | event, depth | Event propagation chain. |
| `entrypoints` | :62 | none | Procedures with no callers. |
| `graphExport` | :74 | format json/dot | Whole insight graph. |
| `insightStats` | :140 | none | Node and edge counts. |
| `deadCode` | :156 | none | Unused code report. |
| `nativeCheck` | :174 | none | Duplicate object IDs, IDs outside `idRanges`, duplicate names (AL-NC codes). |
| `impact` | :186 | symbol | Consumers of a symbol. |
| `suggestEvent` | :222 | query object | Integration events to subscribe to. |
| `tableImpact` | :268 | symbol | Table consumers grouped by object and operation kind. |
| `traceChain` | :281 | event, depth | Multi-hop propagation tree with cycle marks. |
| `eventMap` | :310 | none | Every publisher with its subscribers plus orphan subscribers. |

Source, build and package (`daemon/build_dispatch/`):

| Method | Impl | Input | Purpose |
| --- | --- | --- | --- |
| `source` | build/source_lookup.rs:213 | name, kind?, package?, procedure?, trigger? | Strongest available source for an object or one member, including from a dependency `.app`. |
| `location` | build/source_lookup.rs:28 | object reference | File and line of a symbol. |
| `eventSource` | build/source_lookup.rs:292 | file, line | Resolve the publisher behind an `[EventSubscriber]`. |
| `lint` | fixes.rs:13 | file or all | Native lint. |
| `format` | fixes.rs:97 | file / stdin / all, check | Format. |
| `fix` | fixes.rs:213 | file, rule?, dryRun | Apply registered fixes. |
| `fix.applicationArea` | fixes.rs:595 | value, dryRun | Add `ApplicationArea` to page and report controls. |
| `fix.tooltips` | fixes.rs:625 | fromTable, dryRun | Copy tooltips from base-app symbols. |
| `fix.dataClassification` | fixes.rs:706 | value, dryRun | Add `DataClassification` to fields. |
| `rules` | fixes.rs:460 | none | Lint rule catalog (static, needs no project). |
| `parse` | fixes.rs:555 | file | Parse tree info. |
| `arch.lint` | fixes.rs:863 | none | Architecture rules. |
| `metrics` | build/metrics.rs:11 | file or all, thresholds | Cyclomatic and cognitive complexity. |
| `profiler.hints` | build/metrics.rs:111 | hotspots[] | Optimisation hints for named hotspots. |
| `sqlPatterns` | mod.rs:383 | none | SQL anti-patterns. |
| `sortMembers` | build/organize.rs | file / all, dryRun | Canonical member order. |
| `organizeFiles` | build/organize.rs | dryRun | Rename files to `<Type><Id>.<Name>.al`. |
| `permissions` | codegen.rs:8 | format, name, id, roleId | Generate a permission set. |
| `permissions.audit` | mod.rs:55 | none | Permission set coverage. |
| `audit.dataClassification` | mod.rs:48 | none | `DataClassification` audit. |
| `obsolete` | mod.rs:41 | none | Obsolescence timeline. |
| `deps.graph` | mod.rs:62 | format | Full dependency graph. |
| `breaking` | mod.rs:288 | baselineApp | Breaking API changes against a baseline `.app`. |
| `upgrade` | mod.rs:363 | baselineApp | Upgrade analysis against a baseline `.app`. |
| `duplicates` | mod.rs:310 | minTokens, minSimilarity | Duplicate code blocks. |
| `compile` | build/compile.rs:93 | none | Compile and verify. |
| `package` | build/compile.rs:243 | none | Emit `.app`. |
| `newProject` | codegen.rs:104 | dir, name, publisher, template, runtime | Scaffold a project. |
| `generate` | codegen.rs:280 | kind, id, name, sourceTable?, pageType?, subject? | Scaffold one object. |
| `errorCodes` | codegen.rs:207 | none | Compiler error code list. |
| `builtinTypes` | codegen.rs:230 | none | Built-in types and methods. |
| `setup` | codegen.rs:276 | none | Toolchain check. |
| `clearCache` | symbols_auth.rs:9 | none | Drop the local symbol cache. |
| `authenticate` | symbols_auth.rs:81 | cmd, tenant | BC OAuth login / status / clear. |
| `downloadSymbols` | symbols_auth.rs:344 | project, source | Fetch dependency `.app` files. |
| `xlf.generate` / `xlf.refresh` / `xlf.untranslated` / `xlf.suggest` | xliff.rs:8 / :177 / :291 / :361 | project or xlf path | XLIFF translation workflow. |
| `snapshot` | build/snapshot_profiling.rs:9 | subcommand + server params | BC snapshot debugging. |
| `profiling` | build/snapshot_profiling.rs:166 | subcommand + server params | BC CPU profiling. |
| `debug` | debug_dispatch.rs | cmd + session params | Native debug session. |

Tests (`daemon/build_dispatch/tests_dispatch.rs`):
`tests.discover` (:321), `tests.coverage` (:352), `tests.run` (:398),
`tests.run_batch` (:536), `tests.run_auto` (:1318), `tests.last_results` (:1352),
`tests.affected` (:1492), `tests.classify` (:1544), `tests.snapshot_validate`
(:1589), `tests.snapshot_capture` (:1713), `tests.snapshot_replay` (:2182),
`tests.snapshot_diff` (:2422), `tests.mutate` (:2468).

Housekeeping, in `daemon/mod.rs` itself: `diag` (:800), `ping`, `status`
(reports pid, `indexedSymbols`, `workspaceFiles`, `workspaceObjects`,
`builtinTypes` and semantic cache hit/miss), `shutdown`.

### 1c. al-explorer subcommands

Declared in `crates/al-explorer/src/cli/args.rs`, routed in
`crates/al-explorer/src/cli/mod.rs:28-305`. Every subcommand takes a global
`--json`. Grouped by the agent job they serve:

Symbol lookup (`cli/commands/lsp/query.rs`):
`search <query> [--limit]` (:7), `object <kind> <name>` (:59),
`by-id <kind> <id>` (:66), `source <name> [--kind --package --procedure
--trigger]` (:73), `events <name>` (:132), `subscribers <event>` (:229),
`event-source <file> <line>` (:280), `composed [kind] <name>` (:353),
`packages` (:396), `deps` (:449).

Graph and impact (`cli/commands/insight.rs`):
`trace <event> [--depth --tree]` (:5), `entrypoints` (:48), `graph [--format]`
(:61), `insight-stats` (:96), `dead-code` (:104),
`impact <symbol> [--table]` (:190), `intercept` (:377),
`suggest-event [--object --kind --procedure --table --field --event]` (:451).

Language services (`cli/commands/lsp/language.rs`):
`lint`, `format`, `hover`, `definition`, `references`, `signature`,
`completions`, `symbols`, `folding`, `tokens`, `rename`.

Quality (`cli/commands/lsp/quality.rs`, `reports.rs`, `project.rs`):
`metrics`, `sql-scan`, `hints`, `fix`, `add-application-area`, `add-tooltips`,
`add-data-classification`, `obsolete`, `audit-data`, `permission-audit`,
`deps-graph`, `breaking --baseline-app`, `arch-lint`, `native-check`,
`duplicates`, `upgrade --baseline-app`, `profiler-hints`, `sort-members`,
`organize-files`, `permissions`, `rules`, `error-codes`, `builtins`, `parse`.

Build and project: `compile`, `pack-native`, `package`, `new`, `generate`,
`download-symbols`, `setup`, `doctor`, `init-debug`, `authenticate`,
`clear-cache`, `daemon-shutdown`, `diag`, `version`, `generate-completions`.

Tests (`cli/commands/lsp/tests.rs`):
`tests` (:7), `test-coverage` (:34), `test-run <codeunit>` (:86),
`test-affected <files...>` (:172), `test-results` (:210), `test-classify` (:289),
`test-run-all` (:352), `test-mutate` (refactor.rs:397),
`test-snapshot capture|validate|replay|diff` (refactor.rs:189).

Live BC: `debug start|breakpoint|state|eval|continue|step|history|stop`,
`snapshot start|list|download`, `profile start|stop|analyze`,
`xlf generate|refresh|untranslated|suggest`.

TUI: running `al-explorer` with no subcommand opens the terminal UI
(`crates/al-explorer/src/tui.rs`, views in `crates/al-explorer/src/views/`:
object browser, call graph, event chain, profiler, test runner). Not useful to
an agent.

## 2. Measurements on a real workspace

### Setup

Workspace: a private per-tenant extension (64 AL files, 65 workspace objects, ID range
50000-50099). Its `.alpackages` holds 13 `.app` files including Base Application
28.3 (45 MB), System Application, System, Business Foundation, MobileNAV and the
MobileNAV Configuration framework. Loaded: 8 packages, 12,336 package symbols
plus 8 workspace symbols, 9,416 objects in Base Application alone.

Binaries: `target/release/al-explorer` and `target/release/al-lsp` at
`0.4.0`, built from `campaign/2026-09-21`. Every call used `--json`, was timed
with `date +%s%N` around the process, and its stdout byte count recorded.
Token estimate is bytes / 4.

The runs used a copy of the project at
`$SCRATCH/workspace` with `.alpackages` symlinked to the original, because the
real workspace cannot be indexed at all (see the first finding below).

### Finding 0: a typo in `launch.json` blocks every symbol query

The real `.vscode/launch.json` has `"environmentType": "Sandboclaclx"`
(a stray keystroke) and a trailing comma. The daemon refuses to start:

```
ERROR al_lsp: Daemon failed error=Invalid AL launch configuration at
'.../.vscode/launch.json': configuration 0: unknown environmentType
"Sandboclaclx"; expected OnPrem, Sandbox, or Production
```

What the agent sees from `al-explorer --json packages` is:

```json
{"error": "al-lsp daemon exited before opening its endpoint (exit status: 1);
 inspect ~/.local/share/al-lsp/logs/al-lsp.log for the startup error"}
```

143 ms, 152 bytes, and no way to act on it. A debug-launch field that no symbol
query needs takes down the symbol index, and the diagnosis sits in a log file
the agent has to be told to read.

### Cold start

| Phase | Time | Notes |
| --- | --- | --- |
| Daemon spawn plus package symbol index | 1.4 s to 2.4 s | 8 packages, 12,336 symbols; the first call pays it |
| Second call on the live daemon | 29 ms | `diag`, same payload |
| Insight graph build | 87 ms | 68,684 nodes, 64,350 edges, built after the symbol index |
| Dependency AL source index | 54 s idle machine, 165 s under load | 9,900 source files extracted from the `.app` packages, 107 skipped |

The dependency source index is lazy: it starts when a method first needs it
(`subscribers`, `composed`, `events`, `lint`), not at daemon start. Until it
finishes those methods block, and the client's fixed 30 s timeout fires first.

There is no persistent on-disk symbol cache. `~/.local/share/al-lsp/<hash>/`
holds only `test-results.json`. Warmth lives in the daemon process, so the
1.4 s package index is paid again on every daemon restart and the 54 s source
index on every restart that needs it.

### Memory

| Point | Daemon RSS |
| --- | --- |
| After package symbol index, before source index | 134 MB |
| Peak while building the dependency source index | 2,791 MB |
| Steady state after the index, idle | 2,947 MB |

`diag` reports `symbolIndexMemory.trackedBytes` as 83.9 MB, which is the symbol
payload only and under-reports actual RSS by a factor of 35. A daemon per open
AL project at 3 GB each is the real constraint on running this alongside an
agent session, and nothing releases the source index once built.

### Question-by-question results

Latency is on a warm daemon unless the row says cold. `tok` is bytes / 4.

| # | Agent question | Command | Latency | Bytes | ~tok | Verdict |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Where is table "Item Ledger Entry" defined | `object table "Item Ledger Entry"` | 15 ms | 83,574 | 20,893 | Correct, unusable size. Answer is 3 fields (`package`, `id`, `namespace`); the other 83 KB is 70 methods, 86 fields, 17 keys, 6 variables |
| 2 | Fields of table 18 Customer | `by-id table 18` | 13 ms | 194,951 | 48,737 | Correct and complete: 165 fields. But 134 methods (33 KB) and 47 internal variables (4 KB) come with it, and there is no fields-only mode |
| 3 | Who subscribes to event `OnAfterPostSalesDoc` | `subscribers OnAfterPostSalesDoc` | 114 ms | 3 (`[]`) | 0 | **Wrong.** Returns empty. `trace` on the same event finds three subscribers (`Booking Manager`, `Notification Lifecycle Handler`, `CRM Sales Document Posting Mgt`). `subscribers` only searches workspace source and says nothing about that scope |
| 3b | Same, workspace event | `subscribers OnAfterRun` | 145 ms | 278 | 69 | Correct: finds `PTE Inventory Events.WhsePostReceiptOnAfterRun` on `Whse.-Post Receipt` |
| 3c | Same question via the graph | `trace OnAfterPostSalesDoc` | 145 ms | 842 | 210 | Correct, compact, and includes the second hop. This is the tool an agent should use, and its name does not say so |
| 4 | Who publishes `OnAfterPostSalesDoc` | `events OnAfterPostSalesDoc` | 34 ms | 2,496 | 624 | Correct, with full parameter signatures. Good size |
| 5 | Callers of `Sales-Post.PostSalesDoc` | `impact "Sales-Post.PostSalesDoc"` | 41 ms | 60 | 15 | **Wrong/silent.** `{"impacted": []}`, no note that the procedure name does not exist. The real name is `RunWithCheck` |
| 6 | What breaks if I change `Item."PTE Planning Category"` | `impact 'Item."PTE Planning Category"'` | 40 ms | 1,157 | 289 | Partly right. 5 hits, all `"confidence": "low"` with `"note": "name match only"`, and the list includes the field's own table, its own two pages and a permission set |
| 6b | Table-centric view | `impact "PTE Planning Category" --table` | 98 ms | 81 | 20 | **Wrong.** `totalImpacts: 0`, although the workspace has a page with that `SourceTable` and a table extension referencing it |
| 6c | Consumers of base table `Item` | `impact "Item"` | 142 ms | 356,686 | 89,171 | Correct and package-wide: 1,670 consumers, all `high` confidence. No `--limit`, no pagination, no workspace-only filter |
| 7 | Source of a base-app procedure | `source "Sales-Post" --procedure RunWithCheck` | 106 ms warm, 681 ms cold | 4,758 | 1,189 | **Correct, complete and compact.** The single best answer in the set. Replaces decompiling a 45 MB `.app` |
| 7b | Same without `--procedure` | `source "Sales-Post"` | 62 ms | 837,509 | 209,377 | Correct, 200k tokens. The whole codeunit |
| 7c | Wrong procedure name | `source "Sales-Post" --procedure PostSalesDoc` | 456 ms | 93 | 23 | `procedure 'PostSalesDoc' was not found in object 'Sales-Post' (code -32602)`. Accurate, but offers no candidate names, so the agent has to pull all 837 KB to find one |
| 8 | Free object IDs in 50000-50099 | none | - | - | - | **No tool answers this.** `native-check` returns `[]` (34 ms, 3 bytes): it flags IDs outside `idRanges` and duplicates, never which IDs are free |
| 9 | Which app defines codeunit 80 | `by-id codeunit 80` | 26 ms | 552,710 | 138,177 | Correct (`"package": "Base Application"`) but the answer is one string inside 138k tokens, 299 KB of which is 609 method signatures |
| 10 | Find a symbol by fuzzy name | `search "Planning Categ"` | 11 ms | 488 | 122 | **Correct, complete, compact.** Three hits with kind, id, name, package, source availability. `--limit 5` returned the same 488 bytes because only 3 matched |
| 11 | Base table merged with every extension | `composed table Item` | 10 ms (after source index) | 460,721 | 115,180 | Correct, 115k tokens. Times out at 30 s if the source index is still building |
| 12 | Every event and its subscribers | `intercept` | 25.6 s | 9,468,982 | 2,367,245 | Correct and entirely unusable: 2.4 M tokens, whole base app, no filter |
| 13 | Entry points | `entrypoints` | 114 ms | 6,347,056 | 1,586,764 | 39,209 entries, whole base app, no workspace filter |
| 14 | Which integration event should I subscribe to for Item | `suggest-event --table Item` | 235 ms | 492,740 | 123,185 | 690 integration points and `"partial": true`, with nothing saying what was cut or how to get the rest |
| 15 | Unused code | `dead-code` | 34 ms | 3,220 | 805 | Correct and compact. Workspace-scoped, as it should be |
| 16 | Dependency graph | `deps-graph` | 69 ms | 7,117 | 1,779 | Correct and compact |
| 17 | Diagnostics on one file | `lint objects/Inventory/InventoryEvents.Codeunit.al` | 20.4 s | 1,425 | 356 | Correct (4 AL-NL005 hits) but 20 s for one file, and 30 s timeout if the source index is still building |
| 18 | Workspace complexity | `metrics --all` | 33 ms | 18,213 | 4,553 | Reasonable |
| 19 | Test inventory | `tests` / `test-classify` | 15 ms / 163 ms | 3 / 28 | 0 / 7 | Correct: this app has no tests |
| 20 | Workspace stats | `diag` | 29 ms | 728 | 182 | Correct and compact |

### What the numbers say

Latency is not the problem. Twelve of the twenty queries answer in under 150 ms
on a warm daemon, and the symbol index reaches 12,336 symbols in 1.4 s. The
engine is fast enough for an agent to call it in a loop.

Output size is the problem. Five queries return more than 80,000 tokens, one
returns 2.4 million, and the agent-facing question behind each of them has a
one-line answer. Of the twenty, six produce output an agent can put in its
context unchanged (`search`, `trace`, `events`, `source --procedure`,
`dead-code`, `deps-graph`, `diag`).

Correctness is uneven at the edges. `subscribers` and `trace` disagree on the
same event. `impact --table` reports zero for a table that is in use. `impact`
on a nonexistent procedure returns an empty list rather than saying the symbol
does not exist. An agent has no way to tell an empty result from a wrong one.

The daemon at 3 GB and a lazy 54 s source index behind a fixed 30 s client
timeout mean the first minute of any new project is a sequence of timeouts with
no progress signal.

## 3. Gaps

### 3a. Questions no tool answers

| Question | Why it matters | Nearest existing surface |
| --- | --- | --- |
| Which object IDs in my `idRanges` are free? | Every new object starts here. Today the agent greps `objects/**/*.al` for `table 5000x` and hopes | `native-check` flags out-of-range and duplicate IDs; nothing enumerates free ones |
| Which field numbers are free in this table extension? | Same problem one level down, and the penalty for a collision is a failed deploy | none |
| What changed between two versions of a dependency `.app`? | The recurring upgrade question: BC 28.1 to 28.3 sits in `.alpackages` right now | `breaking` and `upgrade` take `--baseline-app`, but both compare the workspace to a baseline, not two packages to each other |
| Show only my workspace's part of this answer | `impact`, `entrypoints` and `intercept` all return the base app | no scope flag anywhere |
| Which of my subscribers point at an event that no longer exists? | The classic silent breakage after a BC upgrade | `intercept` reports `orphanSubscribers` inside a 9.4 MB payload |
| Where is this object's file on disk? | Needed before any edit | `location` exists as a daemon method but has no `al-explorer` subcommand and no MCP tool |
| What does this enum/option field accept? | Constant during page and validation work | buried in `by-id` output |
| Which permission set covers object N? | Needed before every deploy | `permissions.audit` audits coverage but is not queryable per object |

### 3b. Output too large for an agent

Ranked by tokens for a single call on this workspace:

| Call | Tokens | The answer inside it |
| --- | --- | --- |
| `intercept` | 2,367,245 | usually "does event X have subscribers" |
| `entrypoints` | 1,586,764 | usually the workspace's own entry points |
| `source <object>` (no `--procedure`) | 209,377 | one procedure body |
| `by-id codeunit 80` | 138,177 | one package name, or one method signature |
| `suggest-event --table Item` | 123,185 | two or three candidate events |
| `composed table Item` | 115,180 | which extension adds field X |
| `impact Item` | 89,171 | the first 20 consumers |
| `by-id table 18` | 48,737 | the field list |

Three separate causes, each fixable on its own:

1. **No projection.** `object`, `by-id` and `composed` always return methods,
   fields, keys, properties and variables together. An agent asking for fields
   pays for 134 method signatures.
2. **No limit or pagination.** `impact`, `entrypoints`, `intercept` and
   `suggest-event` return everything. Only `search` takes `--limit`, and its
   maximum is 500,000.
3. **Pretty-printed JSON.** `by-id codeunit 80` is 552,710 bytes indented and
   315,393 compact. A `--compact` flag is a 43% cut for one line of code.

`suggest-event` sets `"partial": true` without saying what was dropped or how
to ask for the rest, so a truncated answer is indistinguishable from a complete
one.

### 3c. Too many round trips

- **Find a base-app procedure body.** `search "Sales-Post"` to get the exact
  name, then `source "Sales-Post"` at 200k tokens to discover the procedure is
  called `RunWithCheck` and not `PostSalesDoc`, then
  `source ... --procedure RunWithCheck`. Three calls and 200k tokens for one
  4.7 KB answer, because the not-found error lists no candidates.
- **Decide where to subscribe.** `suggest-event` (123k tokens) then `events`
  then `trace` to check what already subscribes. One "give me the three best
  integration points for this goal, with signatures and existing subscribers"
  call would replace all three.
- **Add a field to a table extension.** Find free object ID (no tool), find
  free field number (no tool), check the base table's field numbers
  (`by-id`, 49k tokens), check nothing else uses the name (`impact`). Four
  steps, three of them unsupported.
- **Before-and-after on a change.** `impact` for consumers, `dead-code` for
  what the change orphans, `test-affected` for what to re-run. No single
  "what does this change touch" call.

### 3d. Error messages an agent cannot act on

| Message | Where | Problem |
| --- | --- | --- |
| `al-lsp daemon exited before opening its endpoint (exit status: 1); inspect ~/.local/share/al-lsp/logs/al-lsp.log` | `client.rs:527` | The cause (a bad `environmentType` in `launch.json`) is known at the point of failure and is not passed through. The agent is told to read a log |
| `Daemon did not respond within 30s — the operation may still be running. Retry with a longer timeout` | `client.rs:24` | There is no flag to set a longer timeout. It also does not say the dependency source index is building, or how far along it is |
| `Failed to read response: Connection reset by peer (os error 104)` | after `daemon-shutdown` | `daemon-shutdown` returns before the socket closes, so the next call races a dying daemon. No retry, no advice |
| `{"impacted": []}` | `impact` on a name that does not exist | Indistinguishable from a real zero. No "symbol not found" |
| `{"tableName": "...", "totalImpacts": 0}` | `impact --table` | Same, and here the zero is wrong |
| `error: unexpected argument 'objects/...'` | `event-source` | `event-source` takes `--file` and `--line`, unlike `hover`/`definition`/`lint` which take positionals. Inconsistent, and clap's message does not show the working form |
| `procedure 'PostSalesDoc' was not found in object 'Sales-Post'` | `source_lookup.rs:213` | Correct but dead-ends. The tool knows all 609 procedure names and offers none |

The MCP layer already has the right idea: `agent_diagnostic(code, severity,
summary, reason, actions)` at `mcp.rs:891` appends machine-readable next steps
to a result. It fires only for a few live-BC cases. Nothing on the
`al-explorer` path uses it, and none of the seven messages above go through it.

### 3e. Missing compact modes

No subcommand has a compact or summary mode. What each needs:

- `object` / `by-id` / `composed`: `--only fields|methods|keys|properties`,
  `--names-only`, `--compact`.
- `impact`: `--limit`, `--scope workspace|packages|all`, `--group-by object`.
- `entrypoints`, `intercept`, `dead-code`: `--scope workspace` as the default.
- `suggest-event`: `--limit` with an honest `truncated` count.
- `source`: a `--list-procedures` mode, so the agent can pick before it pays.

The daemon returns structured JSON throughout, so every one of these is a
projection over an existing result, not new analysis.

## 4. Plugin design

### 4a. Layout

The plugin lives in this repository so it ships with the binaries it drives.
Claude Code loads a plugin directory with `--plugin-dir`, from a marketplace, or
from `~/.claude/skills/`. Only `plugin.json` goes inside `.claude-plugin/`;
every other directory sits at the plugin root.

```
plugin/
├── .claude-plugin/
│   └── plugin.json
├── .mcp.json
├── skills/
│   ├── bc-symbol-lookup/SKILL.md
│   ├── bc-base-app-source/SKILL.md
│   ├── bc-event-map/SKILL.md
│   ├── bc-impact-check/SKILL.md
│   ├── bc-object-id-allocator/SKILL.md
│   ├── bc-test-locally/SKILL.md
│   ├── bc-upgrade-impact/SKILL.md
│   └── bc-workspace-health/SKILL.md
├── agents/
│   ├── bc-symbol-scout.md
│   └── bc-cop-fixer.md
└── scripts/
    └── al-bin.sh
```

`plugin.json`:

```json
{
  "name": "al-bc",
  "displayName": "AL / Business Central",
  "description": "Answer Business Central questions from the AL symbol index, call and event graphs and impact analysis instead of decompiling .app files.",
  "version": "0.1.0",
  "author": { "name": "Brad Fullwood" },
  "repository": "https://github.com/Brad-Fullwood/al.language.zed",
  "license": "MIT",
  "keywords": ["business-central", "al", "dynamics-365", "symbols"]
}
```

`.mcp.json`, wiring `al-lsp`'s MCP server to the project the session is in:

```json
{
  "mcpServers": {
    "al": {
      "command": "${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh",
      "args": ["al-lsp", "mcp", "--project", "${CLAUDE_PROJECT_DIR}"],
      "env": { "AL_LOG_FILE_LEVEL": "info" }
    }
  }
}
```

Tools then appear to the agent as `mcp__plugin_al-bc_al__al_symbolsearch` and
so on. `${CLAUDE_PROJECT_DIR}` pins the daemon to the workspace the session
opened, which matters because `run_mcp` canonicalizes the path and refuses to
start without one (`bin/al-lsp.rs:458`).

### 4b. Locating the binary

`find_al_lsp_binary` (`crates/al-protocol/src/client.rs:598`) looks next to the
running executable first, then walks `PATH`. So `al-explorer` and `al-lsp` have
to sit in the same directory, and that is the only constraint.

`scripts/al-bin.sh` resolves one in this order and fails with a message that
names the fix:

1. `$AL_BIN_DIR` if set.
2. `<repo>/target/release/` found by walking up from `${CLAUDE_PLUGIN_ROOT}`,
   for a developer working in this repository.
3. `PATH`.
4. `${CLAUDE_PLUGIN_DATA}/bin/`, where a `Setup` hook can drop a release
   download. `${CLAUDE_PLUGIN_DATA}` survives plugin updates, which makes it
   the right home for a 38 MB binary.

Not `bin/`. A plugin's top-level `bin/` is added to the Bash tool's `PATH`, but
it cannot be used by plugins distributed through claude.ai organization
settings, and a 47 MB pair of binaries does not belong in a git repository's
plugin directory anyway.

Skills call `al-explorer` through the same script, so the CLI and the MCP
server always agree on which build they are using.

### 4c. Skills

Each `SKILL.md` carries a `description` that is the trigger. Skills are
model-invoked, so the description is the only thing deciding whether the agent
reaches for the tool instead of grepping. Every description below names the
phrasing a BC developer actually uses.

**`bc-symbol-lookup`**

> Find a Business Central object, table, page, codeunit, enum or field in the
> symbol index. Use when asked where an object is defined, which app or
> extension defines object N, what fields a table has, what a codeunit's
> procedures are, or to find a symbol by partial name. Replaces grepping
> `.alpackages` or decompiling a `.app`.

Tools: `al_symbolsearch` first (488 bytes, always safe), then `al_call` with
`object`, `byId` or `location`. The skill's body teaches the size rule: search
before `by-id`, and never call `by-id` on a base-app codeunit without a
projection, because codeunit 80 is 138k tokens.

**`bc-base-app-source`**

> Read the real source of a base-app or dependency procedure, trigger or object
> from the `.app` packages in `.alpackages`. Use when asked what a standard BC
> procedure does, how Microsoft implements something, or to see code the
> workspace does not contain. Never decompile a `.app` by hand.

Tools: `al_call` with `source` and `--procedure`. The body makes the two-step
explicit: `search` for the exact object name, then `source` with a procedure
name; if the name is wrong, use the `--list-procedures` mode (item 3 in the
build list) rather than pulling the whole object.

**`bc-event-map`**

> Find Business Central integration events: which events a codeunit publishes,
> who subscribes to an event, what an event's parameters are, and which event
> to subscribe to for a given goal. Use when writing an `[EventSubscriber]`,
> choosing an integration point, or tracing why a subscriber does not fire.

Tools: `al_trace_event` (842 bytes, covers packages), `al_call` with `events`
for signatures, `al_suggestevent` for discovery. The body states plainly that
`subscribers` only sees workspace source and `trace` is the one to use for a
base-app event, until item 5 in the build list fixes the disagreement.

**`bc-impact-check`**

> Work out what a change to a Business Central table, field or procedure
> breaks. Use before renaming, obsoleting or changing the type of a field,
> before changing a procedure signature, or when asked what depends on
> something.

Tools: `al_impact`, `al_call` with `tableImpact`, `al_deadcode`, `al_call` with
`tests.affected`. The body requires the agent to state the scope of the answer,
because `impact` on a base-app table returns 1,670 package consumers that the
developer cannot change, and the actionable subset is the workspace's.

**`bc-object-id-allocator`**

> Pick the next free object ID or table-extension field number inside the app's
> declared `idRanges`. Use when creating any new table, page, codeunit, report,
> enum or permission set, or adding fields to a table extension.

Tools: a new `freeIds` daemon method (item 1 in the build list), with
`al_call` `nativeCheck` afterwards to confirm nothing collided. This is the
skill with the shortest path from "does not exist" to "saves a deploy failure".

**`bc-test-locally`**

> Run a Business Central extension's AL tests without a BC server, using the
> built-in Rust interpreter. Use when asked to run, write or check AL tests, to
> find which tests cover a change, or to see which tests need a live
> environment.

Tools: `al_testclassify` first, so the agent knows the local/live split before
it commits; then `al_runtests`, `al_testcoverage`, `al_call` with
`tests.affected`. The classify-first order is the point: the routing decision
tells the agent whether the answer is seconds away or needs a tenant.

**`bc-upgrade-impact`**

> Compare Business Central dependency versions and find what an upgrade breaks:
> removed or obsoleted symbols, changed signatures, subscribers pointing at
> events that no longer exist. Use when moving to a new BC release, when
> `.alpackages` has more than one version of an app, or when asked what a
> version bump breaks.

Tools: `al_call` with `breaking` and `upgrade` against a baseline `.app`,
`al_depgraph` for version conflicts, `al_call` with `obsolete` for the
deprecation timeline. `.alpackages` here holds both 28.1 and 28.3 of four
Microsoft apps, so the baseline is usually already on disk.

**`bc-workspace-health`**

> Audit a Business Central extension before a build or deploy: duplicate or
> out-of-range object IDs, SQL anti-patterns, dead code, missing
> `ApplicationArea`, `DataClassification` or tooltips, permission set coverage.
> Use before compiling, before deploying, or when asked to clean up an app.

Tools: `al_call` with `nativeCheck`, `al_sqlscan`, `al_deadcode`, `al_call`
with `audit.dataClassification` and `permissions.audit`. All workspace-scoped
and all compact today (`dead-code` is 805 tokens). This is the skill that hands
off to Brad's external `bc-build-deploy`.

### 4d. Subagents

Two, both to keep large tool output out of the main thread.

`agents/bc-symbol-scout.md`:

```yaml
---
name: bc-symbol-scout
description: Answer a Business Central symbol, event or impact question by querying the AL index, and return only the answer. Use when the question needs several lookups or the result would be large.
model: haiku
effort: low
tools: [Bash, Read, Grep]
skills: [al-bc:bc-symbol-lookup, al-bc:bc-event-map, al-bc:bc-base-app-source]
---
```

The whole argument for it is in section 2: `intercept` is 2.4 M tokens and
`impact Item` is 89 k. A Haiku scout can absorb those, filter, and hand back
twenty lines. Until the compact modes land this is the only way to use several
of these tools at all.

`agents/bc-cop-fixer.md` mirrors what `bc-build-deploy`'s SKILL.md already does
by hand: a cheap subagent that runs `lint`, fixes a batch, re-runs, and stops
at zero diagnostics in scope. Sonnet, `tools: [Bash, Read, Edit]`.

### 4e. Notes for the implementer

- Plugin-shipped agents cannot declare `hooks`, `mcpServers` or
  `permissionMode`. The MCP server has to come from the plugin's `.mcp.json`.
- The daemon holds 3 GB per project. A `SessionEnd` hook running
  `al-explorer daemon-shutdown` should be part of the plugin, and it needs the
  race in section 3d fixed first.
- `claude plugin validate ./plugin` before any commit, and
  `claude plugin eval` against the twenty questions in section 2 to measure
  whether the descriptions actually trigger.

## 5. New ideas, ranked by payoff per effort

Ranked by what they save an agent divided by what they cost to build. The first
five are cheap projections over results the daemon already computes.

**1. A global `--limit` and `--fields` projection on every list-returning
method.** The daemon returns structured JSON, so this is a filter applied at
the dispatch boundary, not new analysis. It converts `intercept` from 2.4 M
tokens to usable, `impact Item` from 89 k to 2 k, and `by-id table 18` from
49 k to about 3 k. Nothing else on this list comes close on ratio.

**2. `--scope workspace` as the default for `impact`, `entrypoints`,
`intercept` and `dead-code`.** Every one of these already knows which package a
result came from, because `impact` prints `"package": "Base Application"` on
each row. The developer can only change workspace code. Defaulting to it and
reporting `"packageMatches": 1642` as a count is a one-line answer where there
is currently an 89 k-token list.

**3. An object-ID and field-number allocator.** `freeIds` over `app.json`'s
`idRanges` and the workspace object index, returning next free per object kind
and the gaps. The workspace symbol index already holds every ID. This is the
one question in section 3a with zero coverage and a daily frequency.

**4. `source --list-procedures`.** Return the 609 names without the 837 KB of
bodies, and fold the same list into the `procedure not found` error as
candidates. Turns the three-call, 200k-token base-app source lookup into two
calls and 6 KB.

**5. Route every daemon error through `agent_diagnostic`.** The structure
exists at `mcp.rs:891` with `code`, `severity`, `summary`, `reason`, `actions`.
Wiring the seven messages in section 3d into it, and surfacing the daemon's
real startup error instead of "inspect the log", removes a class of
dead-ended agent turns. The `launch.json` typo cost this survey its first
workspace.

**6. A `bc_answer` composite tool.** One MCP tool taking a natural question
shape (`{about: "field", table: "Item", field: "No."}`) and returning the
assembled answer: definition, extensions that touch it, workspace consumers,
affected tests. Replaces the four-call patterns in section 3c. Moderate effort,
and it is the shape a Claude Code plugin actually wants, but it should come
after 1 and 2 or it inherits the size problem.

**7. Dependency `.app` diff.** `breaking` and `upgrade` compare the workspace
to a baseline. Comparing two packages to each other answers "what does
28.1 to 28.3 break for me", and both versions of four Microsoft apps are
already sitting in this workspace's `.alpackages`. The extraction and
comparison machinery is built; the entry point is not.

**8. Persist the symbol index to disk.** Cold start is 1.4 s for packages and
54 s for the dependency source index, paid on every daemon restart, and
`~/.local/share/al-lsp/<hash>/` holds nothing but test results today. A
content-hashed cache keyed on the `.app` files would make a fresh agent session
instant. Higher effort than everything above it, and it also needs the 2.9 GB
resident index addressed, since caching a 3 GB structure has its own cost.

**9. Report index progress instead of blocking.** The 30 s client timeout
against a 54 s lazy index is the worst first-minute experience available. A
`status` field saying `sourceIndex: building, 6200/9900 files` lets a skill
tell the agent to wait rather than retry into another timeout.

**10. A `--from-diff` mode.** Given `git diff --name-only`, return the union of
`impact`, `tests.affected` and `dead-code` for the changed objects. This is the
pre-commit question, and all three inputs exist.

### Where this meets Brad's external tools

`~/Projects/tools` stays external. Three places where its skills could call
these binaries instead of doing the work themselves:

- **`bc-build-deploy`** shells out to `dotnet alc` for every build to get
  Microsoft's cop diagnostics. `al-explorer native-check` runs in 34 ms and
  catches duplicate IDs, IDs outside `idRanges` and duplicate names before alc
  is invoked at all. Running it as a pre-flight would fail the obvious cases in
  under a second instead of a full compile. Its SKILL.md already writes the
  full diagnostic list to a file and feeds only a summary to the main context,
  which is the same discipline section 3b asks for.
- **`bc-build-deploy`'s cop-fixing subagents** re-run `bcbd.py build` after
  each batch. `al-explorer lint <file>` is 20 s for one file against a full
  compile, and `fix.applicationArea`, `fix.tooltips` and
  `fix.dataClassification` apply three of the most common cop fixes
  mechanically. Worth measuring the swap on a real cop backlog.
- **`mobilenav-config`** declares device pages against BC page objects and its
  Doctor checks for drift. `al-explorer composed page <name>` gives it the base
  page merged with every extension, and `impact` tells it which device
  declarations a page change breaks. That is the Doctor check it cannot write
  today.

## 6. Build list

Ten items, each independent enough for one agent to pick up. Sizes assume the
agent reads section 2 first. Items 1 to 5 are prerequisites for the plugin
being useful; 6 builds it; 7 to 10 improve it.

**1. Projection and limit on list-returning daemon methods (6 h)**

Add `limit` (integer) and `only` (array of `fields|methods|keys|properties|
variables`) to the params of `object`, `byId`, `composed`, `impact`,
`entrypoints`, `eventMap` and `suggestEvent`. Apply as a filter on the
`serde_json::Value` before it leaves the dispatcher, and add a `truncated`
count when `limit` cuts. Then mirror them as `--limit` / `--only` on the
matching `al-explorer` subcommands and add them to the MCP schemas.

Files: `crates/al-lsp/src/server/daemon/lsp_dispatch.rs` (571, 725, 897),
`crates/al-lsp/src/server/daemon/insight_dispatch.rs` (62, 186, 222, 310),
`crates/al-explorer/src/cli/args.rs`,
`crates/al-explorer/src/cli/commands/lsp/query.rs`,
`crates/al-explorer/src/cli/commands/insight.rs`,
`crates/al-lsp/src/server/mcp.rs` (628-860).

Done when `by-id table 18 --only fields --limit 20` is under 4 KB and
`intercept --limit 50` is under 20 KB.

**2. `scope` parameter on the package-wide reports (4 h)**

Add `scope` (`workspace` default, `packages`, `all`) to `impact`,
`tableImpact`, `entrypoints`, `eventMap`. Each result row already carries the
originating package, so the filter is on the existing field. Return
`"outOfScopeCount": N` so the agent knows what it did not see.

Files: `crates/al-lsp/src/server/daemon/insight_dispatch.rs` (62, 186, 268,
310), `crates/al-explorer/src/cli/args.rs`,
`crates/al-explorer/src/cli/commands/insight.rs`, `crates/al-lsp/src/server/mcp.rs`.

Done when `impact Item` defaults to the workspace's consumers and says how many
package consumers it left out.

**3. `source --list-procedures` and candidate names in the not-found error (3 h)**

Add a `listProcedures` boolean to the `source` params that returns names,
parameter counts and line numbers without bodies. When a named procedure is not
found, put the closest matches in the error message.

Files: `crates/al-lsp/src/server/daemon/build_dispatch/build/source_lookup.rs`
(213), `crates/al-explorer/src/cli/args.rs` (71-85),
`crates/al-explorer/src/cli/commands/lsp/query.rs` (73).

Done when the base-app procedure lookup in section 3c is two calls and under
10 KB.

**4. Free object ID and field number allocator (6 h)**

New daemon method `freeIds` taking `kind` (optional) and `count` (default 1),
reading `idRanges` from the typed `app.json` and the workspace object index.
Return next free per kind plus the gap list. A second mode takes a table or
table extension name and returns free field numbers. Expose as
`al-explorer free-ids` and MCP tool `al_freeids`.

Files: new `crates/al-lsp/src/server/daemon/build_dispatch/free_ids.rs`,
`crates/al-lsp/src/server/daemon/mod.rs` (dispatch arm near 589),
`crates/al-explorer/src/cli/args.rs`,
`crates/al-explorer/src/cli/commands/lsp/project.rs`,
`crates/al-lsp/src/server/mcp.rs` (tools list).

Done when `free-ids --kind table` on the workspace copy returns 50002 as the
next free table ID in 50000-50099.

**5. Fix `subscribers` and `impact --table`, and route errors through
`agent_diagnostic` (8 h)**

Three defects found in section 2, one file family:

- `subscribers` returns `[]` for `OnAfterPostSalesDoc` while `trace` finds
  three. Either widen it to package scope or make it say its scope in the
  result.
- `impact --table` returns `totalImpacts: 0` for a table that a workspace page
  uses as `SourceTable`.
- `impact` on a symbol that does not exist returns an empty list instead of a
  not-found error.

Then wire the seven messages in section 3d through `agent_diagnostic`, and pass
the daemon's real startup error through `client.rs` instead of
"inspect the log". Fix the `daemon-shutdown` race while in there: wait for the
socket to disappear before returning.

Files: `crates/al-lsp/src/server/daemon/lsp_dispatch.rs` (857),
`crates/al-lsp/src/server/daemon/insight_dispatch.rs` (186, 268),
`crates/al-lsp/src/server/mcp.rs` (891),
`crates/al-protocol/src/client.rs` (24, 527, 598),
`crates/al-explorer/src/cli/commands/lsp/env.rs` (108).

Done when a test asserts `subscribers` and `trace` agree on
`OnAfterPostSalesDoc`, and the `launch.json` typo produces its own error text
at the CLI.

**6. Build the plugin (8 h)**

The layout in section 4: `plugin/.claude-plugin/plugin.json`,
`plugin/.mcp.json`, `plugin/scripts/al-bin.sh`, eight `SKILL.md` files, two
agent definitions. Add a `Setup` hook that checks for the binaries and prints
the build command when missing. Run `claude plugin validate`.

Files: all new under `plugin/`. Add a `make plugin-validate` target to
`Makefile`.

Done when `claude --plugin-dir ./plugin` in the workspace copy answers
"which app defines codeunit 80" in one tool call.

**7. Report index progress and drop the fixed timeout (4 h)**

Add `sourceIndex: {state, filesDone, filesTotal}` to the `status` and `diag`
results. Make the client's 30 s timeout configurable through
`AL_REQUEST_TIMEOUT_MS` and a `--timeout-ms` flag, and have the timeout message
name the index state instead of suggesting a flag that does not exist.

Files: `crates/al-protocol/src/client.rs` (24),
`crates/al-lsp/src/server/daemon/mod.rs` (`status` arm, `dispatch_diag` at
800), `crates/al-workspace/src/lib.rs` (the source index task),
`crates/al-explorer/src/cli/args.rs`.

**8. Compact JSON output mode (2 h)**

`--compact` on `al-explorer` switching `serde_json::to_string_pretty` to
`to_string`, and the MCP path serialising compact always. 43% off every
payload, measured on `by-id codeunit 80` (552,710 to 315,393 bytes).

Files: `crates/al-explorer/src/cli/commands/response_contract.rs`,
`crates/al-explorer/src/cli/args.rs`, `crates/al-lsp/src/server/mcp.rs`.

**9. Dependency `.app` version diff (10 h)**

New daemon method `packageDiff` taking two `.app` paths or two package
name/version pairs, returning removed objects, removed and obsoleted
procedures, changed signatures, and workspace subscribers pointing at events
that the newer package no longer publishes. Reuses the comparison in
`dispatch_breaking_changes`.

Files: `crates/al-lsp/src/server/daemon/build_dispatch/mod.rs` (288, 363),
`crates/al-explorer/src/cli/args.rs`,
`crates/al-explorer/src/cli/commands/lsp/reports.rs` (266, 428).

Done when it diffs Base Application 28.1 against 28.3 from this workspace's
`.alpackages`.

**10. Persist the symbol index (12 h)**

Cache the package symbol index and the dependency source index to
`~/.local/share/al-lsp/<project-hash>/`, keyed on the content hash of each
`.app`. Invalidate per package. Measure the memory before committing: the index
is 2.9 GB resident today, so this needs a decision on what is cached and what
is rebuilt.

Files: `crates/al-workspace/src/lib.rs`, `crates/al-symbols/src/`,
`crates/al-lsp/src/server/daemon/build_dispatch/symbols_auth.rs` (9).

Done when a second cold daemon start on the same `.alpackages` skips the 54 s
source index.

## Design complete
