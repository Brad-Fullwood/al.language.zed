# AI & MCP Server

**Module:** `crates/al-lsp/src/server/mcp.rs` · **Status:** ✅ shipped

AI integration is a first-class surface, not an afterthought. The extension registers a Zed context
server named **AL Tools** that launches `al-lsp mcp` from `PATH`. The MCP server speaks
newline-delimited JSON-RPC 2.0 over stdio (protocol version `2024-11-05`) and forwards every tool call
into the **same daemon dispatcher** the CLI uses — so MCP behavior and CLI behavior cannot drift.
The `al_call` tool accepts any dispatcher method and its parameter object, which means the complete
shared tool surface is available to MCP without a second allow-list. Named tools remain as
discoverable shortcuts for common agent workflows.

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
commands. See [Debugging (DAP) & Business Central Runtime](./debugging-dap.md#ai-agent-control-through-mcp)
for the complete command contract and the boundary between Zed's DAP launch flow and MCP control.

The tool names intentionally **mirror Microsoft's AL agent tool surface** where possible (`al_build`,
`al_downloadsymbols`, `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`), while adding analysis
tools the official surface does not expose (`al_deadcode`, `al_sqlscan`, `al_entrypoints`,
`al_trace_event`, `al_impact`). These names are conveniences; `al_call` exposes every other shared
operation without requiring another hand-written MCP registration.

## Microsoft comparison

Microsoft has an AL agent tool surface for its Copilot/agent integrations. This project matches the
familiar tool names so agents written against Microsoft's surface feel at home, then goes further by
exposing the project's unique analyses (dead code, SQL scan, entrypoints, event tracing, impact) as
agent tools — none of which are in the standard official surface. Because the tools dispatch through
the shared daemon, an agent gets exactly the same answers a developer gets from `al-explorer`.

## Why this approach

LLM agents are most useful when they can *ask precise questions about the codebase* and *act* on the
answers. Routing MCP through the same dispatcher as the CLI means: one implementation, stable answers,
and `al_call` makes every current and future dispatcher operation immediately available to agents.
The stdio + NDJSON-RPC transport works with Claude Code, Zed's agent panel, and any custom MCP client.

## How to use

1. Ensure `al-lsp` is on `PATH` (`make install`). The Zed extension's context server **requires** this
   (no binary auto-download at context-server scope) and returns an actionable error otherwise.
2. In Zed's agent panel, the **AL Tools** context server appears and its tools become callable.
3. Standalone: run `al-lsp mcp --project <path>` and connect any MCP client over stdio.

## Limitations & roadmap

- Methods reached through `al_call` use the shared daemon parameter contract rather than a dedicated
  per-method MCP schema; named aliases can still be added where richer discovery materially helps an
  agent, but they do not control availability.
- `al_debug` requires a reachable, authenticated BC runtime. It controls the attached native debug
  session, while compile-and-publish remains part of Zed's DAP launch flow.
- MCP itself is platform-independent stdio and runs on Linux, macOS, and Windows. Zed's context-server
  command currently resolves `al-lsp` from `PATH` on every platform, rather than using the extension's
  LSP/DAP download-resolution chain.
- `ROADMAP.md` (AI And MCP): add output-schema coverage, include routing details in `al_runtests`
  output so agents know which tests ran locally vs needed live BC, and add agent-oriented diagnostics
  for missing symbols/config/bridge/source.
