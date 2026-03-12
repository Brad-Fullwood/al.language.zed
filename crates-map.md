# Crate Map and Responsibilities

This document defines the target crate layout, responsibilities, and file/module structure. It is the foundation for refactors and should be updated as we learn more.

**Scope**
- Define what each crate owns.
- Prevent duplicate logic across CLI, LSP, and Explorer.
- Identify new crates, merges, and removals.

**Principles**
- Binaries are thin wrappers only.
- Libraries are layered and dependency-directional.
- Shared data types live in a single crate.
- No implicit fallbacks or auto-download behavior in user-facing paths.

**Target Crate Map**
| Crate | Type | Status | Responsibilities | Notes |
| --- | --- | --- | --- | --- |
| `zed-al` (root) | `cdylib` | Keep | Zed extension entrypoint; spawns LSP with explicit path; provides labels and DAP wiring | Keep at workspace root for Zed packaging |
| `al-cli` | Bin | Keep | CLI argument parsing and I/O only; calls into `al-core` services | No direct business logic |
| `al-lsp` | Bin | Keep | LSP transport, initialization, and client wiring only; delegates all queries to `al-core` | Keep `tower-lsp` and protocol types here |
| `al-explorer` | Bin | Keep | TUI UI layer (Explorer + Insight modes); uses `al-core` for data and queries | No direct symbol parsing or discovery |
| `al-core` | Lib (new) | Add | Central analysis pipeline, workspace model, discovery/toolchain, query services, shared caches | The main shared API for CLI/LSP/Explorer/MCP |
| `al-syntax` | Lib | Keep | Tree-sitter parsing, tokens, folding, formatting, lint rules, navigation, type resolution | Must not depend on discovery or symbols |
| `al-symbols` | Lib | Keep | Parse `.app` packages, symbol index, composition, events, source extraction | Owns `AppDependency`, `NuGetFeed` for package fetch |
| `al-semantic` | Lib | Keep | .NET CodeAnalysis bridge, caches, semantic queries | Configured by `al-core` |
| `al-diag` | Lib | Keep | Diagnostics tracing layer and SQLite query API | Optional feature in `al-core` and bins |
| `al-mcp` | Lib | Convert | MCP server implementation that calls `al-core` directly | Remove binary and avoid shelling out to CLI |
| `al-test-harness` | Lib | Keep | Shared testing utilities and integration harness | Depends on `al-core` and `al-symbols` |

**Dependency Direction (Target)**
- `al-cli`, `al-lsp`, `al-explorer`, `al-mcp` → `al-core`
- `al-core` → `al-syntax`, `al-symbols`, `al-semantic`, `al-diag`
- `al-symbols` → (no higher-level crates)
- `al-syntax` → (no higher-level crates)
- `al-semantic` → (no higher-level crates)

**Shared Types Ownership (Single Source)**
- `AppDependency`, `NuGetFeed` → `al-symbols`
- Project model (`AlProject`, `AppManifest`) → `al-core` (project module)
- Toolchain model (`AlToolchain`, analyzer paths) → `al-core` (toolchain module)
- Symbol model (`SymbolEntry`, `ObjectKind`, `SymbolPackage`) → `al-symbols`
- Query results (hover, definition, completions, rename edits) → `al-core`

---

# Module and File Layout (Target)

## `al-core` (new)
- `crates/al-core/src/lib.rs` — Public API surface for all shared operations.
- `crates/al-core/src/workspace.rs` — `Workspace` model, caches, and lifecycle.
- `crates/al-core/src/documents.rs` — Document store, rope handling, parse cache.
- `crates/al-core/src/parsing.rs` — Parse orchestration and caching strategy.
- `crates/al-core/src/queries/mod.rs` — LSP-style query entrypoints.
- `crates/al-core/src/queries/hover.rs` — Hover resolution pipeline.
- `crates/al-core/src/queries/definition.rs` — Go-to definition pipeline.
- `crates/al-core/src/queries/references.rs` — Reference search pipeline.
- `crates/al-core/src/queries/completions.rs` — Completion pipeline.
- `crates/al-core/src/queries/signature.rs` — Signature help pipeline.
- `crates/al-core/src/queries/rename.rs` — Rename pipeline.
- `crates/al-core/src/queries/semantic_tokens.rs` — Tokenization pipeline.
- `crates/al-core/src/queries/folding.rs` — Folding range pipeline.
- `crates/al-core/src/queries/inlay_hints.rs` — Inlay hint pipeline.
- `crates/al-core/src/queries/code_actions.rs` — Quickfix and source actions.
- `crates/al-core/src/formatting.rs` — Formatting orchestration (delegates to `al-syntax`).
- `crates/al-core/src/linting.rs` — Lint orchestration (delegates to `al-syntax` + `al-semantic`).
- `crates/al-core/src/symbols.rs` — Package loading, symbol indexing, composition.
- `crates/al-core/src/semantic.rs` — Semantic bridge wrapper and cache strategy.
- `crates/al-core/src/project.rs` — Project discovery and `app.json` parsing.
- `crates/al-core/src/toolchain.rs` — Toolchain discovery and validation.
- `crates/al-core/src/launch.rs` — `launch.json` parsing.
- `crates/al-core/src/jsonrpc.rs` — JSON-RPC helper types.
- `crates/al-core/src/insight/mod.rs` — Insight engine entrypoint.
- `crates/al-core/src/insight/index.rs` — Graph builders and incremental updates.
- `crates/al-core/src/insight/graph.rs` — Graph types and algorithms.
- `crates/al-core/src/insight/search.rs` — Ranking and search utilities.
- `crates/al-core/src/config.rs` — Explicit path config and runtime options.
- `crates/al-core/src/auth.rs` — Authentication flows and credential handling.
- `crates/al-core/src/errors.rs` — Shared error types for CLI/LSP/MCP.

## `al-cli`
- `crates/al-cli/src/main.rs` — CLI args and dispatch only.
- `crates/al-cli/src/commands/mod.rs` — Command routing into `al-core`.
- `crates/al-cli/src/commands/*.rs` — One file per subcommand or command group.
- `crates/al-cli/src/output.rs` — JSON and human-readable formatting.

## `al-lsp`
- `crates/al-lsp/src/main.rs` — Binary entrypoint only.
- `crates/al-lsp/src/server.rs` — LSP server wiring and request routing.
- `crates/al-lsp/src/handlers.rs` — Thin adapters calling `al-core::queries`.
- `crates/al-lsp/src/diagnostics.rs` — LSP publish diagnostics wiring only.
- `crates/al-lsp/src/dap.rs` — DAP proxy integration (moved from `al-dap`).
- `crates/al-lsp/src/editor_services.rs` — EditorServices.Host location logic.

## `al-explorer`
- `crates/al-explorer/src/main.rs` — TUI wiring only.
- `crates/al-explorer/src/app.rs` — App state, panes, and navigation.
- `crates/al-explorer/src/ui.rs` — Rendering and layout.
- `crates/al-explorer/src/actions.rs` — User actions and command routing.
- `crates/al-explorer/src/data.rs` — Calls into `al-core` for queries and symbols.
 - `crates/al-explorer/src/insight.rs` — Trace and graph views (Insight mode).

## `al-syntax`
- `crates/al-syntax/src/lib.rs` — Public API exports.
- `crates/al-syntax/src/parser.rs` — Tree-sitter integration and parsing.
- `crates/al-syntax/src/context.rs` — Context detection and call-site extraction.
- `crates/al-syntax/src/navigation.rs` — AST navigation helpers.
- `crates/al-syntax/src/type_resolver.rs` — Type inference and resolver.
- `crates/al-syntax/src/formatting/mod.rs` — Formatting orchestration.
- `crates/al-syntax/src/formatting/*.rs` — Formatting rules by construct.
- `crates/al-syntax/src/lint/mod.rs` — Lint runner and rule registry.
- `crates/al-syntax/src/lint/rules/*.rs` — Individual lint rules.
- `crates/al-syntax/src/tokens.rs` — Semantic token extraction.
- `crates/al-syntax/src/folding.rs` — Folding range extraction.
- `crates/al-syntax/src/symbols.rs` — Document symbols extraction.

## `al-symbols`
- `crates/al-symbols/src/lib.rs` — Public API exports.
- `crates/al-symbols/src/model.rs` — Symbol data model.
- `crates/al-symbols/src/app_reader.rs` — `.app` parsing and decompression.
- `crates/al-symbols/src/manifest.rs` — NavxManifest parsing.
- `crates/al-symbols/src/index.rs` — Symbol index and search.
- `crates/al-symbols/src/composition.rs` — Object composition logic.
- `crates/al-symbols/src/events.rs` — Event discovery.
- `crates/al-symbols/src/source_index.rs` — Source indexing within packages.
- `crates/al-symbols/src/virtual_file.rs` — Source extraction helpers.
- `crates/al-symbols/src/types.rs` — `AppDependency`, `NuGetFeed`, shared fetch types.
- `crates/al-symbols/src/fetch/mod.rs` — Download orchestration (NuGet or server).
- `crates/al-symbols/src/fetch/nuget.rs` — NuGet client.
- `crates/al-symbols/src/fetch/bc_server.rs` — Server download.
- `crates/al-symbols/src/fetch/oauth.rs` — Auth helpers.

## `al-semantic`
- `crates/al-semantic/src/lib.rs` — Public API exports and `SemanticBridge`.
- `crates/al-semantic/src/host.rs` — .NET host loading and call dispatch.
- `crates/al-semantic/src/protocol.rs` — JSON protocol structs.
- `crates/al-semantic/src/cache.rs` — Disk cache for builtins and error codes.

## `al-diag`
- `crates/al-diag/src/lib.rs` — Public API exports.
- `crates/al-diag/src/layer.rs` — Tracing layer implementation.
- `crates/al-diag/src/writer.rs` — SQLite writer.
- `crates/al-diag/src/query.rs` — Query API.

## `al-mcp`
- `crates/al-mcp/src/lib.rs` — MCP server and tool definitions.
- `crates/al-mcp/src/tools/*.rs` — One file per MCP tool group.

## `al-test-harness`
- `crates/al-test-harness/src/lib.rs` — Harness utilities.
- `crates/al-test-harness/src/protocol.rs` — Test protocol and fixtures.
- `crates/al-test-harness/tests/*.rs` — Integration tests.

---

# Immediate Actions to Reach This Foundation
- Add `al-core` crate and move shared logic out of CLI/LSP/Explorer.
- Convert `al-mcp` to a library that calls `al-core` directly.
- Move duplicated types (`AppDependency`, `NuGetFeed`) into `al-symbols::types`.
- Fold `al-discovery` into `al-core` (project + toolchain + launch modules).
- Fold `al-dap` into `al-lsp` as an internal module.
- Split large files into module trees as listed above.
- Enforce the dependency direction rules in `Cargo.toml` and CI.