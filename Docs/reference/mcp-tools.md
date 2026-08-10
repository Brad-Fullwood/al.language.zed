# MCP Tool Reference

The `al-tools` MCP server (`al-lsp mcp`) exposes these tools over newline-delimited JSON-RPC 2.0
(protocol `2025-11-25`, with `2024-11-05` accepted for compatibility). Each tool maps to a daemon method and dispatches through the shared daemon
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
| `al_runtests` | `tests.run_auto` | _(none)_ | test results plus per-method classified/actual backend, local/live status, and reasons; unsupported/platform behavior needs live BC |
| `al_deadcode` | `deadCode` | _(none)_ | unused procedures/fields/orphaned subscribers |
| `al_sqlscan` | `sqlPatterns` | _(none)_ | SQL anti-pattern findings |
| `al_entrypoints` | `entrypoints` | _(none)_ | procedures with no incoming calls |
| `al_trace_event` | `trace` | `event: string`, `depth: number = 10` | event propagation chain |
| `al_impact` | `impact` | `symbol: string` (required) | consumers of the symbol |
| `al_suggestevent` | `suggestEvent` | `query: object` (required) | suggested integration events and paths |
| `al_testclassify` | `tests.classify` | _(none)_ | per-test execution routing and reasons |
| `al_testcoverage` | `tests.coverage` | _(none)_ | qualified/transitive static coverage plus explicit unresolved overload targets |
| `al_testsnapshot` | `tests.snapshot_capture` | `codeunitId`, `codeunitName`, `methodName`, `bcVersion`, `breakpoints`, `outputPath`; optional `config`, `timeoutMs` | live BC breakpoint-variable capture for one exact test method |
| `al_testsnapshotreplay` | `tests.snapshot_replay` | `snapshotPath`, `bcVersion`; optional `config`, `timeoutMs` | re-run the recorded method on live BC and return field-level divergences |
| `al_depgraph` | `deps.graph` | `format: json \| dot = json` | GUID-keyed direct/transitive package graph with missing/version-conflict reporting |

The complete method names accepted by `al_call`, grouped by capability, are in the
[daemon method reference](./daemon-methods.md). Its `params` object is passed unchanged to the same
dispatcher used by `al-explorer` and checkout-local contributor tasks.

## Protocol surface

`initialize` → `{ protocolVersion, capabilities: { tools }, serverInfo }`; `ping` → `{}`;
`tools/list` → tool definitions (name, description, `inputSchema`, result-specific `outputSchema`);
`tools/call` → `{ content, structuredContent, isError }`. `structuredContent` preserves the daemon
JSON and may add agent diagnostics or blocked-test routing context.

`tools/call` runs concurrently with the reader loop, so `ping` and the other lifecycle methods stay
responsive during a long call, and `notifications/cancelled` aborts the matching call by
`requestId` (in-flight work already delegated to an external process — a build, a live BC test run —
still finishes). Requests without an `id` are notifications and are never answered; `"id": null` is
answered as a request. Input lines are capped at 64 MB during read, and arguments are validated
against the published `inputSchema`, `minItems` included.

## Notes

- `al_debug` retains its session inside the MCP server workspace across calls. See
  [debugging-dap](../features/debugging-dap.md#mcp-debug-control) for each command and
  parameter. The MCP process must remain running for the session to persist.
- Tool names mirror Microsoft's AL agent surface where possible (`al_build`, `al_downloadsymbols`,
  `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`); the rest are project-specific analyses.
- MCP is platform-independent stdio on Linux, macOS, and Windows. The Zed context server reuses an
  `al-lsp` path already cached by LSP/DAP or uses the shared GitHub release download path. Its
  `Project` callback cannot perform a fresh worktree `PATH` lookup (no `Worktree` handle), but it
  CAN read `al.dotnetPath` directly from the `context_servers.al-tools.settings` block, so that
  setting still reaches MCP even when no LSP session has started yet.
- Dedicated aliases may be added for discovery and richer schemas, but every daemon operation is
  already callable through `al_call`, including XLIFF and code actions. Low-level package inspection
  remains a library API rather than a daemon method.
- Agent diagnostics use stable codes for missing package symbols, missing live-BC configuration,
  unavailable semantic-bridge enrichment, and package navigation without original source. Each
  diagnostic includes a reason and concrete recovery actions.
