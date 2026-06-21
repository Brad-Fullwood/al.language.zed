# MCP Tool Reference

The `al-tools` MCP server (`al-lsp mcp`) exposes these tools over newline-delimited JSON-RPC 2.0
(protocol `2024-11-05`). Each tool maps to a daemon method and dispatches through the shared daemon
dispatcher. Behavior and rationale: [ai-mcp](../features/ai-mcp.md).

| Tool | Internal method | Parameters | Returns |
| --- | --- | --- | --- |
| `al_build` | `compile` | _(none)_ | success, diagnostics, `.app` path |
| `al_downloadsymbols` | `downloadSymbols` | _(none)_ | downloaded package list |
| `al_symbolsearch` | `search` | `query: string`, `limit: number = 20` | matching symbols |
| `al_getdiagnostics` | `lint` | `file: string` (required) | diagnostics for the file |
| `al_runtests` | `tests.run_auto` | _(none)_ | test results (pure-logic local; rest need live BC) |
| `al_deadcode` | `deadCode` | _(none)_ | unused procedures/fields/orphaned subscribers |
| `al_sqlscan` | `sqlPatterns` | _(none)_ | SQL anti-pattern findings |
| `al_entrypoints` | `entrypoints` | _(none)_ | procedures with no incoming calls |
| `al_trace_event` | `trace` | `event: string`, `depth: number = 10` | event propagation chain |
| `al_impact` | `impact` | `symbol: string` (required) | consumers of the symbol |

## Protocol surface

`initialize` → `{ protocolVersion, capabilities: { tools }, serverInfo }`; `ping` → `{}`;
`tools/list` → tool definitions (name, description, inputSchema); `tools/call` →
`{ content: [{ type, text }], isError }`.

## Notes

- Tool names mirror Microsoft's AL agent surface where possible (`al_build`, `al_downloadsymbols`,
  `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`); the rest are project-specific analyses.
- The Zed context server requires `al-lsp` on `PATH` (`make install`); no auto-download at this scope.
- ⛔ Not shipped on the Windows released extension API.
- Planned tools (roadmap): suggest-event, test-classify, test-coverage, XLIFF, package/dependency
  inspection, code-action suggestions.
