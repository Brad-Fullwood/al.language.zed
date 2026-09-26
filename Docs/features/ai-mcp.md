# MCP Server

**Module:** `crates/al-lsp/src/server/mcp/`. **Status:** ✅ shipped

The extension registers a Zed context server named **AL Tools** that launches `al-lsp mcp` with the
binary LSP or DAP already resolved in this Zed session (which may be the one on `PATH`), or else the
latest release, downloaded or reused from disk. The MCP server speaks
newline-delimited JSON-RPC 2.0 over stdio (protocol version `2025-11-25`, with `2024-11-05`
accepted for compatibility) and forwards every tool call
into the **same daemon dispatcher** the CLI uses — so MCP behavior and CLI behavior cannot drift.
The `al_call` tool accepts any dispatcher method and its parameter object, which means the complete
shared tool surface is available to MCP without a second allow-list. Named tools remain as
discoverable shortcuts for common workflows.

## Lifecycle

Standard MCP handshake: `initialize` (returns capabilities + serverInfo), `ping`, `tools/list`
(returns each tool's name, description, input schema, and result-specific output schema), and
`tools/call` (returns `{ content, structuredContent, isError }`). On a tool call, `mcp/mod.rs` looks up the tool, selects either
the alias's mapped method or `al_call`'s requested method, builds a daemon `Request`, calls
`dispatch_request()`, and serializes the result.

### Concurrency and cancellation

`tools/call` is dispatched as its own task, so lifecycle traffic — `ping` above all — keeps being
answered while a long call (a live-BC `al_testsnapshot`, a full `al_build`) is still running. A
client can no longer mistake a busy server for a dead one. Responses are written under a single
stdout lock and may therefore arrive out of request order, which JSON-RPC allows.
`notifications/cancelled` aborts the matching in-flight `tools/call` by `requestId`. Two limits are
deliberate: cancellation aborts the MCP task but cannot roll back work already handed to an external
process (a running `alc` build or a live BC test run continues to completion), and everything other
than `tools/call` is still handled sequentially on the reader loop.

Message framing follows the daemon transport: one JSON object per line, with a 64 MB cap enforced
*while* reading so an oversized client line cannot force an unbounded allocation before parsing.
A message with no `id` is a notification and is never answered. `"id": null` is treated as a request
(and answered) on both the lifecycle and dispatch paths. Tool arguments are validated against the
published input schema, including `minItems`/`maxItems` on array arguments.

## Tools

| MCP tool | Internal method | Parameters | What it does |
| --- | --- | --- | --- |
| `al_call` | selected at call time | `method` (string, required), `params` (object, default `{}`) | Call any method in the shared daemon catalog through one generic entry point. |
| `al_debug` | `debug` | `cmd` (required) plus command-specific debug parameters | Drive a persistent BC debug session: start, breakpoint, state, stack, locals/globals/expansion, evaluate, continue, step, history, and stop. |
| `al_build` | `compile` | | Compile the project (native emitter by default). Returns success, diagnostics, `.app` path. |
| `al_downloadsymbols` | `downloadSymbols` | `source` (`nuget` or `server`, default `nuget`), `config` (launch configuration for `server`) | Download dependency symbol packages into `.alpackages`. |
| `al_symbolsearch` | `search` | `query` (string), `limit` (number, default 50 through MCP), `summary` (default true: name, kind, id and package only, false adds every member) | Fuzzy-search symbols across workspace and packages. |
| `al_getdiagnostics` | `lint` | `file` or `uri`, plus `text` for a file outside the project | Run diagnostics for an AL file. |
| `al_runtests` | `tests.run_auto` | | Discover and run tests. Returns per-method classified/actual routing and reasons. Pure-logic and supported workspace-record tests run locally, while unsupported/platform-dependent tests need a launch config + live BC. |
| `al_deadcode` | `deadCode` | | Find unused procedures, fields, and orphaned subscribers. |
| `al_sqlscan` | `sqlPatterns` | | Detect SQL anti-patterns (FindFirst/Get/CalcFields in loops, unfiltered FindSet). |
| `al_entrypoints` | `entrypoints` | `scope` (default `workspace`) | List procedures with no incoming calls. |
| `al_trace_event` | `trace` | `event` (string), `depth` (number, default 10) | Trace publisher→subscriber event propagation. |
| `al_impact` | `impact` | `symbol` (string, required, e.g. `Customer` or `Sales-Post.PostDocument`), `scope` (default `workspace`) | Answer "who consumes this symbol?" |
| `al_suggestevent` | `suggestEvent` | `query` (structured source/filter object) | Suggest integration events along an object, table, procedure, or event path. |
| `al_testclassify` | `tests.classify` | | Explain where every test runs and why. |
| `al_testcoverage` | `tests.coverage` | | Report qualified/transitive static call-graph coverage, including explicit unresolved overload targets. |
| `al_testsnapshot` | `tests.snapshot_capture` | `codeunitId`, `codeunitName`, `methodName`, `bcVersion`, `breakpoints` (`file`, `line`, optional `condition`), `outputPath`. Optional `config`, `timeoutMs` | Capture explicit breakpoint samples while one exact test method runs on live BC. |
| `al_testsnapshotreplay` | `tests.snapshot_replay` | `snapshotPath`, `bcVersion`. Optional `config`, `timeoutMs` | Re-run a baseline snapshot's exact test method on live BC and return divergences. |
| `al_depgraph` | `deps.graph` | `format` (`json` or `dot`) | Return the GUID-keyed current-manifest/package dependency graph. |
| `al_freeids` | `freeIds` | `kind` (object-kind keyword, omit for a per-kind summary), `object` (table/tableextension/enum/enumextension, wins over `kind`), `count` (1 to 100, default 1), `includeUsed` (default false) | Pick the next free object ID, table field number or enum value ordinal inside the `app.json` idRanges. |

`al_debug` is intentionally stateful: the MCP process retains the workspace's `NativeDebugSession`
between calls. This lets an agent execute a continuous debugging loop instead of launching isolated
commands. See [Debugging (DAP) & Business Central Runtime](./debugging-dap.md#mcp-debug-control)
for the complete command contract and the boundary between Zed's DAP launch flow and MCP control.

The tool names intentionally **mirror Microsoft's AL agent tool surface** where possible (`al_build`,
`al_downloadsymbols`, `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`), while adding analysis
tools the official surface does not expose (`al_deadcode`, `al_sqlscan`, `al_entrypoints`,
`al_trace_event`, `al_impact`). These names are conveniences. `al_call` exposes every other shared
operation without requiring another hand-written MCP registration.

Named tools publish result-specific output schemas. Successful and failed calls can also include
structured agent diagnostics with stable codes, reasons, and recovery actions for missing package
symbols, missing live-BC configuration, unavailable semantic-bridge enrichment, and package
navigation where the original AL source was not shipped. `al_call` intentionally retains a generic
result schema because it forwards heterogeneous methods across the complete daemon catalog.

Tool results are compact JSON. A list method called through MCP without `limit` returns 50 rows
with `total` and `truncated`, and `impact`, `tableImpact`, `entrypoints`, `eventMap` and
`graphExport` default to `scope: workspace`. `object` and `byId` default to `signatures: true`,
one line per member. The [MCP tool reference](../reference/mcp-tools.md#result-size) has the
details. A `search` sent through MCP, by `al_symbolsearch` or `al_call`,
returns summaries unless it passes `summary: false`: three Base Application results take about
500 bytes instead of about 210 KB.

## Microsoft comparison

Microsoft exposes a similar AL tool surface for Copilot and agent integrations. This project keeps
the familiar names for common operations and also exposes dead-code, SQL-pattern, entrypoint, event,
and impact analysis. All tools dispatch through the shared daemon used by `al-explorer`.

## Design rationale

Routing MCP through the same dispatcher as the CLI keeps one implementation for both surfaces.
`al_call` makes dispatcher operations available without a separate registration, while named tools
provide richer discovery for common operations. The stdio transport works with any MCP client.

## How to use

1. Install the extension. Zed resolves or downloads the same `al-lsp` binary for LSP, DAP, and MCP.
2. In Zed's agent panel, the **AL Tools** context server appears and its tools become callable.
3. Standalone: run `al-lsp mcp --project <path>` and connect any MCP client over stdio.

## Limitations

- Methods reached through `al_call` use the shared daemon parameter contract rather than a dedicated
  per-method MCP schema. Named aliases can still be added where richer discovery materially helps an
  agent, but they do not control availability.
- `al_debug` requires a reachable, authenticated BC runtime. It controls the attached native debug
  session, while compile-and-publish remains part of Zed's DAP launch flow.
- MCP itself is platform-independent stdio and runs on Linux, macOS, and Windows. Zed's
  context-server callback exposes a `Project`, not a `Worktree`, so it cannot perform a fresh
  settings/PATH lookup. It reuses a path already cached by LSP/DAP or uses the shared GitHub release
  download path.
