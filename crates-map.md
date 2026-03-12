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
- `crates/al-core/src/workspace.rs` — `Workspace` struct; owns `DocumentStore`, `SymbolIndex`, and `SemanticBridge`.
- `crates/al-core/src/documents.rs` — `DocumentStore` struct; handles rope-based text and parse tree caching.
- `crates/al-core/src/parsing.rs` — Parse orchestration and caching strategy.
- `crates/al-core/src/queries/mod.rs` — Query entrypoints; routes requests to submodules.
- `crates/al-core/src/queries/hover.rs` — Hover resolution (AST + Symbols + Semantic).
- `crates/al-core/src/queries/definition.rs` — Go-to definition (Workspace + Symbols).
- `crates/al-core/src/queries/references.rs` — Reference search (Workspace + Symbols).
- `crates/al-core/src/queries/completions.rs` — Completion provider (Context + Semantic).
- `crates/al-core/src/queries/signature.rs` — Signature help.
- `crates/al-core/src/queries/rename.rs` — Rename orchestration.
- `crates/al-core/src/queries/semantic_tokens.rs` — Semantic highlighting.
- `crates/al-core/src/queries/folding.rs` — Folding ranges.
- `crates/al-core/src/queries/inlay_hints.rs` — Inlay hints.
- `crates/al-core/src/queries/code_actions.rs` — Quickfixes and Insight triggers.
- `crates/al-core/src/formatting.rs` — Formatting orchestration.
- `crates/al-core/src/linting.rs` — Lint orchestration (Syntax rules + Semantic diagnostics).
- `crates/al-core/src/symbols.rs` — Index orchestration; manages package loading and composition.
- `crates/al-core/src/semantic.rs` — .NET bridge lifecycle; owns the `SemanticBridge` process.
- `crates/al-core/src/project.rs` — `app.json` parsing and project discovery.
- `crates/al-core/src/toolchain.rs` — `ALTool` discovery and validation.
- `crates/al-core/src/launch.rs` — `launch.json` parsing.
- `crates/al-core/src/jsonrpc.rs` — Shared JSON-RPC types for internal/external use.
- `crates/al-core/src/insight/mod.rs` — Insight engine entrypoint.
- `crates/al-core/src/insight/index.rs` — Graph builders (Call, Event, Table).
- `crates/al-core/src/insight/graph.rs` — Graph data structures (Petgraph).
- `crates/al-core/src/insight/search.rs` — Ranking and search for entry points.
- `crates/al-core/src/config.rs` — Unified configuration (Settings + CLI flags).
- `crates/al-core/src/auth.rs` — Authentication for NuGet and BC Server.
- `crates/al-core/src/errors.rs` — Unified error hierarchy for the entire workspace.

## `al-cli`
- `crates/al-cli/src/main.rs` — CLI args and dispatch only.
- `crates/al-cli/src/commands/mod.rs` — Command routing.
- `crates/al-cli/src/commands/*.rs` — Command implementations (thin wrappers over `al-core`).
- `crates/al-cli/src/output.rs` — Formatting logic (JSON/Text).

## `al-lsp`
- `crates/al-lsp/src/main.rs` — Binary entrypoint.
- `crates/al-lsp/src/server.rs` — LSP server wiring; routes requests to `al-core::queries`.
- `crates/al-lsp/src/handlers.rs` — Translates LSP types to/from `al-core` types.
- `crates/al-lsp/src/diagnostics.rs` — Push-based diagnostic wiring.
- `crates/al-lsp/src/dap.rs` — DAP proxy and EditorServices.Host management.

## `al-explorer`
- `crates/al-explorer/src/main.rs` — TUI wiring.
- `crates/al-explorer/src/app.rs` — State and navigation.
- `crates/al-explorer/src/ui.rs` — Ratatui layout and rendering.
- `crates/al-explorer/src/actions.rs` — User input handling.
- `crates/al-explorer/src/data.rs` — Data fetching from `al-core`.

## .NET Bridge Process Ownership
- **Owner**: `al-core::semantic::SemanticBridgeHost`
- **Lifetime**: One instance per `Workspace`.
- **Communication**: JSON-RPC over stdio.
- **Responsibility**: `al-core` ensures the bridge is restarted if it crashes and handles concurrency via a request queue.

---

# Immediate Actions to Reach This Foundation
- Add `al-core` crate and move shared logic out of CLI/LSP/Explorer.
- Convert `al-mcp` to a library that calls `al-core` directly.
- Move duplicated types (`AppDependency`, `NuGetFeed`) into `al-symbols::types`.
- Fold `al-discovery` into `al-core` (project + toolchain + launch modules).
- Fold `al-dap` into `al-lsp` as an internal module.
- Split large files into module trees as listed above.
- Enforce the dependency direction rules in `Cargo.toml` and CI.