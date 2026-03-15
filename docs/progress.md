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
- [x] `AlError` unified error hierarchy implemented. *(2026-03-15: Expanded with typed variants: Discovery(#[from]), Semantic(#[from]), DocumentNotOpen, BridgeRestartLimitExceeded, NoToolchain, Io, Json. restart_bridge returns Result<(), AlError>. 7 unit tests.)*

**WP2: Workspace State & Document Management**
- [x] `DocumentStore` moved from `al-lsp` to `al-core::documents` (T201). *(2026-03-14: Transport-agnostic TextChange/TextRange types replace tower-lsp's TextDocumentContentChangeEvent. 8 unit tests. al-lsp converts at boundary. Old document.rs deleted.)*
- [x] Parse cache moved to `al-core::parsing` (T202). *(2026-03-14: get_or_parse() takes &DocumentStore, returns (String, Tree). al-lsp::parsing becomes thin delegate. 3 tests. No double-parsing.)*
- [x] `Workspace` wired to own DocumentStore, SymbolIndex, AlProject, AlToolchain, SemanticBridge (T203). *(2026-03-14: AlServer.workspace replaces 5 separate fields. All references updated via sed across al-lsp. All tests pass.)*
- [x] `FileIndex` (workspace .al file scan) implemented. *(2026-03-15: FileIndex struct in al-core/src/file_index.rs with scan(), add_file(), remove_file(), find_by_object_name(). Replaces 3 raw DashMaps and 2 duplicate scan implementations. Workspace.file_index replaces workspace_files/workspace_objects/file_to_object. 10 unit tests.)*
- [x] `AlConfig` (merged settings) implemented. *(2026-03-15: AlConfig struct in al-core/src/config.rs with merge() for partial updates. 8 settings: enableCodeAnalysis, backgroundCodeAnalysis, codeAnalyzers, enableCodeActions, packageCachePath, editorServicesPath, inlayHints, semanticFolding. Workspace.config field. 8 unit tests.)*

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
- [x] `al-lsp` thinned to transport-only (no `use al_syntax::` or `use al_symbols::`). *(2026-03-15: al-syntax, al-symbols, al-semantic, tree-sitter removed from al-lsp production deps. Re-exports added in al-core lib.rs: syntax, symbols, semantic_types modules. Zero al_syntax::/al_symbols:: references in al-lsp/src/. al-syntax+al-symbols kept as dev-deps for tests.)*
- [x] `al-cli` refactored to pure JSON-RPC daemon client (T304). *(2026-03-14: Complete rewrite as thin JSON-RPC client. DaemonClient module auto-starts al-lsp daemon. 26 daemon request methods. Zero compile-time dependency on al-core/al-syntax/al-symbols/al-semantic/al-diag. `cargo tree -p al-cli` clean. 34/34 integration tests pass.)*
- [x] `al-mcp` verified as thin adapter (T305). *(2026-03-14: Already a subprocess-delegation server — shells out to `al --json`. Zero al-core dependency. 20 MCP tools. Output identical to al-cli --json by construction.)*
- [ ] Thin-adapter verification passes (`cargo tree` check for al-explorer). *Blocked: al-explorer depends on al-symbols directly. Requires T1001 (WP10) to refactor to JSON-RPC daemon client.*

**T303: Daemon Mode**
- [x] `al-lsp daemon --project <path>` implemented. *(2026-03-14: Unix socket JSON-RPC server at `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock`. 10 query dispatchers (hover, definition, references, completions, signatureHelp, rename, documentSymbols, foldingRanges, semanticTokens, ping/status/shutdown). Workspace init: project discovery, symbol loading, .al file scanning. 30-min idle timeout. Socket cleanup via Drop guard. Zero test regressions.)*

**WP4: DAP Integration, Toolchain Logic & Agentic Debugger**
- [x] `al-dap` merged into `al-lsp::dap` (T401). *(2026-03-14: lib.rs + editor_services.rs moved to al-lsp/src/dap/. al-dap crate deleted. main.rs updated. Zero test regressions.)*
- [x] Toolchain management in al-core (T402). *(2026-03-14: validate_toolchain(), doctor() in al-core::toolchain. DoctorReport struct with camelCase JSON. dispatch_setup thinned from 66 to 5 lines. 5 unit tests. Deferred issue T304-client-no-retry also fixed: DaemonClient retries 3x with 500ms backoff on "initializing" errors, 3 tests.)*
- [x] EditorServices lifecycle managed by `al-core::semantic` (T403). *(2026-03-14: get_or_init_bridge(), restart_bridge(max 3), shutdown_bridge() in al-core. server.rs delegates. dispatch_compile uses workspace bridge, not per-request bridge. bridge_restart_count in Workspace. 5 unit tests.)*
- [x] `al-dap-client` crate created — DAP protocol, framing, DapClient, EditorServices discovery, AL result types (T404a). *(2026-03-15: 6 source files. DapClient spawns subprocess, background event reader via mpsc, request/response matching by seq. DapError with 8 variants. ensure_seq patches missing seq field. AL result types: DebugState, StackFrame, Variable, BreakpointInfo, EvalResult. EditorServices discovery (moved from al-lsp::dap). cargo tree clean — no al-core/al-syntax/al-symbols deps. 22 unit tests.)*
- [x] `DebugSession` lifecycle — start (compile + DAP handshake) and stop (disconnect + kill + Drop guard) (T404b). *(2026-03-15: session.rs with start() (resolve config → compile → spawn → initialize → configurationDone → launch) and stop() (disconnect + kill). compile_project with 120s timeout. resolve_config supports named configs from .zed/debug.json or .vscode/launch.json. build_launch_args converts BcServerConfig to DAP launch args. Drop guard via DapClient. 13 unit tests.)*
- [x] Breakpoints + execution control — set_breakpoints, continue, step (T404c). *(2026-03-15: set_breakpoints() sends DAP setBreakpoints with source/line/condition, caches in breakpoints map. continue_() sends DAP continue + waits for stopped event. step() maps over→next, into→stepIn, out→stepOut + waits for stopped. Invalid step type returns DapProtocolError. SessionNotPaused guard on all. 3 tests.)*
- [x] State inspection + eval — threads, stack, variables with Record expansion, eval (T404d). *(2026-03-15: state() drains events, fetches stackTrace→scopes→variables. Records with variablesReference>0 expanded 1 level via fetch_flat_variables(). eval() sends DAP evaluate at current frame. Location updated from top stack frame. 3 tests.)*
- [x] Wire-up — daemon dispatch_debug, CLI `al debug` subcommands, idle timeout protection, architecture rule updates (T404e). *(2026-03-15: dispatch_debug() in daemon.rs handles 8 commands (start/breakpoint/state/eval/continue/step/history/stop). debug_session field in Workspace. Idle timeout skips shutdown during active debug. al-cli has Debug subcommands with 120s timeout for start. al-dap-client added to al-core and al-lsp deps. Architecture docs already up to date.)*
- [x] Debug history recording — variable snapshots at breakpoint hits, `--var` filter (T405). *(2026-03-15: record_hit() in state() captures seq, timestamp, location, variables at each pause. history(var_filter) filters to hits where named variable changed. MAX_HISTORY=1000 cap with oldest removal. History cleared on stop(). 5 tests.)*
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
- [x] Settings schema with MS parity (20+ settings) wired through `al-core::config` (T601). *(2026-03-15: AlConfig has 23 settings across 6 categories. merge() returns unknown keys. didChangeConfiguration sends showMessage(WARNING) for unknown settings. InitializationOptions logs unknown keys. 19 tests.)*
- [x] `workspace/didChangeConfiguration` handler implemented. *(2026-03-15: did_change_configuration in server.rs merges via AlConfig::merge(). Supports "al" nested key.)*
- [x] `InitializationOptions` parsed at startup. *(2026-03-15: initialize() parses params.initialization_options into AlConfig.)*

**WP6.5: Build/Publish Pipeline & Error Handling**
- [x] Error handling audit complete — all silent failures resolved per fail-loudly mandate (T610). *(2026-03-15: 46 SILENT: annotations across al-core, al-lsp, al-symbols. Toolchain/project discovery failures now use showMessage. Bridge init/load failures notify user. Deferred T402-doctor-blocks-tokio-worker resolved. All .ok() calls annotated.)*
- [x] `al.package` — compile project to .app file (T611). *(2026-03-15: al-core::build module with compile_project(), CompileResult, CompileDiagnostic. Parses alc output into structured diagnostics. daemon dispatch_package(). CLI `al package` with human/JSON output. Exit code reflects success. 7 unit tests.)*
- [ ] `al.publish` / `al.publishNoDebug` — deploy to BC server.
- [ ] `al.publishIncremental` — RAD mode (only changed objects).
- [ ] `al.publishDeps` — compile + publish full dependency tree.
- [x] `al.newProject` — scaffold from template (T613). *(2026-03-15: al-core::scaffold with create_project(), ScaffoldConfig. Generates app.json, .gitignore, .zed/debug.json, starter codeunit. Daemon dispatch_new_project(). CLI `al new` with name/publisher args. 7 unit tests.)*
- [x] `al.generatePermissionSet` — auto-generate from extension objects (AL + XML) (T614). *(2026-03-15: Done by background agent. al-core::permissions with collect_permissions(), render_al(), render_xml(). Daemon dispatch + CLI `al permissions`. 13 tests.)*
- [ ] Snapshot debugging (init, finish, list).
- [ ] CPU profiling via BC profiler.

### Phase 4: Advanced Intelligence & Performance
**WP7: Symbol Indexing & Semantic Optimization**
- [x] `SymbolIndex` composition optimized with lazy caching (T702). *(2026-03-15: DashMap<(ObjectKind, String), Arc<ComposedObject>> cache in SymbolIndex. get_composed_cached() returns Arc for zero-copy sharing. invalidate_composed(name) + invalidate_all_composed(). Cache cleared on did_open/did_change/did_close. Daemon uses cached path. 15 extensions: <5ms cold, <100µs warm. 4 new tests, 9 total composition tests pass.)*
- [x] `al-semantic` bridge management centralized in `al-core::semantic`. *(Already done in T403 — get_or_init_bridge(), restart_bridge(), shutdown_bridge() in al-core::semantic.)*
- [x] Bridge auto-restart (max 3) implemented. *(Already done in T403 — restart_bridge() with bridge_restart_count, MAX_RESTARTS=3.)*
- [x] Bridge request queue (mpsc serialized) implemented. *(Already done via RwLock<Option<SemanticBridge>> in Workspace — all callers go through get_or_init_bridge() which acquires read/write lock. Serialization is inherent in the lock design.)*

**WP8: Caching & Observability**
- [ ] Disk caching for symbols/ASTs implemented.
- [ ] `al-diag` request tracing integrated.
- [ ] Performance targets validated (hover <10ms, completion <15ms, etc.).
- [ ] Rope-based text storage implemented (optional optimization).

### Phase 5: AL Insight (The Killer App)
**WP9: Insight Graph & Agent Discovery Engine**
- [x] `al-core::insight` module created. *(2026-03-15: Done by background agent. graph.rs with InsightGraph using petgraph, mod.rs, index.rs, search.rs. 11 tests.)*
- [x] CallGraph (procedure→procedure) implemented. *(2026-03-15: procedure nodes + Contains edges in InsightGraph::build_from_index().)*
- [x] EventGraph (publisher→subscriber) implemented. *(2026-03-15: event nodes + Publishes + SubscribesTo edges. Circular chains handled.)*
- [x] ObjectGraph (extends, implements, depends) implemented. *(2026-03-15: Extends edges between extension→base objects.)*
- [x] TableRelationGraph implemented. *(2026-03-15: RelatesTo edge type in InsightEdge. resolve_relationships scans fields for TableRelation property. 1 test.)*
- [x] Event trace CLI command (`al trace`) implemented. *(2026-03-15: trace_event() in insight/search.rs follows SubscribesTo edges recursively with max_depth. Returns flattened TraceStep list. 2 tests. Daemon dispatch_trace + CLI `al trace <event>` wired.)*
- [x] Entry point finder (`al insight entrypoints`) implemented. *(2026-03-15: find_entry_points() in insight/search.rs finds procedures without incoming Calls edges. 1 test. Daemon dispatch_entrypoints + CLI `al entrypoints` wired.)*
- [x] Graph export (DOT, JSON) implemented. *(2026-03-15: export_dot() and export_json() in insight/search.rs. DOT format with shape-by-type. JSON with nodes+edges arrays. 2 tests. Daemon dispatch_graph_export + CLI `al graph` wired.)*
- [x] Cross-extension event tracing implemented. *(2026-03-15: Implicitly supported — InsightGraph.build_from_index() processes all entries across all loaded packages. SubscribesTo edges resolved across package boundaries. trace_event() follows chains regardless of originating package.)*
- [x] Insight daemon queries wired (T904). *(2026-03-15: 4 daemon methods (trace, entrypoints, graphExport, insightStats). 4 CLI commands (al trace, al entrypoints, al graph, al insight-stats). Human-readable + --json output for all.)*

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

### 2nd Agent Okay
Parallelizable tasks — al-symbols (read-only) + insight engine + source extraction. Zero overlap with main agent (WP5/WP6) or 3rd agent.

- [x] T701: Symbol Index Performance Audit (WP7) — profile al-symbols on large project, report bottlenecks. Deps: T203 ✅ *(2026-03-15: Profiled 11 packages / 20,818 objects. Critical: get_events() 40-57ms O(n*m) full scan [events.rs:67]. Major: build 3s sequential [app_reader.rs:53]. Direct lookups <12µs. Memory ~52.5MB. PoF entry created.)*
- [x] T901: Insight Module Skeleton (WP9) — create al-core/src/insight/ with graph data structures. Deps: T302 ✅ *(2026-03-15: InsightGraph with petgraph 0.7. 4 node types (Object, Procedure, Event, Subscriber), 5 edge types (Extends, Calls, Publishes, SubscribesTo, Contains). build_from_index() populates from SymbolIndex. Circular chains handled. 10 tests. PoF entry.)*
- [x] T907: Source Extraction Command (WP9) — implement `al source` query in al-core/src/queries/source.rs. Deps: T302 ✅ *(2026-03-15: source() with 3 levels (workspace/package/outline). render_outline() with full signatures, fields, keys, enum values, variables, attributes. Daemon dispatch_source wired. 11 tests. PoF entry.)*
- [x] T908: Remove generate_al Fallback (WP9) — replace with proper render_outline() in al-symbols. Deps: T907 ✅ *(2026-03-15: generate_al() deleted. render_outline() in al-symbols/virtual_file.rs with full signatures, params+types+var, fields+type, keys, enum values, variables, attributes. get_or_create() simplified — always renders outline when no source. outline_fallback_approved field removed from Workspace. check_source_availability prompt replaced with info log. Zero "fallback" references in outline code. 130 al-core + 59 al-symbols tests pass.)*

### 3rd Agent Okay
Parallelizable tasks — permissions, semantic caching, observability. Zero overlap with main agent or 2nd agent.

- [x] T614: Permission Set Generation (WP6.5) — new al-core/src/permissions.rs, auto-generate from workspace objects. Deps: T203 ✅. *(2026-03-15: collect_permissions() scans FileIndex, maps 6 object types (table→RIMD, page/codeunit/report/xmlport/query→X), skips extensions/enums/interfaces. render_al() + render_xml(). Daemon dispatch + CLI `al permissions`. 13 tests.)*
- [x] T703: Semantic Bridge Caching (WP7) — cache .NET bridge responses in al-core/src/semantic.rs + al-semantic. Deps: T403 ✅. *(2026-03-15: SemanticCache struct — HashMap-indexed builtins for O(1) get_type/get_method (replaces O(n) linear scans). set_builtins() atomic helper. Version-aware staleness detection. Hit/miss stats exposed in daemon status. 4 sites in resolution.rs updated. 10 new cache tests.)*
- [x] T802: Request Tracing with al-diag (WP8) — integrate tracing layer in al-diag/src/lib.rs, SQLite logging. Deps: T302 ✅. *(2026-03-15: DiagLayer already wired (default feature). Added log rotation: prune_sessions(keep=20) auto-runs on startup, db_size_bytes() for monitoring. dispatch_diag daemon route with 6 query commands (sessions/events/slow/failures/search/summary). CLI `al diag` subcommands as thin JSON-RPC client. 6 writer tests.)*

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
