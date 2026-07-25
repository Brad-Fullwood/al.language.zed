# Language Server (LSP)

**Modules:** `crates/al-lsp/src/server/` (transport) + `crates/al-analysis/src/queries/` (logic) ·
**Status:** ✅ shipped

`al-lsp --stdio` is a native, Rust language server speaking LSP over stdio. The editor LSP path uses
LSP handlers directly over the shared workspace/queries modules (it does **not** go through the
daemon). This page covers the interactive language features; analysis features (impact, dead code,
etc.) are in [analysis-and-insight](./analysis-and-insight.md), and refactorings are in
[code-actions](./code-actions.md).

## Advertised capabilities

From `server/lsp.rs` `initialize`, the server advertises: full text sync, save (no text), hover,
completion (trigger chars `.` `:`), definition, references, document symbols, document & range
formatting, folding ranges, rename (with prepare), semantic tokens (full + legend), CodeLens,
inlay hints, signature help (trigger chars `(` `,`), workspace symbols, code actions, pull
diagnostics (`identifier: "al-lsp"`, inter-file dependencies), and execute commands.

### Client capability gating

The server adapts its responses to the client (negotiated at `initialize`):

- `textDocument.definition.linkSupport` → returns `LocationLink[]` vs `Location[]`.
- `textDocument.documentSymbol.hierarchicalDocumentSymbolSupport` → nested `DocumentSymbol[]` vs flat
  `SymbolInformation[]` (flattened by `server/conversions.rs`).

## Feature reference

Each feature below names the query module that implements the (transport-agnostic) logic.

| Feature | Query module | Notes |
| --- | --- | --- |
| **Hover** | `queries/hover.rs` | Resolves member access → receiver type → member; local procedure signature; in-scope variable with scope annotation; workspace symbol via index or bridge. Falls back to the semantic bridge (`type_at`). |
| **Completion** | `queries/completions.rs` | Context-driven (member / enum `::` / type `:` / default). Member completion via `resolution::resolve_expression_type` + builtin methods; type position offers builtin types + workspace tables/enums/codeunits/interfaces (capped). Has a cached "blank completion at top level" fast path. Falls back to the bridge (`completions_at`). |
| **Go to definition** | `queries/definition.rs` | Member → receiver type → member def; object name → workspace or package symbol; local var; same-file procedure. Can synthesize a **virtual file** to navigate into `.app` package symbols. |
| **Find references** | `queries/references.rs` | Current-file variable refs + event-subscriber string-literal refs + all workspace files. Dedups exact spans. Runs on `spawn_blocking` for cancellation on large files. |
| **Rename / prepare rename** | `queries/rename.rs` | Produces a `WorkspaceEdit`. Local variables/parameters are renamed only within their procedure to avoid clobbering same-named identifiers elsewhere; cross-file symbols rename workspace-wide. Preserves `"quoted"` identifiers. |
| **Document symbols** | `queries/symbols.rs` (+ `syntax/symbols.rs`) | Hierarchical outline (object → procedures/triggers/events/fields/keys/enum values/controls). |
| **Workspace symbols** | `queries/search.rs` | Case-insensitive substring search across objects and child members. |
| **Semantic tokens** | `queries/semantic_tokens.rs` (+ `syntax/tokens.rs`) | Full-document, delta-encoded; `spawn_blocking`. |
| **Inlay hints** | `queries/inlay_hints.rs` | Parameter-name hints at call sites (default on) and return-type hints on procedures (default off), with type-aware overload resolution. Uses cached doc symbols where available. |
| **CodeLens** | `queries/code_lens.rs` | Reference-count lenses on procedures/triggers/events; profiler lenses (`⏱ Xms · N calls`) when an `.alcpuprofile` is loaded; test status lenses (NotRun/Running/Pass/Fail/Skip) on `[Test]` procedures, carrying a `TestTarget`. |
| **Signature help** | `queries/signature.rs` | Parameter list with active-parameter highlight; overload picked by parameter count, widest as fallback. Bridge-backed `signature_help_full`. |
| **Formatting / range formatting** | `queries/format.rs` (+ `syntax/formatting.rs`) | Loads `.alformat.json`; range formatting uses whole-document indent context. |
| **Folding** | `queries/folding.rs` (+ `syntax/folding.rs`) | Blocks, procedures, comment runs. |
| **Diagnostics** | `queries/diagnostics.rs` + `server/diagnostics.rs` | Two-phase (below). |

### Two-phase diagnostics

`server/diagnostics.rs` runs diagnostics in two phases:

1. **Phase 1 (instant):** tree-sitter parse errors, published immediately on open and on a **400 ms
   debounce** after edits. Debouncing exists so the slow semantic bridge can never block
   hover/completion.
2. **Phase 2 (async):** the .NET CodeAnalysis bridge (`analyze`) for compiler-grade diagnostics,
   gated by `al.enableCodeAnalysis` / `al.backgroundCodeAnalysis` / `al.diagnosticsTrigger` /
   `al.diagnosticsScope`. Results are merged and published when ready.

Both push (`publishDiagnostics`) and pull (`textDocument/diagnostic`) flows are supported; pull
computes both phases synchronously. Diagnostic messages are enriched with descriptions from the
bridge's error-code catalog. Virtual symbol-cache files are skipped.

### Document lifecycle correctness

`did_change` applies edits and reads text under a single write lock, warns on out-of-order versions
rather than erroring, and cancels stale debounced diagnostics.
`did_close` aborts pending diagnostics and clears state. `did_save` always re-publishes.

## LSP execute commands

Handled in `server/commands.rs`:

| Command | Effect |
| --- | --- |
| `al.downloadSymbols` / `al.downloadSymbolsNuget` | Download dependency symbols from NuGet into `.alpackages` |
| `al.downloadSymbolsServer` | Download symbols from the BC server (from `launch.json`) |
| `al.clearSymbolCache` | Remove the on-disk virtual-file/symbol cache |
| `al.formatFile` | Format a document and apply the edit |
| `al.lintFile` | Re-publish diagnostics for a file |
| `al.getStatus` | Health snapshot JSON (version, bridge presence, toolchain, indexed counts) |
| `al.reindex` | Re-run workspace init in the background (aborting any in-flight reindex) |
| `al.compile` | Build the project (native emitter by default; `alc` when `al.useOfficialCompiler`) |
| `al.applyRecommendedSettings` | Apply recommended Zed workspace settings for AL |

### CodeLens command IDs

CodeLens entries invoke `al.findReferences`, `al.showProfiler`, and `al.runTest`. All three are
registered execute commands and reuse the same reference, profiler, and test services as the other
entry points.

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| Implementation | Rust, in-process, on every OS Zed runs on | .NET AL Language Server, VS Code-coupled |
| Interactive latency | native parse + cached workspace + debounced bridge | server round-trips; bridge is the only diagnostics source |
| Diagnostics source | native syntax + optional CodeAnalysis bridge | CodeAnalysis only |
| Escape hatch | `al.useOfficialLsp` delegates the whole session to Microsoft | n/a |
| Editor portability | reused across Zed/CLI/MCP/daemon | VS Code-specific |

For *compiler-grade* semantics the official server is authoritative; that is why the project keeps
the semantic bridge for diagnostics/hover/completions and `al.useOfficialLsp` as a one-setting
delegation. See [semantic-bridge](./semantic-bridge.md).

## Why this approach

Native parsing + a cached workspace model means most language requests are answered from in-memory
indexes without a compiler round-trip, and the slow compiler-grade work (the bridge) is debounced and
isolated so it never blocks typing. The transport-boundary rule means the very same query code serves
the editor, the CLI (`al-explorer hover/definition/references/...`), and MCP clients — one
implementation, no drift.

## How to use

In Zed, these features work automatically once `al-lsp` is resolved. The terminal exposes the listed
CLI query subset through `al-explorer hover|definition|references|signature|completions|symbols|
folding|tokens|rename|hints <file> [pos...]`; daemon-only operations such as `implementations` and
`codeActions` remain reachable through MCP `al_call`. See [cli-and-tui](./cli-and-tui.md) and the
[LSP command reference](../reference/lsp-commands.md).

From MCP, `al_call` exposes the daemon equivalents (`hover`, `definition`, `references`,
`implementations`, `completions`, `signatureHelp`, `rename`, `documentSymbols`, `foldingRanges`,
`semanticTokens`, `inlayHints`, and `codeActions`) with the same workspace/query implementation.

## Limitations & roadmap

- `workspace/diagnostic` reports syntax diagnostics for every indexed workspace file and semantic
  diagnostics for open documents. Running the bridge across every unopened file remains optional
  future work because it requires a compiler round-trip per file.
- References/subscribers over `.app` dependencies are limited because package symbols carry public
  API metadata, not call-site bodies.
- Settings-schema autocomplete only lights up on Zed 0.8+ extension API (see the
  [settings reference](../reference/settings.md)).
