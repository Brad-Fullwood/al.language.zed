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
- [x] `CLAUDE.md` created as project constitution.
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
- [x] `al-test-harness` LSP simulator built with Zed-fidelity (T001). *(2026-03-14: Baseline verified — 459/461 tests pass, 2 known failures documented in data_driven.rs with specific failure reasons. PoF entry created.)*
- [x] Centralized evidence log (`proof_of_functionality.toml`) created (T002). *(2026-03-14: Restructured with WP0-WP11 section headers, template in header comments, dual-pass format verified. 3 entries, TOML parses.)*
- [x] Harness daemon-mode extension (T003). *(2026-03-14: Transport abstracted — Lifecycle enum, boxed Writer, generic read_loop, from_transport() shared constructor, connect() stub for T303. Zero test regressions. 459/461 pass.)*

**WPX: Continuous Adversarial Evolution (Persistent)**
- [x] Adversarial sub-agent deployed (`.claude/agents/adversarial.md`, opus, worktree isolation).
- [x] Supervisor agent deployed (`.claude/agents/supervisor.md`, periodic progress verification).
- [x] Enforcement hooks active (PreToolUse blocks boundary violations, Stop blocks broken builds).
- [ ] `docs/adversarial-atlas.md` stress test catalog active (tests to be written as code is implemented).
- [ ] Gap-finding protocol running (adversarial agent writes tests, supervisor verifies coverage).

### Phase 1: Foundation & Core Refactor
**WP1: al-core Skeleton & Discovery Migration**
- [x] `al-core` crate created with module tree per `docs/crates-map.md` (T101). *(2026-03-14: lib.rs, workspace.rs (Workspace struct), errors.rs (AlError enum). al-lsp depends on al-core. No circular deps.)*
- [x] `Workspace` struct implemented with state transitions (T101, expanded in T102).
- [x] `al-discovery` logic migrated into `al-core::project` + `al-core::toolchain` + `al-core::launch` (T102). *(2026-03-14: 38 tests across 3 modules. al-lsp callers updated with boundary conversion. al-discovery marked deprecated.)*
- [x] JSON-RPC bridge types migrated to `al-core::jsonrpc` (T103). *(2026-03-14: 6 tests. al-semantic still imports from al-discovery due to circular dep constraint — will switch when al-protocol is created in T104.)*
- [x] al-discovery deleted, replaced by al-protocol (T104). *(2026-03-14: al-protocol created as leaf-level shared crate with types, discovery functions, and JSON-RPC. All 12 workspace crates updated. Zero references to al-discovery remain.)*
- [ ] `AlError` unified error hierarchy implemented.

**WP2: Workspace State & Document Management**
- [x] `DocumentStore` moved from `al-lsp` to `al-core::documents` (T201). *(2026-03-14: Transport-agnostic TextChange/TextRange types replace tower-lsp's TextDocumentContentChangeEvent. 8 unit tests. al-lsp converts at boundary. Old document.rs deleted.)*
- [x] Parse cache moved to `al-core::parsing` (T202). *(2026-03-14: get_or_parse() takes &DocumentStore, returns (String, Tree). al-lsp::parsing becomes thin delegate. 3 tests. No double-parsing.)*
- [x] `Workspace` wired to own DocumentStore, SymbolIndex, AlProject, AlToolchain, SemanticBridge (T203). *(2026-03-14: AlServer.workspace replaces 5 separate fields. All references updated via sed across al-lsp. All tests pass.)*
- [ ] `FileIndex` (workspace .al file scan) implemented.
- [ ] `AlConfig` (merged settings) implemented.

### Phase 2: Binary Thinning & Language Parity
**WP3: LSP & CLI Thin Adapters (Query Engine)**
- [x] `al-core::queries` module skeleton created (T301). *(2026-03-14: 11 query modules with transport-agnostic types. Position/Range/Location/TextEdit/WorkspaceEdit in mod.rs. Each function takes &Workspace, returns None/empty stubs. Zero tower-lsp dependency.)*
- [x] `al-core::queries::hover` implemented (migrated from `al-lsp`). *(2026-03-14: T302)*
- [x] `al-core::queries::definition` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::completions` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::references` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::signature_help` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::rename` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::code_actions` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::semantic_tokens` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::folding_ranges` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::inlay_hints` implemented. *(2026-03-14: T302)*
- [x] `al-core::queries::document_symbols` implemented. *(2026-03-14: T302. resolution.rs (1296 lines) moved to al-core. al-lsp handlers thinned to 5-15 lines each. al-lsp/src/resolution.rs and parsing.rs deleted. 459/461 tests pass unchanged.)*
- [ ] `al-lsp` thinned to transport-only (no `use al_syntax::` or `use al_symbols::`).
- [x] `al-cli` refactored to pure JSON-RPC daemon client (T304). *(2026-03-14: Complete rewrite as thin JSON-RPC client. DaemonClient module auto-starts al-lsp daemon. 26 daemon request methods. Zero compile-time dependency on al-core/al-syntax/al-symbols/al-semantic/al-diag. `cargo tree -p al-cli` clean. 34/34 integration tests pass.)*
- [x] `al-mcp` verified as thin adapter (T305). *(2026-03-14: Already a subprocess-delegation server — shells out to `al --json`. Zero al-core dependency. 20 MCP tools. Output identical to al-cli --json by construction.)*
- [ ] Thin-adapter verification passes (`cargo tree` check for al-explorer).

**T303: Daemon Mode**
- [x] `al-lsp daemon --project <path>` implemented. *(2026-03-14: Unix socket JSON-RPC server at `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock`. 10 query dispatchers (hover, definition, references, completions, signatureHelp, rename, documentSymbols, foldingRanges, semanticTokens, ping/status/shutdown). Workspace init: project discovery, symbol loading, .al file scanning. 30-min idle timeout. Socket cleanup via Drop guard. Zero test regressions.)*

**WP4: DAP Integration, Toolchain Logic & Agentic Debugger**
- [x] `al-dap` merged into `al-lsp::dap` (T401). *(2026-03-14: lib.rs + editor_services.rs moved to al-lsp/src/dap/. al-dap crate deleted. main.rs updated. Zero test regressions.)*
- [x] Toolchain management in al-core (T402). *(2026-03-14: validate_toolchain(), doctor() in al-core::toolchain. DoctorReport struct with camelCase JSON. dispatch_setup thinned from 66 to 5 lines. 5 unit tests. Deferred issue T304-client-no-retry also fixed: DaemonClient retries 3x with 500ms backoff on "initializing" errors, 3 tests.)*
- [x] EditorServices lifecycle managed by `al-core::semantic` (T403). *(2026-03-14: get_or_init_bridge(), restart_bridge(max 3), shutdown_bridge() in al-core. server.rs delegates. dispatch_compile uses workspace bridge, not per-request bridge. bridge_restart_count in Workspace. 5 unit tests.)*
- [ ] `al-dap-client` crate created — DAP protocol, framing, DapClient, EditorServices discovery, AL result types (T404a).
- [ ] `DebugSession` lifecycle — start (compile + DAP handshake) and stop (disconnect + kill + Drop guard) (T404b).
- [ ] Breakpoints + execution control — set_breakpoints, continue, step (T404c).
- [ ] State inspection + eval — threads, stack, variables with Record expansion, eval (T404d).
- [ ] Wire-up — daemon dispatch_debug, CLI `al debug` subcommands, idle timeout protection, architecture rule updates (T404e).
- [ ] Debug history recording — variable snapshots at breakpoint hits, `--var` filter (T405).
- [ ] DAP locators implemented for one-click test debugging.

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
- **Files rewritten**: `docs/architecture.md`, `docs/crates-map.md`, `CLAUDE.md`, `plan.md` (102→535 lines, 44 tasks), `.claude/rules/thin-adapters.md`, `.claude/rules/library-responsibilities.md`, `.claude/rules/maintenance-governance.md`.
- **Files expanded**: `docs/feature-scope.md`, `docs/market-research.md`, `docs/progress.md`, `.claude/rules/validation-protocol.md`, `.claude/rules/zed-fidelity.md`, `.claude/rules/agentic-efficiency.md`, `.claude/rules/adversarial-evolution.md`.
- **Research**: AL community extensions (AZ AL Dev Tools, NAB AL Tools, ALCops, AL Test Runner, AL Dependency MCP Server), Zed extension API v0.7.0 (slash commands, indexed docs, context servers, DAP locators), BC26/27 language changes.
- **Key findings**: ALCops has MCP server (direct competition), AL Dependency MCP Server (Stefan Maron) handles 50MB+ .app files, MS AL crashes on Linux (our #1 differentiator), grammar needs update for `continue`/`@'...'`/`List of [Interface]`.
- **Plan decomposition**: 44 tasks across WP0-WP11. Critical path: T001→T305 = 17 tasks for minimum viable loop. Full production path = ~40 tasks.
- **Audit status**: [SUPERSEDED by 2026-03-14 audit] Documentation was consistent at time of writing. Rules and file structure were subsequently restructured.

### 2026-03-13 — Documentation Reorganization & Repo Cleanup
- **Summary**: Full documentation restructuring to achieve "perfect spec" quality before implementation.
- **Repo cruft removed**: `grammars/` (3MB stale dir), `Prompt.md`, `docs/zed-extension-research.md`. Added `/grammars/` to `.gitignore`.
- **Files moved/renamed**: `crates-map.md` → `docs/crates-map.md`, `docs/feature-parity.md` → `docs/feature-scope.md`, `docs/cursor-extension-audit.md` → `docs/ms-extension-audit.md` (trimmed: removed commands/config lists already in feature-scope/settings).
- **Files consolidated**: `docs/insight-tools.md` + `docs/insight-graph-model.md` → `docs/insight.md`. Configuration table in `architecture.md` replaced with reference to `docs/settings.md` (authoritative source).
- **Inconsistencies fixed**: Slash command list in `agentic-efficiency.md` now references `docs/agentic-schemas.md` as authoritative catalog. "Cursor" references replaced with "MS" in feature-scope.md. All cross-references updated across claude.md, plan.md, progress.md, release.md, library-responsibilities.md.
- **Quality improvements**: TOC added to `architecture.md`, LSP cross-reference to `lsp-feature-matrix.md` added, gap-analysis language converted to forward-looking specs.
- **Verification**: Grep confirms zero references to deleted/renamed files across docs/, claude.md, plan.md, and .claude/rules/.

### 2026-03-14 — Agent Infrastructure Audit & Restructuring
- **Summary**: Pre-implementation audit of all documentation, rules, and agent infrastructure. Full restructuring of `.claude/` configuration.
- **Rules restructured**: 7 always-loaded rules (~340 lines) → 6 rules: 1 always-loaded (18 lines) + 5 conditionally-scoped via `paths:` (~135 lines). Removed: `adversarial-evolution.md`, `maintenance-governance.md`, `library-responsibilities.md`, `validation-protocol.md`, `agentic-efficiency.md`. Added: `architecture.md`, `code-boundaries.md`, `testing.md`, `agentic-output.md`. Rewrote: `thin-adapters.md`, `zed-fidelity.md`.
- **Agents created**: `test-runner` (haiku), `guardian` (sonnet), `auditor` (sonnet), `adversarial` (opus, worktree), `team-lead` (opus), `supervisor` (sonnet).
- **Skills created**: `/test`, `/check`, `/adversarial`, `/audit`, `/pof`, `/supervise`.
- **Hooks created**: `check-thin-adapter.sh` (PreToolUse, blocks), `enforce-boundaries.sh` (PreToolUse, blocks), `post-edit-compile.sh` (PostToolUse, async cargo check), `check-perf-impact.sh` (PostToolUse, advisory), `enforce-on-stop.sh` (Stop, blocks if code broken/tests fail).
- **Files moved**: `plan.md` → `docs/plan.md`, `claude.md` → `CLAUDE.md` (uppercase).
- **Docs fixed**: ST-06 factual error (bridge is in-process CLR, not subprocess), insight CLI naming (`al trace` not `al insight trace`), JSON-RPC types decision (`al-protocol` crate), `al suggest-event` schema added.
- **External tools**: `rust-analyzer-mcp` installed and configured in `.mcp.json`.
- **Infrastructure created**: `.claude/constraints.toml` (22-entry machine-readable index), `.claude/RATIONALE.md`, `docs/start-work.md` (reusable prompt).
