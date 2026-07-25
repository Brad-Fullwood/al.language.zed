# Daemon Method Reference

JSON-RPC methods handled by `server/daemon/dispatch_request`. The daemon transport, CLI, and Zed tasks
route here; MCP calls the same dispatcher in-process. Every method below is available through MCP's
`al_call`, whether or not it also has a named MCP alias. Transport and lifecycle:
[daemon-protocol](../features/daemon-protocol.md).

## LSP-style (`lsp_dispatch.rs`)

`hover`, `definition`, `references`, `implementations`, `completions`, `signatureHelp`, `rename`,
`documentSymbols`, `foldingRanges`, `semanticTokens`, `inlayHints`, `codeActions`, `search`, `object`,
`byId`, `events`, `subscribers`, `composed`, `packages`, `deps`.

## Build, codegen, fixes, symbols/auth (`build_dispatch/`)

`lint`, `format`, `fix`, `fix.applicationArea`, `fix.tooltips`, `fix.dataClassification`, `rules`,
`parse`, `metrics`, `sqlPatterns`, `sortMembers`, `organizeFiles`, `source`, `eventSource`,
`location`, `permissions`, `compile`, `package`, `newProject`, `errorCodes`, `builtinTypes`, `setup`,
`clearCache`, `authenticate`, `downloadSymbols`, `snapshot`, `profiling`, `generate`, `obsolete`,
`audit.dataClassification`, `permissions.audit`, `deps.graph`, `breaking`, `arch.lint`, `duplicates`,
`upgrade`, `profiler.hints`, `nativeCheck`, `diag`.

XLIFF: `xlf.generate`, `xlf.refresh`, `xlf.untranslated`, `xlf.suggest`.

`compile` is native by default. A native response includes `backend: "native"`, `validated: true`,
`verificationLevel: "native-syntax-project-binding-symbol-graph"`, `appPath` (or `null` on
rejection), and diagnostics with 1-based `line`/`column` plus exact native `endLine`/`endColumn`.
Workspace semantic/call-graph errors gate emission; warnings are returned with a successful build.
Set `al.useOfficialCompiler: true` to select the explicit Microsoft `alc` backend; there is no
silent fallback from native to Microsoft tooling.

Tests: `tests.discover`, `tests.run`, `tests.coverage`, `tests.run_batch`, `tests.run_auto`,
`tests.last_results`, `tests.affected`, `tests.classify`, `tests.snapshot_validate`,
`tests.snapshot_diff`, `tests.mutate`.

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
`upgrade` accept `baselineSymbols`; `tests.snapshot_validate` accepts `snapshotPath`; and
`tests.snapshot_diff` accepts `pathA` and `pathB`. `source` requires `name` and accepts the
disambiguators `kind`, `package`, `proc`, or `trigger` (`proc` and `trigger` are mutually exclusive);
it returns `source_availability` as `workspace_source`, `embedded_source`, `generated_outline`, or
`metadata_only`. `location` accepts the same object identity selectors (`name`, `kind`, `package`,
and `id`) and rejects ambiguous matches. Other method shapes are defined beside their dispatcher and
mirrored by `al-explorer`; MCP passes the same object through `al_call`.
