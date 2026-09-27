# MCP Tool Reference

The `al-tools` MCP server (`al-lsp mcp`) exposes these tools over newline-delimited JSON-RPC 2.0
(protocol `2025-11-25`, with `2024-11-05` accepted for compatibility). Each tool maps to a daemon
method and dispatches through the shared daemon dispatcher. [ai-mcp](../features/ai-mcp.md) covers
behavior and design.

`al_call` reaches every daemon method, including one added to the dispatcher later. The other tools
are named aliases for common agent workflows and do not limit what `al_call` can reach.

| Tool | Internal method | Parameters | Returns |
| --- | --- | --- | --- |
| `al_call` | selected by `method` | `method: string` (required), `params: object = {}` | the selected daemon method's result |
| `al_debug` | `debug` | `cmd: start\|breakpoint\|state\|stack\|variables\|globals\|expand\|eval\|continue\|step\|history\|stop` plus command-specific fields | persistent debug-session control and inspection result |
| `al_build` | `compile` | _(none)_ | success of the verified native build (or the configured `alc`), structured diagnostics, `.app` path |
| `al_downloadsymbols` | `downloadSymbols` | `source: nuget \| server = nuget`, `config: string` (launch configuration for `source=server`, first one when omitted) | downloaded package list |
| `al_symbolsearch` | `search` | `query: string` (required), `limit: number = 50`, `summary: boolean = true` (false includes every member) | matching objects with kind, id, name and package |
| `al_getdiagnostics` | `lint` | `file: string` or `uri: string`, plus `text: string` for a file outside the project | diagnostics for the file |
| `al_runtests` | `tests.run_auto` | _(none)_ | test results plus per-method classified/actual backend, local/live status, and reasons. Unsupported/platform behavior needs live BC |
| `al_deadcode` | `deadCode` | _(none)_ | unused procedures/fields/orphaned subscribers |
| `al_sqlscan` | `sqlPatterns` | _(none)_ | SQL anti-pattern findings |
| `al_entrypoints` | `entrypoints` | `scope: workspace \| packages \| all = workspace` | procedures with no incoming calls |
| `al_trace_event` | `trace` | `event: string` (required), `depth: number = 10` (max 50) | event propagation chain |
| `al_impact` | `impact` | `symbol: string` (required), `scope: workspace \| packages \| all = workspace` | consumers of the symbol. A name that is not loaded is an error naming the closest ones |
| `al_suggestevent` | `suggestEvent` | `query: object` (required) | suggested integration events and paths |
| `al_testclassify` | `tests.classify` | _(none)_ | per-test execution routing and reasons |
| `al_testcoverage` | `tests.coverage` | _(none)_ | qualified/transitive static coverage plus explicit unresolved overload targets |
| `al_testsnapshot` | `tests.snapshot_capture` | `codeunitId`, `codeunitName`, `methodName`, `bcVersion`, `breakpoints`, `outputPath`. Optional `config`, `timeoutMs` | live BC breakpoint-variable capture for one exact test method |
| `al_testsnapshotreplay` | `tests.snapshot_replay` | `snapshotPath`, `bcVersion`. Optional `config`, `timeoutMs` | re-run the recorded method on live BC and return field-level divergences |
| `al_depgraph` | `deps.graph` | `format: json \| dot = json` | GUID-keyed direct/transitive package graph with missing/version-conflict reporting |
| `al_freeids` | `freeIds` | `kind: object-kind keyword` (omit for a per-kind summary), `object: string` (table/tableextension/enum/enumextension, wins over `kind`), `count: number = 1` (max 100), `includeUsed: boolean = false` | next free object ID, table field number or enum ordinal inside the `app.json` idRanges, with per-range used/free counts |

The complete method names accepted by `al_call`, grouped by capability, are in the
[daemon method reference](./daemon-methods.md). Its `params` object is passed unchanged to the same
dispatcher used by `al-explorer` and checkout-local contributor tasks.

## Result size

A tool whose method returns a list also accepts `limit`, `offset` and `fields`, and answers with
`{ items, total, returned, offset, truncated }`, or with those counters beside the list when the
result is an object around one array. `total` counts the rows before the window and `truncated`
says whether more follow, so a caller can tell a full page from a complete answer. `fields` keeps
only the named keys on each row.

An MCP call that passes no `limit` gets 50, because a tool result goes straight into a context
window: `al_symbolsearch` on a common word, `al_entrypoints` on a project with Base Application, or
`al_impact` on a base table each return tens of thousands of rows otherwise. An explicit `limit`
always wins, including `limit: 0` for a count alone.

`al_impact` and `al_entrypoints`, and `impact`, `tableImpact`, `entrypoints`, `eventMap` and
`graphExport` through `al_call`, also accept `scope` as `workspace`, `packages` or `all`, and
report `scope` and `outOfScopeCount`. An MCP call that passes no `scope` gets `workspace`, the code
the open project can change.

Each tool's `inputSchema` carries whichever of these its method takes. The schema is derived from
the method, so it lists the arguments the method accepts. The full semantics are in the
[daemon method reference](./daemon-methods.md#projection-limit-offset-fields).

A `uri` or `file` outside the loaded project is refused with `-32002`, for MCP as for every other
caller, and nothing here reads a file on the caller's behalf. A read-only single-file method can
be given the source as `text` instead, which the caller already has. A method that rewrites the
file it names takes no `text` at all. See
[paths and the project boundary](./daemon-methods.md#paths-and-the-project-boundary).

## Protocol surface

`initialize` → `{ protocolVersion, capabilities: { tools }, serverInfo }`. `ping` → `{}`.
`tools/list` → tool definitions (name, description, `inputSchema`, result-specific `outputSchema`).
`tools/call` → `{ content, structuredContent, isError }`. `structuredContent` preserves the daemon
JSON and may add agent diagnostics or blocked-test routing context.

`tools/call` runs concurrently with the reader loop, so `ping` and the other lifecycle methods stay
responsive during a long call, and `notifications/cancelled` aborts the matching call by
`requestId`. Work already handed to an external process, such as a build or a live BC test run,
still finishes. Requests without an `id` are notifications and are not answered. `"id": null` is
answered as a request. Input lines are capped at 64 MB during read, and arguments are validated
against the published `inputSchema`, `minItems` included.

## Notes

- `al_debug` retains its session inside the MCP server workspace across calls. See
  [debugging-dap](../features/debugging-dap.md#mcp-debug-control) for each command and
  parameter. The session lasts as long as the MCP process.
- Tool names mirror Microsoft's AL agent surface where possible (`al_build`, `al_downloadsymbols`,
  `al_symbolsearch`, `al_getdiagnostics`, `al_runtests`). The rest are project-specific analyses.
- MCP is platform-independent stdio on Linux, macOS, and Windows. The Zed context server reuses an
  `al-lsp` path already cached by LSP/DAP or uses the shared GitHub release download path. Its
  `Project` callback has no `Worktree` handle, so it cannot look up `PATH` in the worktree. It does
  read `al.dotnetPath` from the `context_servers.al-tools.settings` block, so that setting reaches
  MCP before any LSP session has started.
- Dedicated aliases may be added for discovery and richer schemas, but every daemon operation is
  already callable through `al_call`, including XLIFF and code actions. Low-level package inspection
  remains a library API rather than a daemon method.
- Agent diagnostics use stable codes for missing package symbols, missing live-BC configuration,
  unavailable semantic-bridge enrichment, and package navigation without original source. Each
  diagnostic includes a reason and the steps that recover from it.
