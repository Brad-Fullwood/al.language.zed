# AI & MCP Server

**Module:** `crates/al-core/src/server/mcp.rs` · **Status:** ✅ shipped

AI integration is a first-class surface, not an afterthought. The extension registers a Zed context
server named **AL Tools** that launches `al-lsp mcp` from `PATH`. The MCP server speaks
newline-delimited JSON-RPC 2.0 over stdio (protocol version `2024-11-05`) and forwards every tool call
into the **same daemon dispatcher** the CLI uses — so MCP behavior and CLI behavior cannot drift.

## Lifecycle

Standard MCP handshake: `initialize` (returns capabilities + serverInfo), `ping`, `tools/list`
(returns each tool's name, description, and input schema), and `tools/call` (returns
`{ content: [{ type, text }], isError }`). On a tool call, `mcp.rs` looks up the tool, builds a daemon
`Request` with the mapped internal method name, calls `dispatch_request()`, and serializes the result.

## Tools

| MCP tool | Internal method | Parameters | What it does |
| --- | --- | --- | --- |
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

The tool names intentionally **mirror Microsoft's AL agent tool surface** where possible (`al_build`,
`al_downloadsymbols`, `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`), while adding analysis
tools the official surface does not expose (`al_deadcode`, `al_sqlscan`, `al_entrypoints`,
`al_trace_event`, `al_impact`).

## Microsoft comparison

Microsoft has an AL agent tool surface for its Copilot/agent integrations. This project matches the
familiar tool names so agents written against Microsoft's surface feel at home, then goes further by
exposing the project's unique analyses (dead code, SQL scan, entrypoints, event tracing, impact) as
agent tools — none of which are in the standard official surface. Because the tools dispatch through
the shared daemon, an agent gets exactly the same answers a developer gets from `al-explorer`.

## Why this approach

LLM agents are most useful when they can *ask precise questions about the codebase* and *act* on the
answers. Routing MCP through the same dispatcher as the CLI means: one implementation, stable answers,
and every analysis the project can do is one schema entry away from being an agent capability. The
stdio + NDJSON-RPC transport works with Claude Code, Zed's agent panel, and any custom MCP client.

## How to use

1. Ensure `al-lsp` is on `PATH` (`make install`). The Zed extension's context server **requires** this
   (no binary auto-download at context-server scope) and returns an actionable error otherwise.
2. In Zed's agent panel, the **AL Tools** context server appears and its tools become callable.
3. Standalone: run `al-lsp mcp --project <path>` and connect any MCP client over stdio.

## Limitations & roadmap

- The tool set is a curated subset; several powerful analyses are CLI-only today.
- On Windows the MCP context server is not shipped on the released extension API.
- `ROADMAP.md` (AI And MCP): add tools for `suggest-event`, `test-classify`, `test-coverage`, XLIFF,
  package/dependency inspection, and code-action suggestions; add schema tests for every tool I/O;
  include routing details in `al_runtests` output so agents know which tests ran locally vs needed
  live BC; and add agent-oriented diagnostics for missing symbols/config/bridge/source.
