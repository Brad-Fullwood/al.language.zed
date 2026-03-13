# Project Progress Tracker

This file is maintained by the PM agent during execution. Updated after every milestone.

## How to Update
1. Every completed task should be checked off here.
2. Each update must include date, summary, and evidence (files changed or commands run).
3. Do not move to a new phase until the previous phase checklist is complete and verified.

## Status
- Current Phase: Phase 0 Complete — Ready for WP0 Implementation
- Last Update: 2026-03-13

---

## Phase Checklists

### Phase 0: The Adversarial Foundation
**WP0: Project Constitution & Adversarial Harness**
- [x] `claude.md` created as project constitution.
- [x] `.claude/rules/` established (7 mandate files) — all sharpened with daemon architecture constraints.
- [x] `docs/` documentation suite established and expanded:
  - [x] `docs/architecture.md` — rewritten for daemon architecture (layers, state management, .NET bridge lifecycle, error taxonomy, data flows)
  - [x] `docs/feature-scope.md` — expanded with community extensions (AZ AL Dev Tools, NAB AL Tools, ALCops, AL Test Runner) and 11 beyond-parity features
  - [x] `docs/market-research.md` — expanded with competitive landscape, community pain points, BC26/27 changes
  - [x] `docs/adversarial-atlas.md` — NEW: 15 stress tests (ST-01 to ST-15) + 7 fidelity gaps (FG-01 to FG-07)
  - [x] `docs/agentic-schemas.md` — NEW: 10 slash command schemas, 18 CLI --json schemas, 16 MCP tool registrations (insight queries: tables, callgraph, intercept, subscribers, source)
  - [x] `docs/agent-scenarios.md` — NEW: 8 real-world agent discovery scenarios as test cases for insight engine
- [x] `docs/crates-map.md` rewritten for daemon architecture with target module layout.
- [x] `plan.md` decomposed into 44 tasks across WP0-WP11 with IDs, file ownership, dependencies, pass/fail criteria, and critical path.
- [ ] `al-test-harness` LSP simulator built with Zed-fidelity (T001).
- [ ] Centralized evidence log (`proof_of_functionality.toml`) created (T002).
- [ ] Harness daemon-mode extension (T003).

**WPX: Continuous Adversarial Evolution (Persistent)**
- [ ] Adversarial sub-agent deployed.
- [ ] `docs/adversarial-atlas.md` stress test catalog active.
- [ ] Gap-finding protocol running.

### Phase 1: Foundation & Core Refactor
**WP1: al-core Skeleton & Discovery Migration**
- [ ] `al-core` crate created with module tree per `docs/crates-map.md`.
- [ ] `Workspace` struct implemented with state transitions.
- [ ] `al-discovery` logic migrated into `al-core::project` + `al-core::toolchain` + `al-core::launch`.
- [ ] `AlError` unified error hierarchy implemented.

**WP2: Workspace State & Document Management**
- [ ] `DocumentStore` moved from `al-lsp` to `al-core::documents`.
- [ ] Incremental parsing (tree-sitter `tree.edit()`) wired through `al-core`.
- [ ] `FileIndex` (workspace .al file scan) implemented.
- [ ] `AlConfig` (merged settings) implemented.

### Phase 2: Binary Thinning & Language Parity
**WP3: LSP & CLI Thin Adapters (Query Engine)**
- [ ] `al-core::queries::hover` implemented (migrated from `al-lsp`).
- [ ] `al-core::queries::definition` implemented.
- [ ] `al-core::queries::completions` implemented.
- [ ] `al-core::queries::references` implemented.
- [ ] `al-core::queries::signature_help` implemented.
- [ ] `al-core::queries::rename` implemented.
- [ ] `al-core::queries::code_actions` implemented.
- [ ] `al-core::queries::semantic_tokens` implemented.
- [ ] `al-core::queries::folding_ranges` implemented.
- [ ] `al-core::queries::inlay_hints` implemented.
- [ ] `al-core::queries::document_symbols` implemented.
- [ ] `al-lsp` thinned to transport-only (no `use al_syntax::` or `use al_symbols::`).
- [ ] `al-cli` refactored to pure JSON-RPC daemon client (no al-core dependency).
- [ ] Thin-adapter verification passes (`cargo tree` check).

**WP4: DAP Integration, Toolchain Logic & Agentic Debugger**
- [ ] `al-dap` merged into `al-lsp::dap`.
- [ ] DAP proxy wired through `al-core::toolchain`.
- [ ] EditorServices.Host lifecycle managed by `al-core`.
- [ ] DAP locators implemented for one-click test debugging.
- [ ] Headless DAP control via daemon (`al debug start/breakpoint/state/eval/step/continue/stop`).
- [ ] Debug history recording (variable snapshots at breakpoint hits, `--var` filter).
- [ ] MCP `al/debug` tool with identical capabilities.

### Phase 3: Assets & Zed Integration
**WP5: Language Config & Asset Parity**
- [ ] Grammar updated for BC26/27 (`continue`, `@'...'`, `List of [Interface]`).
- [ ] `highlights.scm` finalized.
- [ ] `runnables.scm` expanded (Test, TestPermissions, EventSubscriber, Handler).
- [ ] `outline.scm` expanded (annotations, fields).
- [ ] `brackets.scm` finalized.
- [ ] `inline_values.scm` created for debugging.
- [ ] Semantic tokens expanded from 16 to 31 types (MS parity: builtinFunction, globalVariable, localVariable, tableField, pageControl, pageAction, triggerName, preprocessorKeyword, excludedCode, etc.).
- [ ] All 4 analyzers supported (CodeCop, AppSourceCop, UICop, PerTenantCop) configurable via `al.codeAnalyzers`.
- [ ] 23 snippet files imported and referenced in `extension.toml`.
- [ ] Zed tasks created (Go, Publish, Debug, Download Symbols, Explorer, etc.).

**WP6: WASM Entry & Settings Implementation**
- [ ] `zed-al` WASM refactored (no fallbacks).
- [ ] Slash commands registered (`/al-symbols`, `/al-events`, `/al-object`, `/al-trace`, `/al-deps`, `/al-lint`).
- [ ] Indexed docs provider implemented (`suggest_docs_packages`, `index_docs`).
- [ ] Context server registered (al-mcp).
- [ ] Settings schema with MS parity (20+ settings) wired through `al-core::config`.
- [ ] `workspace/didChangeConfiguration` handler implemented.
- [ ] `InitializationOptions` parsed at startup.

**WP6.5: Build/Publish Pipeline & Error Handling**
- [ ] Error handling audit complete — all silent failures resolved per fail-loudly mandate.
- [ ] `al.package` — compile project to .app file.
- [ ] `al.publish` / `al.publishNoDebug` — deploy to BC server.
- [ ] `al.publishIncremental` — RAD mode (only changed objects).
- [ ] `al.publishDeps` — compile + publish full dependency tree.
- [ ] `al.newProject` — scaffold from template.
- [ ] `al.generatePermissionSet` — auto-generate from extension objects (AL + XML).
- [ ] Snapshot debugging (init, finish, list).
- [ ] CPU profiling via BC profiler.

### Phase 4: Advanced Intelligence & Performance
**WP7: Symbol Indexing & Semantic Optimization**
- [ ] `SymbolIndex` composition optimized with lazy caching.
- [ ] `al-semantic` bridge management centralized in `al-core::semantic`.
- [ ] Bridge auto-restart (max 3) implemented.
- [ ] Bridge request queue (mpsc serialized) implemented.

**WP8: Caching & Observability**
- [ ] Disk caching for symbols/ASTs implemented.
- [ ] `al-diag` request tracing integrated.
- [ ] Performance targets validated (hover <10ms, completion <15ms, etc.).
- [ ] Rope-based text storage implemented (optional optimization).

### Phase 5: AL Insight (The Killer App)
**WP9: Insight Graph & Agent Discovery Engine**
- [ ] `al-core::insight` module created.
- [ ] CallGraph (procedure→procedure) implemented.
- [ ] EventGraph (publisher→subscriber) implemented.
- [ ] ObjectGraph (extends, implements, depends) implemented.
- [ ] TableRelationGraph implemented.
- [ ] Event trace CLI command (`al trace`) implemented.
- [ ] Entry point finder (`al insight entrypoints`) implemented.
- [ ] Graph export (DOT, JSON) implemented.
- [ ] Cross-extension event tracing implemented.

**WP10: Explorer & TUI Transformation**
- [ ] `al-explorer` refactored to pure JSON-RPC daemon client (no al-core dependency).
- [ ] Insight mode (Search, Trace, Events, Tables, Graph tabs).
- [ ] Action images browser.
- [ ] Multiple build configuration switching.

### Phase 6: Validation & Release
**WP11: Integration & Release Engineering**
- [ ] All stress tests from `docs/adversarial-atlas.md` passing.
- [ ] Full Zed-fidelity validation in real Zed environment.
- [ ] Deterministic build/release pipeline (`Makefile`).
- [ ] `docs/release.md` finalized.
- [ ] Extension published to Zed extension registry.

### Beyond v1: Future Features
- [ ] Dead code detection (`al dead-code`).
- [ ] Dependency impact analysis (`al impact`).
- [ ] AI-assisted event wiring (`al suggest-event`).
- [ ] Automated permission set from usage analysis.
- [ ] Code complexity metrics dashboard (`al metrics`).
- [ ] Offline test discovery and scaffolding.
- [ ] Smart rename with translation awareness.
- [ ] Architectural linting (`al lint --arch`).
- [ ] XLF translation management (`al xlf`).

---

## Evidence Log

### 2026-03-13 — Strategic Audit & Architecture Decision (Phase 0)
- **Summary**: Zero-code strategic audit, documentation expansion, and architecture finalization.
- **Architecture decision**: al-lsp is the sole server binary. Two modes: LSP (stdio, for Zed) and daemon (Unix socket, for CLI/Explorer/MCP). al-cli, al-explorer, al-mcp are pure JSON-RPC clients with zero compile-time dependency on al-core. This follows the industry-standard daemon pattern (gopls, rust-analyzer, sorbet).
- **Files created**: `docs/adversarial-atlas.md`, `docs/agentic-schemas.md`.
- **Files rewritten**: `docs/architecture.md`, `docs/crates-map.md`, `claude.md`, `plan.md` (102→535 lines, 44 tasks), `.claude/rules/thin-adapters.md`, `.claude/rules/library-responsibilities.md`, `.claude/rules/maintenance-governance.md`.
- **Files expanded**: `docs/feature-scope.md`, `docs/market-research.md`, `docs/progress.md`, `.claude/rules/validation-protocol.md`, `.claude/rules/zed-fidelity.md`, `.claude/rules/agentic-efficiency.md`, `.claude/rules/adversarial-evolution.md`.
- **Research**: AL community extensions (AZ AL Dev Tools, NAB AL Tools, ALCops, AL Test Runner, AL Dependency MCP Server), Zed extension API v0.7.0 (slash commands, indexed docs, context servers, DAP locators), BC26/27 language changes.
- **Key findings**: ALCops has MCP server (direct competition), AL Dependency MCP Server (Stefan Maron) handles 50MB+ .app files, MS AL crashes on Linux (our #1 differentiator), grammar needs update for `continue`/`@'...'`/`List of [Interface]`.
- **Plan decomposition**: 44 tasks across WP0-WP11. Critical path: T001→T305 = 17 tasks for minimum viable loop. Full production path = ~40 tasks.
- **Audit status**: All documentation architecturally consistent. Daemon architecture verified across all 7 rules files, claude.md, architecture.md, crates-map.md, plan.md, and progress.md. No inconsistencies found.

### 2026-03-13 — Documentation Reorganization & Repo Cleanup
- **Summary**: Full documentation restructuring to achieve "perfect spec" quality before implementation.
- **Repo cruft removed**: `grammars/` (3MB stale dir), `Prompt.md`, `docs/zed-extension-research.md`. Added `/grammars/` to `.gitignore`.
- **Files moved/renamed**: `crates-map.md` → `docs/crates-map.md`, `docs/feature-parity.md` → `docs/feature-scope.md`, `docs/cursor-extension-audit.md` → `docs/ms-extension-audit.md` (trimmed: removed commands/config lists already in feature-scope/settings).
- **Files consolidated**: `docs/insight-tools.md` + `docs/insight-graph-model.md` → `docs/insight.md`. Configuration table in `architecture.md` replaced with reference to `docs/settings.md` (authoritative source).
- **Inconsistencies fixed**: Slash command list in `agentic-efficiency.md` now references `docs/agentic-schemas.md` as authoritative catalog. "Cursor" references replaced with "MS" in feature-scope.md. All cross-references updated across claude.md, plan.md, progress.md, release.md, library-responsibilities.md.
- **Quality improvements**: TOC added to `architecture.md`, LSP cross-reference to `lsp-feature-matrix.md` added, gap-analysis language converted to forward-looking specs.
- **Verification**: Grep confirms zero references to deleted/renamed files across docs/, claude.md, plan.md, and .claude/rules/.
