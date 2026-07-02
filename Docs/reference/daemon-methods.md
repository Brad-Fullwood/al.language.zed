# Daemon Method Reference

JSON-RPC methods dispatched by `al-lsp daemon` (`server/daemon/dispatch_request`). The CLI, MCP, and
Zed tasks all route here. Transport and lifecycle: [daemon-protocol](../features/daemon-protocol.md).

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
`upgrade`, `profiler.hints`.

XLIFF: `xlf.generate`, `xlf.refresh`, `xlf.untranslated`, `xlf.suggest`.

Tests: `tests.discover`, `tests.run`, `tests.coverage`, `tests.run_batch`, `tests.run_auto`,
`tests.last_results`, `tests.affected`, `tests.classify`, `tests.snapshot_record`,
`tests.snapshot_replay`, `tests.snapshot_diff`, `tests.mutate`.

## Insight (`insight_dispatch.rs`)

`trace`, `traceChain`, `entrypoints`, `graphExport`, `insightStats`, `deadCode`, `impact`,
`tableImpact`, `suggestEvent`, `eventMap`.

## Debug (`debug_dispatch.rs`)

`debug` with `params.cmd` ∈ { `start`, `set_breakpoint`, `continue`, `step_over`, `step_into`,
`step_out`, `state`, `eval`, `history`, `stop` }.

## Conventions & limits

- JSON-RPC 2.0 over newline-delimited frames; error codes include standard set plus `-32000`
  (code analysis) and `-32001` (file not found). `null` results serialized explicitly (F-017).
- Hardening: `duplicates` `minTokens`/`minSimilarity` clamped (F-OPEN-007); `graphExport` capped at
  50k nodes+edges; `trace`/`traceChain` depth bounded; 64 MB max request line; ≤64 concurrent
  connections; 30-minute idle shutdown (skipped during an active debug session).
- Unix-only (Unix domain socket at `$XDG_RUNTIME_DIR/al-lsp/{hash}.sock`).

> The exact parameter shapes for each method are defined at the call sites in
> `crates/al-lsp/src/server/daemon/` and mirrored by the `al-explorer` CLI argument parsing; the CLI
> is the most convenient way to invoke any of these.
