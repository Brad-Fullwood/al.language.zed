# MCP Server

**Module:** `crates/al-lsp/src/server/mcp.rs` · **Status:** ✅ shipped

The extension registers a Zed context server named **AL Tools** that launches `al-lsp mcp` from the
extension's resolved cache or downloads the current release when that cache is empty. An
explicit/PATH binary previously resolved by LSP or DAP is reused from the cache. The MCP server speaks
newline-delimited JSON-RPC 2.0 over stdio (protocol version `2025-11-25`, with `2024-11-05`
accepted for compatibility) and forwards every tool call
into the **same daemon dispatcher** the CLI uses — so MCP behavior and CLI behavior cannot drift.
The `al_call` tool accepts any dispatcher method and its parameter object, which means the complete
shared tool surface is available to MCP without a second allow-list. Named tools remain as
discoverable shortcuts for common workflows.

## Lifecycle

Standard MCP handshake: `initialize` (returns capabilities + serverInfo), `ping`, `tools/list`
(returns each tool's name, description, input schema, and result-specific output schema), and
`tools/call` (returns `{ content, structuredContent, isError }`). On a tool call, `mcp.rs` looks up the tool, selects either
the alias's mapped method or `al_call`'s requested method, builds a daemon `Request`, calls
`dispatch_request()`, and serializes the result.

## Tools

| MCP tool | Internal method | Parameters | What it does |
| --- | --- | --- | --- |
| `al_call` | selected at call time | `method` (string, required), `params` (object, default `{}`) | Call any method in the shared daemon catalog through one generic entry point. |
| `al_debug` | `debug` | `cmd` (required) plus command-specific debug parameters | Drive a persistent BC debug session: start, breakpoint, state, stack, locals/globals/expansion, evaluate, continue, step, history, and stop. |
| `al_build` | `compile` | — | Compile the project (native emitter by default); returns success, diagnostics, `.app` path. |
| `al_downloadsymbols` | `downloadSymbols` | — | Download dependency symbol packages into `.alpackages`. |
| `al_symbolsearch` | `search` | `query` (string), `limit` (number, default 20) | Fuzzy-search symbols across workspace and packages. |
| `al_getdiagnostics` | `lint` | `file` (path, required) | Run diagnostics for an AL file. |
| `al_runtests` | `tests.run_auto` | — | Discover and run tests; returns per-method classified/actual routing and reasons. Pure-logic and supported workspace-record tests run locally, while unsupported/platform-dependent tests need a launch config + live BC. |
| `al_deadcode` | `deadCode` | — | Find unused procedures, fields, and orphaned subscribers. |
| `al_sqlscan` | `sqlPatterns` | — | Detect SQL anti-patterns (FindFirst/Get/CalcFields in loops, unfiltered FindSet). |
| `al_entrypoints` | `entrypoints` | — | List procedures with no incoming calls. |
| `al_trace_event` | `trace` | `event` (string), `depth` (number, default 10) | Trace publisher→subscriber event propagation. |
| `al_impact` | `impact` | `symbol` (string, required, e.g. `Customer` or `Sales-Post.PostDocument`) | Answer "who consumes this symbol?" |
| `al_suggestevent` | `suggestEvent` | `query` (structured source/filter object) | Suggest integration events along an object, table, procedure, or event path. |
| `al_testclassify` | `tests.classify` | — | Explain where every test runs and why. |
| `al_testcoverage` | `tests.coverage` | — | Report qualified/transitive static call-graph coverage, including explicit unresolved overload targets. |
| `al_testsnapshot` | `tests.snapshot_capture` | — | Capture explicit breakpoint samples while one exact test method runs on live BC. |
| `al_testsnapshotreplay` | `tests.snapshot_replay` | — | Re-run a baseline snapshot's exact test method on live BC and return divergences. |
| `al_depgraph` | `deps.graph` | `format` (`json` or `dot`) | Return the GUID-keyed current-manifest/package dependency graph. |

`al_debug` is intentionally stateful: the MCP process retains the workspace's `NativeDebugSession`
between calls. This lets an agent execute a continuous debugging loop instead of launching isolated
commands. See [Debugging (DAP) & Business Central Runtime](./debugging-dap.md#mcp-debug-control)
for the complete command contract and the boundary between Zed's DAP launch flow and MCP control.

The tool names intentionally **mirror Microsoft's AL agent tool surface** where possible (`al_build`,
`al_downloadsymbols`, `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`), while adding analysis
tools the official surface does not expose (`al_deadcode`, `al_sqlscan`, `al_entrypoints`,
`al_trace_event`, `al_impact`). These names are conveniences; `al_call` exposes every other shared
operation without requiring another hand-written MCP registration.

Named tools publish result-specific output schemas. Successful and failed calls can also include
structured agent diagnostics with stable codes, reasons, and recovery actions for missing package
symbols, missing live-BC configuration, unavailable semantic-bridge enrichment, and package
navigation where the original AL source was not shipped. `al_call` intentionally retains a generic
result schema because it forwards heterogeneous methods across the complete daemon catalog.

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
  per-method MCP schema; named aliases can still be added where richer discovery materially helps an
  agent, but they do not control availability.
- `al_debug` requires a reachable, authenticated BC runtime. It controls the attached native debug
  session, while compile-and-publish remains part of Zed's DAP launch flow.
- MCP itself is platform-independent stdio and runs on Linux, macOS, and Windows. Zed's
  context-server callback exposes a `Project`, not a `Worktree`, so it cannot perform a fresh
  settings/PATH lookup; it reuses a path already cached by LSP/DAP or uses the shared GitHub release
  download path.
