# Zed AL Extension Master Plan (PM First Prompt)

## You Are the PM (Read This First)
1. You own execution. Keep scope, order, and quality on track.
2. Before any code changes, read `crates-map.md` and all files in `docs/`.
3. Create `/task` and write one task file per Work Package using `task/_template.md`.
4. You have the freedom to decompose these packages into smaller tasks as you see fit.
5. Update `docs/progress.md` after every milestone with evidence.

## Strategic Guardrails
- **Thin Adapters**: Binaries (`al-cli`, `al-lsp`, `al-explorer`) must contain ZERO business logic. They only handle I/O and transport.
- **Single Source of Truth**: All workspace state, document management, and orchestration lives in `al-core::Workspace`.
- **Stateless Queries**: Queries in `al-core` should rely on the `Workspace` state, not maintain their own side-effects.
- **Explicit Configuration**: Remove all auto-download/PATH fallbacks. All paths must be configured or discovered via `al-core::toolchain`.

## Quality Gates (Non‑Negotiable)
1. No direct `AlParser::parse_quick` or `extract_document_symbols` outside `al-core`.
2. No `SymbolIndex::new` in CLI, LSP, or Explorer binaries.
3. `al-discovery` and `al-dap` crates must be removed; logic folded into `al-core` and `al-lsp` respectively.
4. `al-mcp` must become a library calling `al-core` directly.
5. All Work Packages must include verification via `al-test-harness`.

---

# Execution Phases & Work Packages

## Phase 1: Foundation & Core Refactor
**WP1: Core Engine & State Migration**
- **Goal**: Create `al-core` and migrate the central "brain" of the extension.
- **Scope**: `al-core` skeleton, `Workspace` struct, `DocumentStore` migration, and `al-discovery` logic (project/toolchain/launch).
- **Files**: `crates/al-core/*`, `crates/al-discovery/*`, `crates/al-lsp/src/server.rs`, `crates/al-lsp/src/document.rs`.

**WP2: Query Engine & Thin Adapters**
- **Goal**: Decentralize query logic and thin out the binaries.
- **Scope**: Migrate all LSP-style queries (hover, definition, completions, etc.) to `al-core::queries`. Refactor `al-lsp` and `al-cli` to be thin callers.
- **Files**: `crates/al-lsp/src/*`, `crates/al-cli/src/*`, `crates/al-core/src/queries/*`.

## Phase 2: Toolchain & Asset Parity
**WP3: DAP Integration & Toolchain Logic**
- **Goal**: Fold debugging and toolchain management into the core/LSP.
- **Scope**: Merge `al-dap` into `al-lsp::dap`. Ensure it uses the new `al-core::toolchain` discovery.
- **Files**: `crates/al-dap/*`, `crates/al-lsp/src/dap.rs`, `crates/al-core/src/toolchain.rs`.

**WP4: Language Assets & Parity Commands**
- **Goal**: Reach feature parity with Cursor snippets and Zed tasks.
- **Scope**: Sync grammar, import 23 snippet files, and implement the task/runnable mappings defined in `docs/feature-parity.md`.
- **Files**: `snippets/*`, `languages/al/*`, `extension.toml`, `Makefile`.

## Phase 3: Advanced Intelligence & Performance
**WP5: Semantic & Symbol Optimization**
- **Goal**: Optimize the high-level analysis layers.
- **Scope**: Refactor `al-symbols` for better composition. Optimize `al-semantic` bridge management (caching, process lifetime) within `al-core`.
- **Files**: `crates/al-symbols/*`, `crates/al-semantic/*`, `crates/al-core/src/semantic.rs`.

**WP6: Caching & Observability**
- **Goal**: Meet performance targets and add system transparency.
- **Scope**: Implement disk caching for symbols/ASTs. Integrate `al-diag` for request tracing and latency monitoring.
- **Files**: `crates/al-core/src/workspace.rs`, `crates/al-diag/*`.

## Phase 4: AL Insight (The Killer App)
**WP7: Insight Graph Engine**
- **Goal**: Build the publisher-subscriber and call-graph engine.
- **Scope**: Implement `al-core::insight` using `petgraph`. Add LSP code actions to trigger Insight views.
- **Files**: `crates/al-core/src/insight/*`, `crates/al-lsp/src/handlers.rs`.

**WP8: Explorer & TUI Transformation**
- **Goal**: Provide a rich UI for Insight and workspace exploration.
- **Scope**: Refactor `al-explorer` into a modular TUI that consumes `al-core` data for both standard exploration and Insight graphs.
- **Files**: `crates/al-explorer/src/*`.

## Phase 5: Validation & Release
**WP9: Integration & Release Engineering**
- **Goal**: Finalize for production.
- **Scope**: Exhaustive integration testing via `al-test-harness`. Finalize deterministic build/release pipeline in `Makefile`.
- **Files**: `crates/al-test-harness/*`, `Makefile`, `docs/release.md`.
