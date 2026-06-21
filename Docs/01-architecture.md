# 01 — Architecture

This page describes the crates, binaries, runtime modes, and the data-flow rules that hold the
project together. If you only read one architecture page, read this one.

## Crate layout

The repository is a Cargo workspace. The root package is the Zed WASM extension; the real engine
lives in `crates/`.

```
zed-al  (root package, src/)            WASM extension for Zed (cdylib → wasm32-wasip1)
crates/
  al-core/                              THE ENGINE. All state, queries, build, symbols, tests, DAP.
    src/bin/al-lsp.rs                   The al-lsp binary entry point (LSP / daemon / mcp / dap modes)
    bridge/Bridge.cs, AlBridge.csproj   The .NET CodeAnalysis bridge (feature = "semantic")
  al-protocol/                          Daemon JSON-RPC types + Unix-socket client (al-explorer ↔ daemon)
  al-explorer/                          CLI + TUI companion (Unix-first)
  al-test-harness/                      End-to-end / fidelity / regression test harness
tree-sitter-al/  (git submodule)        Generated AL grammar + generator (al-gen) + committed data
languages/al/                           Generated Zed language package (queries, config, tasks)
schemas/                                JSON Schemas for app.json, rulesets, settings, migration
snippets/, themes/                      AL/JSON snippets and Business Central themes
docs/, examples/                        Settings reference, retirement plan, example settings
```

A historical note from `crates/al-core/src/lib.rs`: the legacy `al-syntax` / `al-symbols` /
`al-semantic` / `al-cli` / `al-dap-client` crates were **consolidated into `al-core` as modules**
(April 2026). Today `al-core` is internally layered into modules — `syntax`, `symbols`, `semantic`,
`dap`, `server`, `queries`, `insight`, `emit`, `test_engine`, `test_runtime` — not separate crates.

### `al-core` module map

| Module | Responsibility | Documented in |
| --- | --- | --- |
| `syntax/` | tree-sitter parsing, tokens, navigation, folding, formatting, complexity, sort | [parsing-and-syntax](./features/parsing-and-syntax.md) |
| `queries/` | transport-agnostic LSP & analysis queries (hover, completion, definition, …, impact, dead_code, …) | [language-server](./features/language-server.md), [code-actions](./features/code-actions.md), [analysis-and-insight](./features/analysis-and-insight.md) |
| `symbols/` | `.app` reading, symbol index, disk cache, composition, NuGet/server download, OAuth | [symbol-and-package-engine](./features/symbol-and-package-engine.md) |
| `emit/` | pure-Rust `.app` (NAVX/ZIP) emitter, `SymbolReference.json`, method-id hashing | [native-app-emitter](./features/native-app-emitter.md) |
| `semantic/` | the in-process .NET CodeAnalysis bridge | [semantic-bridge](./features/semantic-bridge.md) |
| `insight/` | graph engine: call graph, event chains, entrypoints, impact | [analysis-and-insight](./features/analysis-and-insight.md) |
| `dap/`, `native_debug.rs`, `bc_client.rs` | native debug adapter + BC SignalR/REST | [debugging-dap](./features/debugging-dap.md) |
| `test_engine/`, `test_runtime/`, `test_runner.rs` | native interpreter, routing, mutation, coverage, live-BC backend | [native-test-runtime](./features/native-test-runtime.md) |
| `server/` | LSP transport, daemon, MCP, DAP modes — the only place `tower_lsp` types appear | [language-server](./features/language-server.md), [daemon-protocol](./features/daemon-protocol.md), [ai-mcp](./features/ai-mcp.md) |
| `build.rs`, `publish.rs`, `toolchain.rs`, `launch.rs`, `config.rs` | compile/publish orchestration, toolchain discovery, launch.json parsing, settings | [native-app-emitter](./features/native-app-emitter.md) |
| `scaffold.rs`, `generators.rs`, `permissions.rs`, `xliff.rs` | project/object codegen, permission sets, translation | [scaffolding-and-codegen](./features/scaffolding-and-codegen.md), [xliff-translation](./features/xliff-translation.md) |
| `workspace.rs`, `documents.rs`, `file_index.rs`, `parsing.rs` | the live workspace model and document store | this page |

## Binaries

### `al-lsp`

One binary, multiple modes, selected by argument (`crates/al-core/src/bin/al-lsp.rs`):

| Mode | Invocation | Purpose |
| --- | --- | --- |
| **LSP** (default) | `al-lsp --stdio` | Language Server Protocol over stdio. Used by Zed for editing. |
| **Daemon** | `al-lsp daemon --project <path>` | Long-lived JSON-RPC backend over a Unix socket. Used by `al-explorer`, MCP, and Zed tasks. |
| **MCP** | `al-lsp mcp --project <path>` | Model Context Protocol server (stdio, NDJSON-RPC). Used by AI agents / Zed agent panel. |
| **Native DAP** | `al-lsp --dap` | Pure-Rust Business Central debug adapter. |
| **Legacy DAP** | `al-lsp --dap-legacy` | Proxy to Microsoft `EditorServices.Host`. |
| **Official LSP** | `al-lsp --official-lsp` | Delegates the whole session to Microsoft's `ALTool launchlspserver`. |

The LSP and DAP modes are cross-platform. The daemon and MCP modes use Unix domain sockets and are
effectively Unix-only (Windows returns a stub error for the daemon).

### `al-explorer`

The terminal companion (`crates/al-explorer`). With a subcommand it is a JSON-RPC **CLI** client of
the daemon (with `--json` for scripts/CI). With no subcommand on Unix it opens an interactive
**TUI** with five views (object browser, event chain, call graph, profiler, test runner). It is
Unix-first because its daemon IPC uses Unix sockets; on Windows it is a stub. See
[cli-and-tui](./features/cli-and-tui.md).

### `zed-al` (WASM extension)

The root package compiles to `wasm32-wasip1` and runs in Zed's extension sandbox. It does not do
analysis itself — it *wires up* the language server, debug adapter, MCP context server, tasks,
snippets, themes, and settings, and it resolves/downloads the `al-lsp` binary. See
[02-zed-extension](./02-zed-extension.md).

## Runtime topology

```
                       ┌───────────────────────────────────────────────┐
                       │                   al-core engine               │
                       │  workspace · symbols · queries · build · tests │
                       │  insight · emit · dap · semantic bridge        │
                       └───────────────────────────────────────────────┘
                          ▲              ▲             ▲            ▲
            LSP handlers  │   daemon RPC │   MCP tools │   DAP      │
        (server/lsp.rs)   │ (server/     │ (server/    │ (dap/      │
                          │  daemon/)    │  mcp.rs)    │  native_dap)│
        ┌─────────────────┴───┐   ┌──────┴──────┐  ┌───┴────────┐  ┌┴──────────────┐
        │ Zed editor (LSP)    │   │ al-explorer │  │ AI agents  │  │ Zed debugger  │
        │ via zed-al WASM     │   │ CLI / TUI   │  │ (MCP)      │  │ (DAP)         │
        └─────────────────────┘   └─────────────┘  └────────────┘  └───────────────┘
                                          │
                                    Zed tasks also
                                    shell out to al-explorer
```

Two important nuances:

- **Native LSP mode does not go through the daemon.** `al-lsp --stdio` uses LSP handlers
  (`server/lsp.rs`, `server/handlers.rs`, etc.) directly over the same `workspace`/`queries`/`build`
  modules. The daemon dispatcher (`server/daemon/`) is the shared backend for the *CLI, MCP, and Zed
  tasks*, not for the editor LSP path.
- **MCP and CLI converge on one dispatcher.** `server/mcp.rs` maps each MCP tool to a daemon method
  name and calls the same `dispatch_request()` that the CLI uses. This is why MCP tool behavior and
  CLI behavior cannot drift.

## The transport-boundary rule

A core design discipline (stated in `crates/al-core/src/server/mod.rs`):

> `crate::queries::*` returns transport-agnostic types; the `server` module is the **only** place in
> the crate that imports `tower_lsp::lsp_types::*`.

So:

- `syntax/` works in `SyntaxRange` / `SyntaxPosition`.
- `queries/` works in its own `Range` / `Position` / `Location` / `WorkspaceEdit` types.
- `server/conversions.rs` converts query types ⇄ LSP `tower_lsp` types at the wire boundary.

The payoff: every query has exactly one implementation, reused by LSP, daemon, CLI, and MCP, with
conversion isolated at the edges. This is the mechanism behind the project's "one implementation
where possible" north star.

## The live workspace model

`Workspace` (`crates/al-core/src/workspace.rs`) is the shared, `Arc`-wrapped state every surface
reads from. Its key components:

- **`DocumentStore`** (`documents.rs`) — open-document text as shared `Arc<String>`, with a bounded
  parse-tree cache to cap memory.
- **`FileIndex`** (`file_index.rs`) — the on-disk project model: file text, object metadata, parse
  trees, document symbols, procedure definitions, event subscribers, and reverse indexes.
- **`SymbolIndex`** (`symbols/index.rs`) — concurrent (DashMap) symbol index over `.app` packages,
  with secondary indexes by name, kind+id, kind, extension target, and a composed-object cache.
- **Insight / call graph** — built lazily from the symbol index + workspace source and cached, with
  invalidation keyed to edit kind (body-only vs. topology change).
- **Semantic bridge handle** — lazily initialized .NET CodeAnalysis bridge (when built with
  `--features semantic` and `al.enableCodeAnalysis` is on).
- **`AlConfig`** (`config.rs`) — merged workspace settings.

### Workspace initialization sequence

From `server/workspace.rs`, the LSP `initialized` notification spawns a **background** init so the
editor is responsive immediately (ISSUE-026):

1. Toolchain discovery (ALTool, .NET, paths, version).
2. Load builtins + error codes from disk cache (no bridge needed).
3. Project discovery (`app.json`, dependencies, `launch.json`).
4. Workspace file scan → populate `FileIndex`.
5. **Signal readiness** (F-018) so request handlers can proceed *before* any package download.
6. Load pre-cached packages from disk; load runtime enums; invalidate insight graph.
7. If dependencies are declared but uncached, prompt for a download source (Server / NuGet).

Request handlers call an `await_ready()` gate (max ~30 s) so early requests block only until the
workspace is usable, not until downloads finish.

## Concurrency & safety patterns

These recur throughout the engine and are worth knowing as a reader:

- **`Arc` + DashMap** for lock-free concurrent reads of the symbol index; secondary indexes are kept
  in lockstep on removal (the "T049" discipline) to avoid dangling references.
- **Atomic writes** (temp file + rename) for `.app` output, symbol cache, and virtual files, so a
  crash never leaves a half-written artifact.
- **Mtime/size/schema-version cache validation** for parsed packages and external symbols.
- **Bounded everything**: max document size, max archive entries (200k), max decompressed bytes
  (1 GiB), max `.app`/JSON/binary response sizes, parse-tree LRU caps, bounded DAP frames (20 MB) and
  header lines (8 KiB). These are decompression-bomb and DoS defenses.
- **Zeroization** of OAuth tokens on drop (`zeroize`).
- **Debounced diagnostics** (400 ms) so the slow semantic bridge never blocks interactive hover/
  completion (ISSUE-025).
- **Per-operation timeouts** (e.g. DAP step 10 s, variables 30 s, attach 120 s; semantic bridge 30 s
  with a cooldown gate after a timeout).

## Generated vs. authored files

A repository invariant that matters for any contributor (see `README.md` and `ROADMAP.md`):

- `tree-sitter-al/` grammar/parser/query artifacts are **generated** by `al-gen` + `tree-sitter
  generate`.
- `languages/al/` is **generated** Zed output. Canonical queries are copied from
  `tree-sitter-al/queries`; Zed-specific config/tasks/runnables/overrides/etc. are generated from
  templates. **Do not hand-edit `languages/al/`** — the generator rejects unknown files there.
- `themes/bc-themes.json` is generated from Business Central VS Code theme data.
- Release hygiene (`scripts/check-release-hygiene.sh`) enforces that `extension.toml`'s grammar
  `rev` matches the submodule gitlink, versions move together, and generated files are current.

Continue to [02 — Zed Extension Integration](./02-zed-extension.md).
