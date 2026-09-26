# MCP Server

**Module:** `crates/al-lsp/src/server/mcp/`. **Status:** ✅ shipped

The extension registers a Zed context server named **AL Tools** that launches `al-lsp mcp` with the
binary LSP or DAP already resolved in this Zed session (which may be the one on `PATH`), or else the
latest release, downloaded or reused from disk. The MCP server speaks newline-delimited JSON-RPC
2.0 over stdio (protocol version `2025-11-25`, with `2024-11-05` accepted for compatibility) and
forwards every tool call into the **same daemon dispatcher** the CLI uses, so MCP and the CLI give
the same answers. The `al_call` tool accepts any dispatcher method and its parameter object, so
every daemon method is available to MCP. The named tools are shortcuts for common workflows.

## Lifecycle

Standard MCP handshake: `initialize` (returns capabilities + serverInfo), `ping`, `tools/list`
(returns each tool's name, description, input schema, and result-specific output schema), and
`tools/call` (returns `{ content, structuredContent, isError }`). On a tool call, `mcp/mod.rs`
looks up the tool, selects either the alias's mapped method or `al_call`'s requested method, builds
a daemon `Request`, calls `dispatch_request()`, and serializes the result.

### Concurrency and cancellation

`tools/call` is dispatched as its own task, so lifecycle traffic, `ping` above all, is still
answered while a long call (a live-BC `al_testsnapshot`, a full `al_build`) runs, and a client can
tell a busy server from a dead one. Responses are written under a single stdout lock and may
therefore arrive out of request order, which JSON-RPC allows.
`notifications/cancelled` aborts the matching in-flight `tools/call` by `requestId`. Cancellation
aborts the MCP task but does not roll back work already handed to an external process: a running
`alc` build or a live BC test run continues to completion. Everything other than `tools/call` is
handled in order on the reader loop.

Message framing follows the daemon transport: one JSON object per line, with a 64 MB cap enforced
*while* reading so an oversized client line cannot force an unbounded allocation before parsing.
A message with no `id` is a notification and is not answered. `"id": null` is treated as a request
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

`al_debug` keeps state: the MCP process holds the workspace's `NativeDebugSession` between calls,
so an agent can work through one debugging session across many calls. See
[Debugging (DAP) & Business Central Runtime](./debugging-dap.md#mcp-debug-control) for every command
and for which steps belong to Zed's DAP launch flow and which to MCP.

The tool names match **Microsoft's AL agent tools** where possible (`al_build`,
`al_downloadsymbols`, `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`), and add analysis tools
Microsoft's set does not have (`al_deadcode`, `al_sqlscan`, `al_entrypoints`, `al_trace_event`,
`al_impact`). `al_call` reaches every other shared operation without an MCP registration of its
own.

Named tools publish result-specific output schemas. Successful and failed calls can also include
structured agent diagnostics with stable codes, reasons, and recovery actions for missing package
symbols, missing live-BC configuration, unavailable semantic-bridge enrichment, and package
navigation where the original AL source was not shipped. `al_call` has a generic result schema
because it forwards every method in the daemon catalog, each with its own result shape.

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

## Design

Routing MCP through the same dispatcher as the CLI keeps one implementation for both. `al_call`
makes dispatcher operations available without a separate registration, and the named tools, with
their own schemas, make common operations easier for an agent to find. The stdio transport works
with any MCP client.

## How to use

1. Install the extension. Zed resolves or downloads the same `al-lsp` binary for LSP, DAP, and MCP.
2. In Zed's agent panel, the **AL Tools** context server appears and its tools become callable.
3. Standalone: run `al-lsp mcp --project <path>` and connect any MCP client over stdio.

## Limitations

- Methods reached through `al_call` take the daemon's own parameters and have no MCP schema of
  their own. A named alias can be added where a schema helps an agent find an operation. Aliases do
  not decide which methods are available.
- `al_debug` requires a reachable, authenticated BC runtime. It controls the attached native debug
  session, while compile-and-publish remains part of Zed's DAP launch flow.
- MCP runs over stdio on Linux, macOS, and Windows. Zed's context-server callback gets a
  `Project` and no `Worktree`, so it cannot look up worktree settings or `PATH`. It reuses a path
  already cached by LSP/DAP or uses the shared GitHub release download path.
