# Zed AL Extension Master Plan (PM First Prompt)

## You Are the PM (Read This First)
1. You own execution. Keep scope, order, and quality on track.
2. Before any code changes, read `crates-map.md` and all files in `docs/`.
3. Create `/task` and write one task file per Work Package using `task/_template.md`.
4. You have the freedom to decompose these packages into smaller tasks.
5. Update `docs/progress.md` after every milestone with evidence.

## Strategic Guardrails
- **The "Thin Adapter" Rule**: Binaries (`al-cli`, `al-lsp`, `al-explorer`) and the WASM entry point (`zed-al`) must contain ZERO business logic. They handle transport, UI, and Zed-specific protocol glue only.
- **Unified Analysis Engine**: All analysis, indexing, and logic live in `al-core`. This ensures that what a dev sees in Zed is 100% consistent with what an AI agent sees via the CLI/MCP.
- **Zed-Native Performance**: Meet all latency targets in `docs/performance-plan.md` to ensure Zed feels "instant." Use incremental parsing and indexing.
- **Agentic Context Density**: CLI/MCP outputs must be high-density (e.g., event traces) to minimize token usage, replacing expensive multi-file reads for agents.

## Quality Gates (Non‑Negotiable)
1. No direct `AlParser::parse_quick` or `extract_document_symbols` outside `al-core`.
2. No `SymbolIndex::new` in CLI, LSP, or Explorer binaries.
3. `al-discovery` and `al-dap` crates must be removed; logic folded into `al-core` and `al-lsp`.
4. `al-mcp` must be a library calling `al-core` directly, maintaining 1:1 parity with CLI features.
5. All Work Packages must include verification via `al-test-harness`.

---

# Execution Phases & Work Packages

## Phase 1: Foundation & Core Refactor
**WP1: al-core Skeleton & Discovery Migration**
- **Goal**: Establish the central "brain" and project discovery logic.
- **Scope**: Create `al-core`, implement the `Workspace` state container, and migrate `al-discovery` (project/toolchain/launch) into `al-core`.
- **Zed Impact**: Provides the foundation for project loading and toolchain detection in Zed.
- **Agent Impact**: Accurate project discovery for CLI/MCP queries.

**WP2: Workspace State & Document Management**
- **Goal**: Centralize document handling and parse caching.
- **Scope**: Move `DocumentStore` from `al-lsp` to `al-core`. Implement rope-based text and incremental parse tree caching in the core.
- **Zed Impact**: Faster file opening and typing responsiveness.
- **Agent Impact**: Stable, cached state for multi-turn agent interactions.

## Phase 2: Binary Thinning & Language Parity
**WP3: LSP & CLI Thin Adapters (Query Engine)**
- **Goal**: Move query logic to the core and thin out the binaries.
- **Scope**: Migrate hover, definition, completions, and references to `al-core::queries`. Refactor `al-lsp` (transport) and `al-cli` (agent discovery) to use these queries.
- **Zed Impact**: Reliable, standard LSP behavior.
- **Agent Impact**: Surgical, context-dense discovery calls.

**WP4: DAP Integration & Toolchain Logic**
- **Goal**: Fold debugging and toolchain management into the core/LSP.
- **Scope**: Merge `al-dap` into `al-lsp::dap`. Refactor EditorServices management to use `al-core::toolchain`.
- **Zed Impact**: Seamless "Publish & Debug" from Zed.
- **Agent Impact**: Accurate toolchain verification via CLI `setup` and `doctor`.

## Phase 3: Assets & Zed Integration
**WP5: Language Config & Asset Parity**
- **Goal**: High-fidelity Zed integration and snippet parity.
- **Scope**: Sync grammar, import 23 snippet files, and implement `highlights.scm`, `runnables.scm`, and `brackets.scm`. Implement all Zed tasks (AL: Go, AL: Publish, etc.).
- **Zed Impact**: Beautiful highlighting, snippets, and task-based workflows.
- **Agent Impact**: Parity in available commands and scaffold generation.

**WP6: WASM Entry & Settings Implementation**
- **Goal**: Clean up the user-facing extension entry point.
- **Scope**: Refactor `zed-al` (WASM) to remove fallbacks. Implement full settings map in `al-core::config`.
- **Zed Impact**: Explicit, predictable settings behavior; no "auto-download" surprises.

## Phase 4: Advanced Intelligence & Performance
**WP7: Symbol Indexing & Semantic Optimization**
- **Goal**: Scale analysis to large AL projects.
- **Scope**: Refactor `al-symbols` for better composition. Optimize `al-semantic` bridge management (caching, process lifetime) within `al-core`.
- **Zed Impact**: Instant symbol navigation across massive projects.
- **Agent Impact**: 100% accurate symbol resolution for cross-package dependencies.

**WP8: Caching & Observability**
- **Goal**: Meet performance targets and add system transparency.
- **Scope**: Implement disk caching for symbols/ASTs. Integrate `al-diag` for request tracing and latency monitoring.
- **Zed Impact**: Sub-10ms hover/completion; fast warm starts.

## Phase 5: AL Insight (The Killer App)
**WP9: Insight Graph & Agent Discovery Engine**
- **Goal**: Build the publisher-subscriber and call-graph engine.
- **Scope**: Implement `al-core::insight` using `petgraph`. Ensure the CLI/MCP/TUI can perform complex traces (e.g., event chain) in a single call.
- **Zed Impact**: Visual call graphs and event traces in `al-explorer`.
- **Agent Impact**: Context-dense traces that replace manual file scanning.

**WP10: Explorer & TUI Transformation**
- **Goal**: Provide a rich UI for workspace exploration.
- **Scope**: Refactor `al-explorer` into a modular TUI consuming `al-core` data.
- **Zed Impact**: High-density object/event designer accessible via Zed task.

## Phase 6: Validation & Release
**WP11: Integration & Release Engineering**
- **Goal**: Finalize for production.
- **Scope**: Exhaustive integration testing via `al-test-harness`. Finalize deterministic build/release pipeline.
- **Files**: `crates/al-test-harness/*`, `Makefile`, `docs/release.md`.
