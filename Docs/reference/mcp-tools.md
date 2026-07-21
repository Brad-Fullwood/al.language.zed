# MCP Tool Reference

The `al-tools` MCP server (`al-lsp mcp`) exposes these tools over newline-delimited JSON-RPC 2.0
(protocol `2024-11-05`). Each tool maps to a daemon method and dispatches through the shared daemon
dispatcher. Behavior and rationale: [ai-mcp](../features/ai-mcp.md).

`al_call` exposes the complete daemon method catalog. The other tools are discoverable aliases for
common agent workflows, not an allow-list. Consequently, adding a dispatcher operation cannot leave
MCP without a way to invoke it.

| Tool | Internal method | Parameters | Returns |
| --- | --- | --- | --- |
| `al_call` | selected by `method` | `method: string` (required), `params: object = {}` | the selected daemon method's result |
| `al_debug` | `debug` | `cmd: start\|breakpoint\|state\|stack\|variables\|globals\|expand\|eval\|continue\|step\|history\|stop` plus command-specific fields | persistent debug-session control and inspection result |
| `al_build` | `compile` | _(none)_ | verified-native (or configured `alc`) success, structured diagnostics, `.app` path |
| `al_downloadsymbols` | `downloadSymbols` | _(none)_ | downloaded package list |
| `al_symbolsearch` | `search` | `query: string`, `limit: number = 20` | matching symbols |
| `al_getdiagnostics` | `lint` | `file: string` (required) | diagnostics for the file |
| `al_runtests` | `tests.run_auto` | _(none)_ | test results (pure-logic local; rest need live BC) |
| `al_deadcode` | `deadCode` | _(none)_ | unused procedures/fields/orphaned subscribers |
| `al_sqlscan` | `sqlPatterns` | _(none)_ | SQL anti-pattern findings |
| `al_entrypoints` | `entrypoints` | _(none)_ | procedures with no incoming calls |
| `al_trace_event` | `trace` | `event: string`, `depth: number = 10` | event propagation chain |
| `al_impact` | `impact` | `symbol: string` (required) | consumers of the symbol |
| `al_suggestevent` | `suggestEvent` | `query: object` (required) | suggested integration events and paths |
| `al_testclassify` | `tests.classify` | _(none)_ | per-test execution routing and reasons |
| `al_testcoverage` | `tests.coverage` | _(none)_ | static object/procedure coverage |
| `al_depgraph` | `deps.graph` | `format: json \| dot = json` | dependency graph |

The complete method names accepted by `al_call`, grouped by capability, are in the
[daemon method reference](./daemon-methods.md). Its `params` object is passed unchanged to the same
dispatcher used by `al-explorer` and Zed tasks.

## Protocol surface

`initialize` → `{ protocolVersion, capabilities: { tools }, serverInfo }`; `ping` → `{}`;
`tools/list` → tool definitions (name, description, inputSchema); `tools/call` →
`{ content: [{ type, text }], isError }`.

## Notes

- `al_debug` retains its session inside the MCP server workspace across calls. See
  [debugging-dap](../features/debugging-dap.md#ai-agent-control-through-mcp) for each command and
  parameter. The MCP process must remain running for the session to persist.
- Tool names mirror Microsoft's AL agent surface where possible (`al_build`, `al_downloadsymbols`,
  `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`); the rest are project-specific analyses.
- MCP is platform-independent stdio on Linux, macOS, and Windows. The Zed context server requires
  `al-lsp` on `PATH` (`make install`); it does not use the LSP/DAP auto-download path at this scope.
- Dedicated aliases may be added for discovery and richer schemas, but every daemon operation is
  already callable through `al_call`, including XLIFF and code actions. Low-level package inspection
  remains a library API rather than a daemon method.
