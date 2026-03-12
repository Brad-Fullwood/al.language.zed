# Zed AL Extension Master Plan (PM First Prompt)

## You Are the PM (Read This First)
1. You own execution. Keep scope, order, and quality on track.
2. Before any code changes, read `crates-map.md` and all files in `docs/`.
3. Create `/task` and write one task file per unit of work using `task/_template.md`.
4. Assign one sub‑agent per task and enforce exclusive file ownership.
5. Update `docs/progress.md` after every task completion with evidence.
6. Do not move phases forward until quality gates are met.

## Quick Orientation
- This plan is the authoritative execution guide. It references detailed specs in `docs/`.
- We are building a best‑in‑class AL experience for Zed, exceeding VS Code parity.
- No hard dependency on Cursor/VS Code AL extension is allowed. Only `tree-sitter-al` and Microsoft AL tooling (NuGet DLLs).

## Source‑of‑Truth Docs (Read All)
- `crates-map.md` — final crate boundaries and module ownership.
- `docs/feature-parity.md` — Cursor command parity and mappings.
- `docs/settings.md` — full settings map and status.
- `docs/insight-tools.md` — AL Insight requirements and CLI surface.
- `docs/insight-graph-model.md` — graph schema and indexing strategy.
- `docs/lsp-feature-matrix.md` — LSP request to `al-core` mappings.
- `docs/performance-plan.md` — targets and cache strategy.
- `docs/market-research.md` — feature motivations and competitive baseline.
- `docs/architecture.md` — data flow and ownership rules.
- `docs/release.md` — packaging and release steps.
- `docs/progress.md` — progress tracker and evidence log.

## Execution Rules
1. **Task decomposition**: each task is 1–2 days, one agent, exclusive files, explicit dependencies.
2. **Buildability**: keep the workspace buildable after every task merge.
3. **Ownership**: no two tasks may modify the same file.
4. **Verification**: every task includes commands + acceptance criteria.
5. **Progress**: every task updates `docs/progress.md`.

## Required Tools
- Rust stable + `wasm32-wasip2` target.
- .NET SDK + Microsoft AL Development Tools.
- `tree-sitter` CLI for grammar regeneration.
- Graphviz optional for DOT rendering.

## Quality Gates (Non‑Negotiable)
1. No direct `AlParser::parse_quick` or `extract_document_symbols` outside `al-core`.
2. No `SymbolIndex::new` in CLI, LSP, or Explorer.
3. `al-discovery` crate removed; discovery lives in `al-core`.
4. `al-dap` crate removed; DAP lives in `al-lsp`.
5. No auto‑download or PATH fallback remains in `src/lib.rs` or LSP workspace.
6. Insight graphs live under `al-core::insight`.
7. Docs are consistent: `docs/settings.md`, `docs/feature-parity.md`, `docs/market-research.md`.

## Verification Commands (Run When Relevant)
1. `rg -n "AlParser::parse_quick" crates -g "*.rs"` => only matches in `al-core`.
2. `rg -n "extract_document_symbols" crates -g "*.rs"` => only matches in `al-core`.
3. `rg -n "SymbolIndex::new" crates -g "*.rs"` => no matches in CLI/LSP/Explorer.
4. `rg -n "al-discovery" crates -g "Cargo.toml"` => no matches.
5. `rg -n "al-dap" crates -g "Cargo.toml"` => no matches.
6. `cargo test --workspace` => pass or documented skip.

## Completion Criteria (All Must Be True)
1. All phases complete and checked in `docs/progress.md` with evidence.
2. Parity mappings are implemented (see `docs/feature-parity.md`).
3. Insight features available via CLI, TUI, and LSP code actions.
4. Performance targets in `docs/performance-plan.md` are met or justified.
5. `al-cli`, `al-lsp`, and `al-explorer` are the only binaries.
6. Build, install, grammar regen, and release steps are reproducible (`docs/release.md`).

---

# Plan Phases (Use These to Create /task Files)

## Phase 0 — Inventory and Baselines
1. Dependency graph of crates/binaries.
2. CLI + LSP feature map aligned with `docs/lsp-feature-matrix.md`.
3. Baseline performance and test coverage.

## Phase 1 — Architecture & Core Refactor
1. Create `al-core` and move document store, parsing, workspace, discovery, launch, JSON‑RPC.
2. Centralize queries in `al-core::queries` and make LSP/CLI thin adapters.
3. Fold `al-dap` into `al-lsp`.
4. Refactor `al-explorer` into modules and add Insight scaffold.
5. Dependency hygiene and workspace dependency consolidation.

## Phase 2 — Language Assets and Parity
1. Grammar pipeline: single source `tree-sitter-al`, sync to `languages/al`.
2. Snippet import and `extension.toml` updates.
3. Tasks/runnables parity for publish/debug workflows.
4. Debug adapter schema parity with launch.json.
5. Settings implementation (`docs/settings.md`).
6. Auth, browser, incognito, and credential workflows.

## Phase 3 — Large File Refactors + Query Quality
1. Split huge files in CLI, LSP, syntax, symbols (see `crates-map.md`).
2. Move all query logic into `al-core` and update LSP/CLI.
3. Add advanced LSP features: selectionRange, documentHighlight, hierarchy, codeLens.

## Phase 4 — Remove Fallbacks
1. Remove auto‑download and PATH fallback in `src/lib.rs`.
2. Remove interactive prompts in LSP workspace.

## Phase 5 — Performance and Observability
1. Caching and incremental updates per `docs/performance-plan.md`.
2. Add observability (`al-diag`) and cancellation for long tasks.

## Phase 6 — Tests and Validation
1. Reorganize tests by layer.
2. Real‑world validation with BCApps/ALAppExtensions.
3. Test coverage support.

## Phase 7 — Release and Packaging
1. Makefile targets for grammar/build/release.
2. Deterministic binary distribution + compatibility checks.
3. Documentation complete and current.

## Phase 8 — UX Polish
1. Explorer UX improvements and workflow polish.
2. Clear errors and first‑run setup tasks.

## Phase 9 — AL Insight (Beyond Parity)
1. Build Insight graphs in `al-core::insight` (call/event/object/table).
2. TUI Insight views and graph rendering.
3. LSP code actions for trace/graph.
4. Entry point finder and event recorder integration.
5. Community parity+: object designer features, dependency graphs, file reorg.

---

# Initial Task Pack (PM Must Create /task Files)
Use this list to seed task files. Each task has explicit file ownership and dependencies.

1. **T000 Project Setup** — Files: `docs/progress.md`, `/task/*`. Dependencies: none.
2. **T100 al-core Skeleton** — Files: `crates/al-core/*`, `Cargo.toml`. Depends: T000.
3. **T101 Document Store + Parsing** — Files: `crates/al-lsp/src/document.rs`, `crates/al-lsp/src/parsing.rs`, `crates/al-core/src/documents.rs`, `crates/al-core/src/parsing.rs`. Depends: T100.
4. **T102 Workspace Bootstrapping** — Files: `crates/al-lsp/src/workspace.rs`, `crates/al-core/src/workspace.rs`. Depends: T100.
5. **T103 Fold Discovery** — Files: `crates/al-discovery/*`, `crates/al-core/src/project.rs`, `toolchain.rs`, `launch.rs`, `jsonrpc.rs`, `Cargo.toml`. Depends: T100.
6. **T104 LSP Query Core** — Files: `crates/al-lsp/src/hover.rs`, `definition.rs`, `completions.rs`, `resolution.rs`, `handlers.rs`, `crates/al-core/src/queries/*`. Depends: T100, T101.
7. **T105 LSP Thin Adapters** — Files: `crates/al-lsp/src/server.rs`, `crates/al-lsp/src/handlers.rs`. Depends: T104.
8. **T106 CLI Refactor** — Files: `crates/al-cli/src/main.rs`, `crates/al-cli/src/commands/*`, `crates/al-cli/src/output.rs`. Depends: T100, T104.
9. **T107 Explorer Refactor** — Files: `crates/al-explorer/src/main.rs`, `app.rs`, `ui.rs`, `actions.rs`, `data.rs`, `insight.rs`. Depends: T100.
10. **T108 DAP Merge** — Files: `crates/al-dap/*`, `crates/al-lsp/src/dap.rs`, `editor_services.rs`. Depends: T105.
11. **T109 Remove Old Crates** — Files: `Cargo.toml`, `crates/*/Cargo.toml`. Depends: T103, T108.
12. **T110 Dependency Hygiene** — Files: `Cargo.toml`, `crates/*/Cargo.toml`. Depends: T109.
13. **T200 Grammar Pipeline** — Files: `scripts/grammar_sync.sh`, `Makefile`, `languages/al/*`. Depends: T000.
14. **T201 Snippets + extension.toml** — Files: `snippets/*`, `extension.toml`. Depends: T000.
15. **T202 Tasks + Runnables** — Files: `languages/al/tasks.json`, `languages/al/runnables.scm`. Depends: T000.
16. **T203 Debug Adapter Schema** — Files: `debug_adapter_schemas/al.json`, `crates/al-core/src/launch.rs`. Depends: T103.
17. **T204 Settings Implementation** — Files: `crates/al-core/src/config.rs`, `crates/al-lsp/src/server.rs`. Depends: T100.
18. **T205 Auth + Browser** — Files: `crates/al-core/src/auth.rs`, `crates/al-core/src/launch.rs`, CLI commands. Depends: T204.
19. **T300 Syntax Refactors** — Files: `crates/al-syntax/src/type_resolver.rs`, `lint.rs`, `symbols.rs`. Depends: T100.
20. **T301 Symbols Model Refactor** — Files: `crates/al-symbols/src/model.rs`, `types.rs`. Depends: T100.
21. **T400 Remove Fallbacks** — Files: `src/lib.rs`, `crates/al-lsp/src/workspace.rs`. Depends: T105.
22. **T500 Performance Caching** — Files: `crates/al-core/src/parsing.rs`, `workspace.rs`, `queries/*`. Depends: T104.
23. **T600 Tests + Validation** — Files: `crates/al-test-harness/*`, `crates/al-lsp/tests/*`, `crates/al-cli/tests/*`. Depends: T104, T106.
24. **T700 Release + Docs** — Files: `docs/release.md`, `docs/architecture.md`, `docs/performance-plan.md`. Depends: T000.
25. **T900 Insight Graph Engine** — Files: `crates/al-core/src/insight/*`, `docs/insight-graph-model.md`. Depends: T100, T301.
26. **T901 Insight TUI** — Files: `crates/al-explorer/src/insight.rs`, `crates/al-explorer/src/ui.rs`. Depends: T107, T900.
27. **T902 Insight LSP Actions** — Files: `crates/al-lsp/src/handlers.rs`, `crates/al-core/src/queries/code_actions.rs`. Depends: T104, T900.
28. **T903 Entry Point Finder** — Files: `crates/al-core/src/insight/search.rs`, `crates/al-cli/src/commands/insight.rs`. Depends: T900.
29. **T904 Event Recorder Workflow** — Files: `crates/al-core/src/launch.rs`, `languages/al/tasks.json`. Depends: T203.
30. **T905 Object Designer Parity** — Files: `crates/al-explorer/src/app.rs`, `crates/al-explorer/src/ui.rs`. Depends: T107.
31. **T906 Community Feature Parity** — Files: CLI commands, explorer actions. Depends: T106, T107.

---

# Definitions
- **al-core**: shared analysis and insight engine used by CLI, LSP, Explorer.
- **Insight**: trace/graph tools built on symbols and workspace AST.
- **Parity**: matching or exceeding VS Code AL extension and community tool workflows.
