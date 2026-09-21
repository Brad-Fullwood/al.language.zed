# Daemon Method Reference

JSON-RPC methods handled by `server/daemon/dispatch_request`. The daemon transport, CLI, and
checkout-local contributor tasks route here; MCP calls the same dispatcher in-process. Every method
below is available through MCP's `al_call`, whether or not it also has a named MCP alias. Transport and lifecycle:
[daemon-protocol](../features/daemon-protocol.md).

## LSP-style (`lsp_dispatch.rs`)

`hover`, `definition`, `references`, `implementations`, `completions`, `signatureHelp`, `rename`,
`documentSymbols`, `foldingRanges`, `semanticTokens`, `inlayHints`, `codeActions`, `search`, `object`,
`byId`, `events`, `subscribers`, `composed`, `packages`, `deps`.

## Build, codegen, fixes, symbols/auth (`build_dispatch/`)

`lint`, `format`, `fix`, `fix.applicationArea`, `fix.tooltips`, `fix.dataClassification`, `rules`,
`parse`, `metrics`, `sqlPatterns`, `sortMembers`, `organizeFiles`, `source`, `eventSource`,
`location`, `permissions`, `compile`, `package`, `publish`, `newProject`, `errorCodes`, `builtinTypes`, `setup`,
`clearCache`, `authenticate`, `downloadSymbols`, `snapshot`, `profiling`, `generate`, `obsolete`,
`audit.dataClassification`, `permissions.audit`, `deps.graph`, `breaking`, `arch.lint`, `duplicates`,
`upgrade`, `profiler.hints`, `nativeCheck`, `freeIds`, `diag`.

XLIFF: `xlf.generate`, `xlf.refresh`, `xlf.untranslated`, `xlf.suggest`.

`publish` compiles the project and uploads it to the Business Central dev endpoint named by a
launch configuration in the project. Params: `config` (launch configuration name, optional),
`incremental` (boolean, default false). The target server comes only from the project's own
launch configuration.

`freeIds` allocates inside the `idRanges` declared in `app.json`. Params: `kind` (object kind
keyword, omit for a per-kind summary), `object` (a table, tableextension, enum or enumextension
whose next free field number or enum ordinal is wanted; wins over `kind`, which then disambiguates
the name), `count` (1 to 100, default 1) and `includeUsed` (default false). Used numbers come from
every object declared in the workspace, including the second and later objects in a multi-object
file, plus the package objects that sit inside a declared range. A tableextension's fields must fall
inside `idRanges` and avoid the base table and every other extension of it that is visible; an
enumextension's ordinals work the same way. An exhausted range is an `INVALID_PARAMS` error naming
the range, and an `app.json` without `idRanges` returns a `warnings` entry.

`compile` is native by default. A native response includes `backend: "native"`, `validated: true`,
`verificationLevel: "native-syntax-project-binding-symbol-graph"`, `appPath` (or `null` on
rejection), and diagnostics with 1-based `line`/`column` plus exact native `endLine`/`endColumn`.
Workspace semantic/call-graph errors gate emission; warnings are returned with a successful build.
Set `al.useOfficialCompiler: true` to select the explicit Microsoft `alc` backend; there is no
silent fallback from native to Microsoft tooling.

Tests: `tests.discover`, `tests.run`, `tests.coverage`, `tests.run_batch`, `tests.run_auto`,
`tests.last_results`, `tests.affected`, `tests.classify`, `tests.snapshot_validate`,
`tests.snapshot_capture`, `tests.snapshot_replay`, `tests.snapshot_diff`, `tests.mutate`.

## Insight (`insight_dispatch.rs`)

`trace`, `traceChain`, `entrypoints`, `graphExport`, `insightStats`, `deadCode`, `impact`,
`tableImpact`, `suggestEvent`, `eventMap`.

## Debug (`debug_dispatch.rs`)

`debug` with `params.cmd` ∈ { `start`, `breakpoint`, `state`, `stack`, `variables`, `globals`,
`expand`, `eval`, `continue`, `step`, `history`, `stop` }. Inspection/evaluation commands accept
`frameId`; `expand` also requires `path`; `step` accepts `stepType: over|in|out`. This stateful method
backs both the CLI debug commands and the MCP `al_debug` tool; the process must remain alive between
calls.

## Conventions & limits

- Lifecycle methods: `ping` returns an empty object, `status` returns daemon/workspace state, and
  `shutdown` requests an orderly daemon stop.
- JSON-RPC 2.0 over newline-delimited frames; error codes include standard set plus `-32000`
  (code analysis) and `-32001` (file not found). `null` results are serialized explicitly.
- `duplicates` clamps `minTokens` and `minSimilarity`; `graphExport` is capped at
  50k nodes+edges; `trace`/`traceChain` depth bounded; 64 MB max request line; ≤64 concurrent
  connections; 30-minute idle shutdown (skipped during an active debug session).
- Local-only IPC: Unix-domain socket at `$XDG_RUNTIME_DIR/al-lsp/{hash}.sock` (with platform
  runtime-directory fallbacks) on Linux/macOS; per-user named pipe on Windows.

Common parameter shapes: position queries accept `uri` plus `{line, character}`; `breaking` and
`upgrade` accept `baselineSymbols`; `tests.snapshot_validate` accepts `snapshotPath`;
`tests.snapshot_replay` accepts `snapshotPath`, `bcVersion`, and optional `config`/`timeoutMs`; and
`tests.snapshot_diff` accepts `pathA` and `pathB`. Snapshot paths must resolve inside the current
project. `source` requires `name` and accepts the
disambiguators `kind`, `package`, `proc`, or `trigger` (`proc` and `trigger` are mutually exclusive);
it returns `source_availability` as `workspace_source`, `embedded_source`, `generated_outline`, or
`metadata_only`. `location` accepts the same object identity selectors (`name`, `kind`, `package`,
and `id`) and rejects ambiguous matches. Other method shapes are defined beside their dispatcher and
mirrored by `al-explorer`; MCP passes the same object through `al_call`.

`deps.graph` rereads and validates the current `app.json`, reads dependency metadata from every
configured `.app` package, and fails explicitly on an unreadable/malformed manifest. Identity is the
app GUID; minimum versions are treated as compatible when a loaded version satisfies them.
