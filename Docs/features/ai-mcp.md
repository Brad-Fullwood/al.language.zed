# MCP Server

**Module:** `crates/al-lsp/src/server/mcp.rs` · **Status:** ✅ shipped

The extension registers a Zed context server named **AL Tools** that launches `al-lsp mcp` from
`PATH`. The MCP server speaks
newline-delimited JSON-RPC 2.0 over stdio (protocol version `2024-11-05`) and forwards every tool call
into the **same daemon dispatcher** the CLI uses — so MCP behavior and CLI behavior cannot drift.
The `al_call` tool accepts any dispatcher method and its parameter object, which means the complete
shared tool surface is available to MCP without a second allow-list. Named tools remain as
discoverable shortcuts for common workflows.

## Lifecycle

Standard MCP handshake: `initialize` (returns capabilities + serverInfo), `ping`, `tools/list`
(returns each tool's name, description, and input schema), and `tools/call` (returns
`{ content: [{ type, text }], isError }`). On a tool call, `mcp.rs` looks up the tool, selects either
the alias's mapped method or `al_call`'s requested method, builds a daemon `Request`, calls
`dispatch_request()`, and serializes the result.

## Tools

| MCP tool | Internal method | Parameters | What it does |
| --- | --- | --- | --- |
| `al_call` | selected at call time | `method` (string, required), `params` (object, default `{}`) | Call any method in the shared daemon catalog; this is the complete, zero-drift bridge. |
| `al_debug` | `debug` | `cmd` (required) plus command-specific debug parameters | Drive a persistent BC debug session: start, breakpoint, state, stack, locals/globals/expansion, evaluate, continue, step, history, and stop. |
| `al_build` | `compile` | — | Compile the project (native emitter by default); returns success, diagnostics, `.app` path. |
| `al_downloadsymbols` | `downloadSymbols` | — | Download dependency symbol packages into `.alpackages`. |
| `al_symbolsearch` | `search` | `query` (string), `limit` (number, default 20) | Fuzzy-search symbols across workspace and packages. |
| `al_getdiagnostics` | `lint` | `file` (path, required) | Run diagnostics for an AL file. |
| `al_runtests` | `tests.run_auto` | — | Discover and run tests; pure-logic tests run on the built-in interpreter, the rest need a launch config + live BC. |
| `al_deadcode` | `deadCode` | — | Find unused procedures, fields, and orphaned subscribers. |
| `al_sqlscan` | `sqlPatterns` | — | Detect SQL anti-patterns (FindFirst/Get/CalcFields in loops, unfiltered FindSet). |
| `al_entrypoints` | `entrypoints` | — | List procedures with no incoming calls. |
| `al_trace_event` | `trace` | `event` (string), `depth` (number, default 10) | Trace publisher→subscriber event propagation. |
| `al_impact` | `impact` | `symbol` (string, required, e.g. `Customer` or `Sales-Post.PostDocument`) | Answer "who consumes this symbol?" |
| `al_suggestevent` | `suggestEvent` | `query` (structured source/filter object) | Suggest integration events along an object, table, procedure, or event path. |
| `al_testclassify` | `tests.classify` | — | Explain where every test runs and why. |
| `al_testcoverage` | `tests.coverage` | — | Report static test coverage from the call graph. |
| `al_depgraph` | `deps.graph` | `format` (`json` or `dot`) | Return the application dependency graph. |

`al_debug` is intentionally stateful: the MCP process retains the workspace's `NativeDebugSession`
between calls. This lets an agent execute a continuous debugging loop instead of launching isolated
commands. See [Debugging (DAP) & Business Central Runtime](./debugging-dap.md#mcp-debug-control)
for the complete command contract and the boundary between Zed's DAP launch flow and MCP control.

The tool names intentionally **mirror Microsoft's AL agent tool surface** where possible (`al_build`,
`al_downloadsymbols`, `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`), while adding analysis
tools the official surface does not expose (`al_deadcode`, `al_sqlscan`, `al_entrypoints`,
`al_trace_event`, `al_impact`). These names are conveniences; `al_call` exposes every other shared
operation without requiring another hand-written MCP registration.

## Microsoft comparison

Microsoft exposes a similar AL tool surface for Copilot and agent integrations. This project keeps
the familiar names for common operations and also exposes dead-code, SQL-pattern, entrypoint, event,
and impact analysis. All tools dispatch through the shared daemon used by `al-explorer`.

## Design rationale

Routing MCP through the same dispatcher as the CLI keeps one implementation for both surfaces.
`al_call` makes dispatcher operations available without a separate registration, while named tools
provide richer discovery for common operations. The stdio transport works with any MCP client.

## How to use

1. Ensure `al-lsp` is on `PATH` (`make install`). The Zed extension's context server **requires** this
   (no binary auto-download at context-server scope) and returns an actionable error otherwise.
2. In Zed's agent panel, the **AL Tools** context server appears and its tools become callable.
3. Standalone: run `al-lsp mcp --project <path>` and connect any MCP client over stdio.

## Limitations

- Methods reached through `al_call` use the shared daemon parameter contract rather than a dedicated
  per-method MCP schema; named aliases can still be added where richer discovery materially helps an
  agent, but they do not control availability.
- `al_debug` requires a reachable, authenticated BC runtime. It controls the attached native debug
  session, while compile-and-publish remains part of Zed's DAP launch flow.
- MCP itself is platform-independent stdio and runs on Linux, macOS, and Windows. Zed's context-server
  command currently resolves `al-lsp` from `PATH` on every platform, rather than using the extension's
  LSP/DAP download-resolution chain.
