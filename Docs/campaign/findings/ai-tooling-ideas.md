# AI tooling: making Claude Code fast and accurate on Business Central

Workstream goal: an agent working on a BC extension should get answers from the
symbol index, call and event graphs and impact analysis in one tool call,
instead of decompiling `.app` files and grepping.

Status: in progress. Sections fill as the survey runs.

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

The complete match arm list is `crates/al-lsp/src/server/daemon/mod.rs:589-780`.
119 methods. Implementations live in four dispatch modules.

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

_pending_

## 3. Gaps

_pending_

## 4. Plugin design

_pending_

## 5. New ideas, ranked by payoff per effort

_pending_

## 6. Build list

_pending_
