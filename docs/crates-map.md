# Crate Map and Responsibilities

This document defines the target crate layout, responsibilities, and file/module structure. It is the foundation for refactors and should be updated as we learn more.

**Scope**
- Define what each crate owns.
- Prevent duplicate logic across CLI, LSP, and Explorer.
- Identify new crates, merges, and removals.

**Principles**
- `al-lsp` is the single server binary. All consumers go through it.
- `al-cli` and `al-explorer` are thin JSON-RPC clients — no compile-time dependency on `al-core`.
- `al-core` is the brain — state management, queries, orchestration. Only `al-lsp` imports it.
- Analysis libraries are layered and dependency-directional.
- Shared data types live in a single crate.
- No implicit fallbacks or auto-download behavior in user-facing paths.

**Target Crate Map**
| Crate | Type | Status | Responsibilities | Notes |
| --- | --- | --- | --- | --- |
| `zed-al` (root) | `cdylib` | Keep | Zed extension entrypoint; spawns al-lsp with explicit path; provides labels, slash commands, DAP wiring | Keep at workspace root for Zed packaging |
| `al-cli` | Bin | Keep | CLI argument parsing, daemon connection, output formatting only. Pure JSON-RPC client. | No dependency on al-core — talks to al-lsp daemon via socket |
| `al-lsp` | Bin | Keep | The server. LSP mode (stdio, for Zed) + daemon mode (Unix socket, for CLI/Explorer). Routes all requests to `al-core`. Owns tower-lsp transport and DAP proxy. | The sole entry point for all analysis |
| `al-explorer` | Bin | Keep | TUI rendering and input only. Pure JSON-RPC client connecting to al-lsp daemon. | No dependency on al-core |
| `al-core` | Lib (new) | Add | Central engine: Workspace state, DocumentStore, SymbolIndex, SemanticBridge lifecycle, all query implementations, insight graphs, config, errors | Only al-lsp imports this |
| `al-syntax` | Lib | Keep | Tree-sitter parsing, tokens, folding, formatting, lint rules, navigation, type resolution | Must not depend on discovery or symbols |
| `al-symbols` | Lib | Keep | Parse `.app` packages, symbol index, composition, events, source extraction | Owns `AppDependency`, `NuGetFeed` for package fetch |
| `al-semantic` | Lib | Keep | .NET CodeAnalysis bridge, caches, semantic queries | Lifecycle managed by `al-core` |
| `al-diag` | Lib | Keep | Diagnostics tracing layer and SQLite query API | Optional feature in `al-core` |
| `al-mcp` | Bin | Keep | MCP server. Thin JSON-RPC adapter connecting to al-lsp daemon. | Same architecture as al-cli but MCP transport |
| `al-protocol` | Lib (new) | Add | JSON-RPC request/response types, method names, error codes for daemon protocol. Serde structs only — no logic. | Depended on by al-lsp, al-cli, al-explorer, al-mcp |
| `al-test-harness` | Lib | Keep | Full-stack integration harness: spawns al-lsp, tests via JSON-RPC, validates full pipeline | Depends on al-lsp (spawns it as a process) |

**Dependency Direction (Target)**
```
Compile-time dependencies:
  al-lsp → al-core → al-syntax, al-symbols, al-semantic, al-diag

Runtime connections (JSON-RPC, no compile-time dep on al-core):
  al-cli        → al-lsp daemon (Unix socket)
  al-explorer   → al-lsp daemon (Unix socket)
  al-mcp        → al-lsp daemon (Unix socket)
  zed-al (WASM) → al-lsp process (stdio)

Test:
  al-test-harness → spawns al-lsp process (tests full stack)

Analysis libraries (no upward deps):
  al-symbols → (standalone)
  al-syntax  → (standalone)
  al-semantic → (standalone)
  al-diag    → (standalone)
```

**Shared Types Ownership (Single Source)**
- `AppDependency`, `NuGetFeed` → `al-symbols`
- Project model (`AlProject`, `AppManifest`) → `al-core` (project module)
- Toolchain model (`AlToolchain`, analyzer paths) → `al-core` (toolchain module)
- Symbol model (`SymbolEntry`, `ObjectKind`, `SymbolPackage`) → `al-symbols`
- Query results (hover, definition, completions, rename edits) → `al-core`
- JSON-RPC protocol types (shared between al-lsp, al-cli, al-mcp, al-explorer) → `al-protocol` (standalone types-only crate)

**Decision: `al-protocol` crate.** A tiny crate containing only the JSON-RPC request/response serde structs, method names, and error codes. Thin adapters depend on `al-protocol` (types only, no logic). al-lsp and al-core also depend on it. This preserves the thin-adapter mandate (no al-core dependency) while providing type safety (better than raw serde_json). The crate has no analysis logic — just `#[derive(Serialize, Deserialize)]` structs.

---

# Module and File Layout (Target)

## `al-core` (new)
- `crates/al-core/src/lib.rs` — Public API surface for all shared operations.
- `crates/al-core/src/workspace.rs` — `Workspace` struct; owns `DocumentStore`, `SymbolIndex`, and `SemanticBridge`.
- `crates/al-core/src/documents.rs` — `DocumentStore` struct; handles text and parse tree caching.
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
- ~~`crates/al-core/src/jsonrpc.rs`~~ — **Moved to `al-protocol` crate** (see shared types decision above).
- `crates/al-core/src/insight/mod.rs` — Insight engine entrypoint.
- `crates/al-core/src/insight/index.rs` — Graph builders (Call, Event, Table).
- `crates/al-core/src/insight/graph.rs` — Graph data structures (Petgraph).
- `crates/al-core/src/insight/search.rs` — Ranking and search for entry points.
- `crates/al-core/src/config.rs` — Unified configuration (Settings + CLI flags).
- `crates/al-core/src/auth.rs` — Authentication for NuGet and BC Server.
- `crates/al-core/src/errors.rs` — Unified error hierarchy for the entire workspace.

## `al-lsp` (server binary)
- `crates/al-lsp/src/main.rs` — Binary entrypoint. Parses args: `al-lsp` (LSP stdio), `al-lsp daemon --project <path>` (Unix socket).
- `crates/al-lsp/src/server.rs` — LSP server wiring (tower-lsp); routes requests to `al-core::queries`.
- `crates/al-lsp/src/daemon.rs` — Daemon mode: Unix socket listener, JSON-RPC handler, auto-shutdown timer.
- `crates/al-lsp/src/handlers.rs` — Translates LSP types to/from `al-core` types.
- `crates/al-lsp/src/diagnostics.rs` — Push-based diagnostic wiring.
- `crates/al-lsp/src/dap.rs` — DAP proxy and EditorServices.Host management.

## `al-cli` (thin JSON-RPC client)
- `crates/al-cli/src/main.rs` — CLI args (clap) and daemon connection.
- `crates/al-cli/src/client.rs` — JSON-RPC client: connect to daemon socket, send request, receive response. Auto-start daemon if not running.
- `crates/al-cli/src/commands/mod.rs` — Command routing.
- `crates/al-cli/src/commands/*.rs` — Command implementations (build JSON-RPC request, format response).
- `crates/al-cli/src/output.rs` — Formatting logic (JSON/Text).
- `crates/al-cli/src/project.rs` — Project root detection (walk up to find `app.json`), socket path computation.

## `al-explorer` (thin JSON-RPC TUI client)
- `crates/al-explorer/src/main.rs` — TUI wiring and daemon connection.
- `crates/al-explorer/src/client.rs` — JSON-RPC client (same protocol as al-cli).
- `crates/al-explorer/src/app.rs` — State and navigation.
- `crates/al-explorer/src/ui.rs` — Ratatui layout and rendering.
- `crates/al-explorer/src/actions.rs` — User input handling.

## `al-mcp` (thin JSON-RPC MCP adapter)
- `crates/al-mcp/src/main.rs` — MCP server binary. Connects to al-lsp daemon, exposes MCP tools.
- `crates/al-mcp/src/tools.rs` — MCP tool definitions mapping to al-lsp JSON-RPC methods.

## .NET Bridge Process Ownership
- **Owner**: `al-core::semantic::SemanticBridgeHost`
- **Lifetime**: One instance per `Workspace` (inside al-lsp process).
- **Communication**: JSON-RPC over stdio to the .NET process.
- **Responsibility**: `al-core` ensures the bridge is restarted if it crashes and handles concurrency via a request queue.

---

# Crates to Remove or Fold

| Current Crate | Action | Target |
|---|---|---|
| `al-discovery` | Fold into `al-core` | `al-core::project`, `al-core::toolchain`, `al-core::launch` |
| `al-dap` | Fold into `al-lsp` | `al-lsp::dap` |

# Immediate Actions to Reach This Foundation
1. Add `al-core` crate and move shared logic out of al-lsp.
2. Add daemon mode to `al-lsp` (Unix socket listener + JSON-RPC handler).
3. Refactor `al-cli` from direct al-core consumer to JSON-RPC daemon client.
4. Refactor `al-explorer` from direct al-core consumer to JSON-RPC daemon client.
5. Refactor `al-mcp` to connect to al-lsp daemon instead of calling al-core directly.
6. Move duplicated types (`AppDependency`, `NuGetFeed`) into `al-symbols::types`.
7. Fold `al-discovery` into `al-core` (project + toolchain + launch modules).
8. Fold `al-dap` into `al-lsp` as an internal module.
9. Define the JSON-RPC protocol schema for daemon communication.
10. Enforce the dependency direction rules in `Cargo.toml` and CI.
