# Zed AL Extension — Master Plan

44 tasks across WP0–WP11. Each task has an ID, file ownership, dependencies, and pass/fail criteria.

To begin work, run `/start-work`.

---

## Critical Path

The minimum task sequence to reach a working state where al-lsp serves both Zed (stdio) and daemon clients (CLI/Explorer/MCP) via al-core:

```
T001 (WP0 Harness bootstrap)
  |
T101 -> T102 -> T103 -> T104  (WP1: al-core skeleton + discovery migration)
  |                       |
T201 -> T202 -> T203      |   (WP2: DocumentStore + parse cache in al-core)
                  |       |
                  v       v
               T301 -> T302 -> T303 -> T304 -> T305  (WP3: query migration + daemon + thin adapters)
                                                  |
                                         T401 -> T402  (WP4: DAP fold + toolchain)
                                                  |
                                         T403 -> T404 -> T405  (WP4: agentic debugger)
                                                  |
                                T501 -> T502 -> T504 -> T505  (WP5: assets + tokens + analyzers)
                                                  |
                                         T601 -> T602 -> T603  (WP6: WASM + settings)
                                                  |
                                T610 -> T611 -> T612  (WP6.5: error handling + build/publish)
                                         |
                                T613  T614  T615  T616  (WP6.5: scaffold, perms, snapshots, profiling)
```

**Minimum viable loop** (Zed works end-to-end with new architecture):
- T001 through T305 = **17 tasks** across WP0-WP3.

**Full production path** adds WP4-WP11 = **~40 total tasks**.

### Critical Dependencies (cross-WP)
- T301 (query migration) requires T104 (al-core compiles with discovery) AND T203 (DocumentStore in al-core).
- T303 (daemon mode) is WP3 but unblocks ALL thin adapter work (T304, T305, T601, T1001).
- T501 (grammar sync) is independent of WP1-WP4; can run in parallel.
- WP7-WP10 are additive and can be parallelized after WP3.

---

# Execution Phases & Work Packages

## Phase 0: The Adversarial Foundation
**WP0: Project Constitution & The Adversarial Harness**
- **Goal**: Establish the project's rules and build its "Immune System."
- **Scope**:
    - Create `CLAUDE.md` as the project's rulebook.
    - Evaluate and define required Claude Skills or Agents for the workspace.
    - Build an LSP simulator with high Zed-fidelity in `al-test-harness`.
    - Implement the **Centralized Evidence Log** system in TOML.
- **Gating**: No other work packages may start until WP0 is verified.

### Recommended Task Sequence

**T001: Harness Baseline Verification**
- **Name**: Verify existing test harness compiles and passes
- **Files**: `crates/al-test-harness/src/lib.rs`, `crates/al-test-harness/tests/*.rs`
- **Dependencies**: None
- **Pass Criteria**: `cargo test -p al-test-harness` compiles. All existing tests either pass or are documented as known-broken with specific failure reasons.
- **Fail Criteria**: Harness cannot spawn al-lsp binary, or tests panic without clear error messages.
- **Estimated Complexity**: Low

**T002: Evidence Log Skeleton**
- **Name**: Create proof_of_functionality.toml structure
- **Files**: `docs/proof_of_functionality.toml`
- **Dependencies**: T001
- **Pass Criteria**: TOML file parses. Contains section headers for every WP. Template entry demonstrates adversarial/fidelity dual-pass format.
- **Fail Criteria**: File does not parse as valid TOML, or lacks the adversarial pass template.
- **Estimated Complexity**: Low

**T003: Harness Daemon-Mode Extension**
- **Name**: Extend test harness to support future daemon-mode testing
- **Files**: `crates/al-test-harness/src/lib.rs`, `crates/al-test-harness/src/protocol.rs`
- **Dependencies**: T001, T303 (full implementation deferred until daemon exists; this task adds the trait/interface)
- **Pass Criteria**: `LspClient` trait is abstracted so tests can run against stdio OR socket transport. Existing stdio tests unchanged.
- **Fail Criteria**: Abstraction forces rewrite of existing tests, or introduces runtime overhead on stdio path.
- **Estimated Complexity**: Medium

---

**WPX: Continuous Adversarial Evolution (Persistent)**
- **Goal**: A sub-agent is **permanently deployed** to break the code and interrogate the log.
- **Scope**: Identify where the harness lacks Zed-fidelity. Interrogate the `proof_of_functionality.toml` for tasks with insufficient negative testing.
- **Feedback**: Discovered gaps are reported to the PM as immediate blocking issues.

---

## Phase 1: Foundation & Core Refactor
**WP1: al-core Skeleton & Discovery Migration**
- **Goal**: Establish the central "brain" and project discovery logic.
- **Scope**: Create `al-core`, implement the `Workspace` state container, and migrate `al-discovery` (project/toolchain/launch) into `al-core`.
- **Zed Impact**: Provides the foundation for project loading and toolchain detection in Zed.
- **Agent Impact**: Accurate project discovery for CLI/MCP queries.

### Recommended Task Sequence

**T101: al-core Crate Skeleton**
- **Name**: Create al-core crate with empty module structure
- **Files**: `crates/al-core/Cargo.toml`, `crates/al-core/src/lib.rs`, `crates/al-core/src/workspace.rs`, `crates/al-core/src/errors.rs`
- **Dependencies**: None
- **Pass Criteria**: `cargo check -p al-core` succeeds. `Workspace` struct exists (empty). `al-core` is added to workspace `Cargo.toml`. `al-lsp` has `al-core` as a dependency in its `Cargo.toml`.
- **Fail Criteria**: Circular dependency introduced. Any analysis library depends on al-core.
- **Estimated Complexity**: Low

**T102: Project Discovery Migration**
- **Name**: Move project/toolchain/launch logic from al-discovery to al-core
- **Files**: `crates/al-core/src/project.rs`, `crates/al-core/src/toolchain.rs`, `crates/al-core/src/launch.rs`, `crates/al-discovery/src/lib.rs`, `crates/al-discovery/src/launch.rs`
- **Dependencies**: T101
- **Pass Criteria**: `al-core::project::find_project()` and `al-core::toolchain::find_toolchain()` work identically to existing `al_discovery` functions. All callers in `al-lsp` updated. `al-discovery` crate marked deprecated or emptied.
- **Fail Criteria**: Any behavior change in project discovery (different paths found, different error messages). Any test regression.
- **Estimated Complexity**: Medium

**T103: JSON-RPC Bridge Types Migration**
- **Name**: Move shared JSON-RPC types from al-discovery to al-core
- **Files**: `crates/al-core/src/jsonrpc.rs`, `crates/al-discovery/src/jsonrpc.rs`
- **Dependencies**: T101
- **Pass Criteria**: `al-core::jsonrpc` exports `Request`, `Response`, `RpcError`, and error codes. `al-semantic` and `al-dap` import from al-core (or a re-export) instead of al-discovery. All 6 existing JSON-RPC tests pass.
- **Fail Criteria**: Import cycle. Tests broken by moved types.
- **Estimated Complexity**: Low

**T104: Remove al-discovery Crate**
- **Name**: Delete al-discovery, update all references
- **Files**: `crates/al-discovery/` (delete), `Cargo.toml` (workspace members), all `Cargo.toml` files that depended on al-discovery
- **Dependencies**: T102, T103
- **Pass Criteria**: `cargo check --workspace` succeeds with no references to `al-discovery`. Workspace member list has 11 crates (al-core replaces al-discovery).
- **Fail Criteria**: Any crate still imports `al_discovery`. Build fails.
- **Estimated Complexity**: Low

---

**WP2: Workspace State & Document Management**
- **Goal**: Centralize document handling and parse caching.
- **Scope**: Move `DocumentStore` from `al-lsp` to `al-core`. Implement rope-based text and incremental parse tree caching in the core.
- **Zed Impact**: Faster file opening and typing responsiveness.
- **Agent Impact**: Stable, cached state for multi-turn agent interactions.

### Recommended Task Sequence

**T201: DocumentStore Migration**
- **Name**: Move DocumentStore from al-lsp to al-core
- **Files**: `crates/al-core/src/documents.rs`, `crates/al-lsp/src/document.rs` (delete or stub), `crates/al-lsp/src/server.rs`
- **Dependencies**: T101
- **Pass Criteria**: `al-core::documents::DocumentStore` compiles with identical API. `al-lsp` imports it from al-core. All existing document operations (open, close, get_text, apply_changes) work. Harness tests pass.
- **Fail Criteria**: Any change to DocumentStore's public API that breaks al-lsp. Text content differs after apply_changes.
- **Estimated Complexity**: Medium

**T202: Parse Cache in al-core**
- **Name**: Move parse tree caching from al-lsp to al-core
- **Files**: `crates/al-core/src/parsing.rs`, `crates/al-lsp/src/parsing.rs`
- **Dependencies**: T201
- **Pass Criteria**: `al-core::parsing::get_or_parse()` returns `(String, Tree)` given a URI and the Workspace. `al-lsp::parsing` becomes a thin delegate. Parse trees are cached in DocumentStore (or alongside it). No double-parsing on repeated hover calls.
- **Fail Criteria**: Parse cache misses where it previously hit. Memory leak from unbounded tree cache.
- **Estimated Complexity**: Medium

**T203: Workspace State Container**
- **Name**: Wire Workspace struct to own DocumentStore, SymbolIndex, project info
- **Files**: `crates/al-core/src/workspace.rs`, `crates/al-core/src/lib.rs`
- **Dependencies**: T201, T202, T104
- **Pass Criteria**: `Workspace` owns: `DocumentStore`, `SymbolIndex`, `AlProject` (Option), `AlToolchain` (Option), `SemanticBridge` handle (Option). `al-lsp::AlServer` holds a single `Workspace` instead of separate fields. All harness tests pass.
- **Fail Criteria**: AlServer still owns DocumentStore or SymbolIndex directly. Any field duplication between Workspace and AlServer.
- **Estimated Complexity**: High

---

## Phase 2: Binary Thinning & Language Parity
**WP3: LSP & CLI Thin Adapters (Query Engine)**
- **Goal**: Move query logic to the core and thin out the binaries.
- **Scope**: Migrate hover, definition, completions, and references to `al-core::queries`. Refactor `al-lsp` (transport) and `al-cli` (agent discovery) to use these queries.
- **Zed Impact**: Reliable, standard LSP behavior.
- **Agent Impact**: Surgical, context-dense discovery calls.

### Recommended Task Sequence

**T301: Query Module Skeleton in al-core**
- **Name**: Create queries module structure with trait/API definitions
- **Files**: `crates/al-core/src/queries/mod.rs`, `crates/al-core/src/queries/hover.rs`, `crates/al-core/src/queries/definition.rs`, `crates/al-core/src/queries/completions.rs`, `crates/al-core/src/queries/references.rs`, `crates/al-core/src/queries/signature.rs`, `crates/al-core/src/queries/rename.rs`, `crates/al-core/src/queries/semantic_tokens.rs`, `crates/al-core/src/queries/folding.rs`, `crates/al-core/src/queries/inlay_hints.rs`, `crates/al-core/src/queries/code_actions.rs`, `crates/al-core/src/queries/symbols.rs`
- **Dependencies**: T203
- **Pass Criteria**: Each query function takes `&Workspace` (not `&AlServer`) and returns transport-agnostic result types. Module compiles. Functions can initially delegate to existing al-lsp code via temporary internal imports.
- **Fail Criteria**: Query functions require tower-lsp types. Any LSP-specific type in al-core's public API.
- **Estimated Complexity**: High

**T302: Query Logic Migration**
- **Name**: Move hover, definition, completions, references, resolution logic to al-core
- **Files**: `crates/al-core/src/queries/*.rs`, `crates/al-core/src/resolution.rs`, `crates/al-lsp/src/hover.rs`, `crates/al-lsp/src/definition.rs`, `crates/al-lsp/src/completions.rs`, `crates/al-lsp/src/resolution.rs`, `crates/al-lsp/src/handlers.rs`
- **Dependencies**: T301
- **Pass Criteria**: All query logic lives in al-core. `al-lsp` handler functions are 5-15 lines each: extract params from LSP types, call `al_core::queries::*`, convert result to LSP response. `resolution.rs` (1295 lines) moves entirely to al-core. All harness tests pass unchanged.
- **Fail Criteria**: Any business logic remaining in al-lsp handlers (beyond type conversion). Harness test regression.
- **Estimated Complexity**: High

**T303: Daemon Mode for al-lsp**
- **Name**: Add Unix socket daemon mode to al-lsp binary
- **Files**: `crates/al-lsp/src/main.rs`, `crates/al-lsp/src/daemon.rs` (new)
- **Dependencies**: T203 (Workspace must exist so daemon can hold it)
- **Pass Criteria**: `al-lsp daemon --project /path/to/project` listens on a Unix socket at a deterministic path (e.g., `/run/user/$UID/al-lsp/<project-hash>.sock`). Accepts JSON-RPC requests, routes to `al-core::queries`, returns JSON-RPC responses. Auto-shutdown after 30 minutes of idle. Socket file cleaned up on shutdown.
- **Fail Criteria**: Daemon crashes on second connection. Socket file leaks on crash. No idle timeout. Response format differs from what a JSON-RPC client expects.
- **Estimated Complexity**: High

**T304: al-cli as JSON-RPC Client**
- **Name**: Rewrite al-cli to connect to al-lsp daemon instead of calling al-core directly
- **Files**: `crates/al-cli/src/main.rs` (rewrite), `crates/al-cli/src/client.rs` (new), `crates/al-cli/src/commands/mod.rs` (new), `crates/al-cli/src/output.rs` (new), `crates/al-cli/src/project.rs` (new), `crates/al-cli/Cargo.toml`
- **Dependencies**: T303
- **Pass Criteria**: `al-cli` has ZERO compile-time dependency on al-core, al-syntax, al-symbols, al-semantic, or al-discovery. It connects to the daemon socket, sends JSON-RPC, formats output. Auto-starts daemon if not running. All 27 existing CLI commands produce identical output. `cargo tree -p al-cli` shows no path to al-core.
- **Fail Criteria**: Any import of al-core in al-cli. CLI command returns different output than before. Daemon not auto-started.
- **Estimated Complexity**: High

**T305: al-mcp as JSON-RPC Client**
- **Name**: Rewrite al-mcp to connect to al-lsp daemon
- **Files**: `crates/al-mcp/src/main.rs` (rewrite), `crates/al-mcp/src/tools.rs` (new), `crates/al-mcp/Cargo.toml`
- **Dependencies**: T303
- **Pass Criteria**: `al-mcp` connects to al-lsp daemon via Unix socket. All MCP tools produce identical results to al-cli `--json` output. No compile-time dependency on al-core.
- **Fail Criteria**: Any MCP tool returns different data than the equivalent CLI command. Import of al-core in al-mcp.
- **Estimated Complexity**: Medium

---

**WP4: DAP Integration, Toolchain Logic & Agentic Debugger**
- **Goal**: Fold debugging and toolchain management into the core/LSP. Expose headless debug control for agentic debugging.
- **Scope**: Merge `al-dap` into `al-lsp::dap`. Refactor EditorServices management to use `al-core::toolchain`. Implement `al debug` CLI/MCP for headless DAP control with variable inspection, expression evaluation, and debug history recording.
- **Zed Impact**: Seamless "Publish & Debug" from Zed.
- **Agent Impact**: Full agentic debugging — AI agents can start debug sessions, set conditional breakpoints, step through code, inspect variables, evaluate expressions, and review debug history. All via CLI/MCP with no UI required.

### Recommended Task Sequence

**T401: DAP Fold into al-lsp**
- **Name**: Merge al-dap crate into al-lsp as internal module
- **Files**: `crates/al-lsp/src/dap.rs` (new, from `crates/al-dap/src/*.rs`), `crates/al-dap/` (delete), `Cargo.toml` (workspace members)
- **Dependencies**: T302 (queries must be in al-core so al-lsp is already thinned)
- **Pass Criteria**: `al-lsp::dap` contains all DAP proxy logic. `al-dap` crate removed from workspace. DAP launch/attach sequences work (tested via harness or manual Zed session). No functionality lost.
- **Fail Criteria**: DAP session fails to start. Compiler management (EditorServices.Host) broken. Build fails.
- **Estimated Complexity**: Medium

**T402: Toolchain Management in al-core**
- **Name**: Move toolchain validation, setup, and doctor logic to al-core
- **Files**: `crates/al-core/src/toolchain.rs` (expand), `crates/al-lsp/src/workspace.rs`
- **Dependencies**: T104 (discovery already migrated), T203
- **Pass Criteria**: `al-core::toolchain` provides `find_toolchain()`, `validate_toolchain()`, `setup_toolchain()`, and `doctor()` functions. CLI `setup` and `doctor` commands work via daemon. Bridge initialization uses al-core toolchain info.
- **Fail Criteria**: Toolchain detection gives different results than before migration. Setup command fails.
- **Estimated Complexity**: Medium

**T403: EditorServices Lifecycle in al-core**
- **Name**: Move compiler process management into al-core::semantic
- **Files**: `crates/al-core/src/semantic.rs`, `crates/al-lsp/src/dap.rs`
- **Dependencies**: T401, T402
- **Pass Criteria**: `al-core::semantic` owns the `SemanticBridge` process lifecycle (start, restart on crash, shutdown). `al-lsp::dap` uses the bridge from Workspace. Single process instance shared between LSP semantic queries and DAP.
- **Fail Criteria**: Two bridge processes spawned simultaneously. Bridge crash not detected or recovered.
- **Estimated Complexity**: High

**T404: Agentic Debugger — Headless DAP Control via Daemon**
- **Name**: Expose DAP session control through daemon JSON-RPC so CLI/MCP can drive debugging headlessly
- **Files**: `crates/al-core/src/debug.rs` (new), `crates/al-lsp/src/daemon.rs`, `crates/al-cli/src/main.rs`
- **Dependencies**: T401 (DAP in al-lsp), T403 (EditorServices lifecycle), T303 (daemon mode)
- **Pass Criteria**: `al debug start` compiles project and launches debug session via EditorServices.Host. `al debug breakpoint` sets/clears breakpoints with optional conditions. `al debug state` returns current location, call stack, and all in-scope variables with values (Record types expand to show field values). `al debug eval` evaluates expressions at current frame. `al debug step [over|into|out]` controls execution. `al debug continue` resumes to next breakpoint. `al debug stop` cleanly terminates session. MCP `al/debug` produces identical results. All schemas match `docs/agentic-schemas.md`. Scenarios 9-11 from `docs/agent-scenarios.md` pass.
- **Fail Criteria**: Debug session requires UI interaction. Variables not readable when paused. Conditional breakpoints ignored. Session not cleaned up on stop (orphaned EditorServices.Host process).
- **Estimated Complexity**: High

**T405: Debug History Recording**
- **Name**: Record variable snapshots at each breakpoint hit for post-mortem analysis
- **Files**: `crates/al-core/src/debug.rs`
- **Dependencies**: T404
- **Pass Criteria**: Every breakpoint hit records: sequence number, timestamp, location (file + line + procedure), and all in-scope variable values. `al debug history` returns full recording. `al debug history --var <name>` filters to only hits where that variable's value changed from the previous hit. History persists for the session lifetime and is cleared on `stop`.
- **Fail Criteria**: History missing variable snapshots. `--var` filter returns hits where the variable didn't change. History grows unbounded (must cap at reasonable limit, e.g., 1000 hits).
- **Estimated Complexity**: Medium

---

## Phase 3: Assets & Zed Integration
**WP5: Language Config & Asset Parity**
- **Goal**: High-fidelity Zed integration and snippet parity.
- **Scope**: Sync grammar, import 23 snippet files, and implement `highlights.scm`, `runnables.scm`, and `brackets.scm`. Implement all Zed tasks (AL: Go, AL: Publish, etc.).
- **Zed Impact**: Beautiful highlighting, snippets, and task-based workflows.
- **Agent Impact**: Parity in available commands and scaffold generation.

### Recommended Task Sequence

**T500: tree-sitter-al: CodeAnalysis.dll Extraction**
- **Name**: Replace TextMate-based extraction with CodeAnalysis.dll reflection in tree-sitter-al
- **Files**: `tree-sitter-al/generator/tools/al-extract/` (new C# project), `tree-sitter-al/generator/tools/al-gen/src/main.rs`, `tree-sitter-al/data/al-language-data.json` (new, generated)
- **Dependencies**: ALTool must be installed
- **Pass Criteria**: New `al-extract` C# tool reflects on CodeAnalysis.dll and outputs `data/al-language-data.json` containing: all 159 keywords (classified by category via SyntaxFacts), 313 properties (with per-object-type validity), 127+ triggers (with per-object-type validity), 158 builtin types, token classification, preprocessor keywords. al-gen reads this JSON instead of TextMate grammar. All references to TextMate, `alsyntax.tmlanguage`, `find_al_extension()`, and VS Code/Cursor extension paths removed from al-gen. Generated grammar.js, parser.c, scanner.c, keywords.c, and all query files committed. Grammar passes all existing fixture tests AND real-world repo tests.
- **Fail Criteria**: Any reference to TextMate, VS Code, or Cursor remains in tree-sitter-al. Generated grammar differs in parse behavior from current grammar on valid AL files (regression). Missing keywords that CodeAnalysis.dll exposes.
- **Estimated Complexity**: High

**T501: Grammar Regeneration and Highlights**
- **Name**: Regenerate grammar from CodeAnalysis.dll data, verify highlights coverage
- **Files**: `tree-sitter-al/grammar.js`, `tree-sitter-al/queries/highlights.scm`, `languages/al/highlights.scm`, `grammars/` symlink
- **Dependencies**: T500
- **Pass Criteria**: `tree-sitter-al` submodule updated to CodeAnalysis.dll-based generation. `highlights.scm` covers all node types produced by the grammar. No "unknown node" warnings in Zed. All 159 keywords classified. Property keywords validated per object context. Preprocessor directives parsed correctly.
- **Fail Criteria**: Zed shows unstyled AL keywords. Grammar parse errors on valid AL files. Missing keyword categories.
- **Estimated Complexity**: Medium

**T502: Snippets and Brackets**
- **Name**: Generate snippets from extracted language data, implement bracket matching
- **Files**: `tree-sitter-al/data/snippets/` (generated from language data), `snippets/*.json`, `languages/al/brackets.scm`
- **Dependencies**: T500 (language data must be extracted), T501
- **Pass Criteria**: Snippet templates generated from extracted property and trigger data for each object type. All AL object types covered (table, page, codeunit, report, query, xmlport, enum, interface, permissionset, profile, entitlement, controladdin). Bracket matching works for begin/end, if/then, case/of, repeat/until. Snippet expansion in Zed produces valid AL code. No dependency on MS VS Code extension snippet files.
- **Fail Criteria**: Snippet triggers conflict with each other. Bracket matching pairs begin with wrong end. Snippets hardcode properties instead of using extracted data.
- **Estimated Complexity**: Medium

**T503: Runnables and Zed Tasks**
- **Name**: Implement runnables.scm and Zed task definitions
- **Files**: `languages/al/runnables.scm`, `.zed/tasks.json`
- **Dependencies**: T501
- **Pass Criteria**: "AL: Publish" task runs `alc` compilation. "AL: Debug" task launches DAP session. Runnables detect test codeunits and individual test functions. Tasks appear in Zed's task picker.
- **Fail Criteria**: Tasks present but fail to execute. Runnables match non-test procedures.
- **Estimated Complexity**: Medium

**T504: Semantic Token Expansion to MS Parity**
- **Name**: Expand semantic token types from 16 to 31 to match MS extension
- **Files**: `crates/al-syntax/src/tokens.rs`, `languages/al/semantic_token_rules.json`, `crates/al-core/src/queries/semantic_tokens.rs`
- **Dependencies**: T301, T501
- **Pass Criteria**: All 15 missing token types added (see `docs/architecture.md` "Must add" table): `builtinFunction`, `globalVariable`, `localVariable`, `tableField`, `pageControl`, `pageAction`, `triggerName`, `preprocessorKeyword`, `excludedCode`, `tableKey`, `tableFieldGroup`, `reportLabel`, `eventCreation`, `eventSubscription`, `returnParameter`. Classifier logic in `classify_node()` updated to emit these types. `semantic_token_rules.json` maps all custom types to Zed theme scopes. All types render with distinct colors in at least 3 Zed themes. No token type maps to default/plain text.
- **Fail Criteria**: More than 2 token types visually indistinguishable. Missing type falls back to generic `variable` or `keyword`. Existing token classifications regress.
- **Estimated Complexity**: High

**T505: All Analyzers Support**
- **Name**: Enable AppSourceCop, UICop, PerTenantCop in the bridge alongside CodeCop
- **Files**: `crates/al-semantic/bridge/Bridge.cs`, `crates/al-semantic/src/lib.rs`, `crates/al-core/src/config.rs`
- **Dependencies**: T601 (config must support `al.codeAnalyzers` setting)
- **Pass Criteria**: `AnalyzeRequest` accepts analyzer list. Bridge loads and runs all 4 analyzer DLLs. Each analyzer can be individually enabled/disabled via `al.codeAnalyzers` setting. Diagnostics from each analyzer are tagged with their source. `al lint --analyzers CodeCop,AppSourceCop` works. Default: CodeCop only (matching MS default).
- **Fail Criteria**: Analyzer DLL not found produces silent empty results instead of error. Analyzer-specific diagnostics not distinguishable from each other.
- **Estimated Complexity**: Medium

---

**WP6: WASM Entry & Settings Implementation**
- **Goal**: Clean up the user-facing extension entry point.
- **Scope**: Refactor `zed-al` (WASM) to remove fallbacks. Implement full settings map in `al-core::config`.
- **Zed Impact**: Explicit, predictable settings behavior; no "auto-download" surprises.

### Recommended Task Sequence

**T601: Settings Schema in al-core**
- **Name**: Define unified configuration model with MS parity
- **Files**: `crates/al-core/src/config.rs`
- **Dependencies**: T203
- **Pass Criteria**: `al-core::config::AlConfig` struct covers all settings from `docs/architecture.md` Configuration table (20+ settings). Includes: `enableCodeAnalysis`, `backgroundCodeAnalysis`, `codeAnalyzers` (list of analyzer names), `enableCodeActions`, `enableExternalRulesets`, `ruleSetPath`, `packageCachePath`, `nugetFeeds`, `editorServicesPath`, `editorServicesLogLevel`, `compilationOptions`, `incrementalBuild`, `rootNamespace`, `symbolsCountryRegion`, `useOnlyCustomFeeds`, inlay hint toggles, `semanticFolding.enabled`. Serializable to/from JSON. Default values match MS extension defaults. `workspace/didChangeConfiguration` handler parses incoming settings into AlConfig. `InitializationOptions` parsed at startup. Invalid settings reported to user via `showMessage(Error)`.
- **Fail Criteria**: Setting has effect in one mode (LSP) but not another (CLI). Undocumented default. Invalid setting silently ignored.
- **Estimated Complexity**: Medium

**T602: WASM Entry Cleanup**
- **Name**: Refactor zed-al to use explicit settings, remove auto-download fallbacks
- **Files**: `src/lib.rs` (workspace root, WASM entry), `extension.toml`
- **Dependencies**: T601, T303 (daemon must exist for zed-al to spawn al-lsp)
- **Pass Criteria**: `zed-al` extension entry spawns `al-lsp` with explicit binary path from settings. No auto-download of anything without user action. Settings changes in Zed propagate to running al-lsp instance. Extension installs cleanly in Zed.
- **Fail Criteria**: Extension silently downloads packages on first open. Settings change requires Zed restart.
- **Estimated Complexity**: Medium

**T603: Settings Propagation via Daemon**
- **Name**: Implement workspace/didChangeConfiguration for daemon mode
- **Files**: `crates/al-lsp/src/daemon.rs`, `crates/al-core/src/config.rs`
- **Dependencies**: T601, T303
- **Pass Criteria**: CLI can send `workspace/didChangeConfiguration` to running daemon. Settings change (e.g., enable a lint rule) takes effect immediately without restart. Daemon persists settings to disk for cold start.
- **Fail Criteria**: Settings change requires daemon restart. Settings lost on daemon auto-shutdown.
- **Estimated Complexity**: Medium

**WP6.5: Build/Publish Pipeline & Error Handling**
- **Goal**: Implement the full build/publish lifecycle and eliminate silent failures.
- **Scope**: Package, publish (standard + RAD), project scaffolding, permission set generation, snapshot debugging. Audit and fix all silent error handling.
- **Zed Impact**: Full development lifecycle without leaving Zed — compile, publish, debug, profile.
- **Agent Impact**: Agentic build/deploy (compile, publish, verify via CLI/MCP).

### Recommended Task Sequence

**T610: Error Handling Audit & Fix**
- **Name**: Eliminate all silent failures per fail-loudly mandate
- **Files**: All `.rs` files in `crates/al-lsp/`, `crates/al-symbols/`, `crates/al-semantic/`, `crates/al-discovery/`
- **Dependencies**: T203 (Workspace must exist for centralized error reporting)
- **Pass Criteria**: Every `.ok()?` that discards a meaningful error replaced with proper error propagation + user notification via `showMessage`. Bridge init failure notifies user with install instructions. Package download failures shown via `showMessage(Error)`. Toolchain discovery failures explain what was searched and where. Bridge timeouts notify user on repeat occurrence. No `warn!()` or `debug!()` used as sole error reporting for user-affecting issues. Each intentional silent handling has `// SILENT: <reason>` comment. List of all 13 critical patterns from audit (see architecture.md Error Handling Mandate) resolved.
- **Fail Criteria**: Any `.ok()?` without a `// SILENT:` justification comment. User-affecting error reported only to log, not to user. Bridge unavailable with no user-visible indication.
- **Estimated Complexity**: Medium

**T611: Package Command (Compile to .app)**
- **Name**: Implement `al.package` — compile project into .app file
- **Files**: `crates/al-core/src/build.rs` (new), `crates/al-lsp/src/server.rs`, `crates/al-cli/src/main.rs`
- **Dependencies**: T402 (toolchain in al-core), T601 (config for compilation options)
- **Pass Criteria**: `al package` CLI command compiles project via `alc` and produces .app file. `al.package` LSP execute command works. Compilation errors returned as LSP diagnostics. `al package --deps` compiles full dependency tree. Exit code reflects success/failure.
- **Fail Criteria**: Compilation error not surfaced to user. .app produced despite errors.
- **Estimated Complexity**: Medium

**T612: Publish Commands (Standard + RAD)**
- **Name**: Implement publish to BC server — standard and incremental (RAD)
- **Files**: `crates/al-core/src/publish.rs` (new), `crates/al-core/src/bc_client.rs` (new), `crates/al-lsp/src/server.rs`, `crates/al-cli/src/main.rs`
- **Dependencies**: T611 (package must work), T601 (config for server details)
- **Pass Criteria**: `al publish` compiles + deploys .app to BC server from debug.json/launch.json config. `al publish --no-debug` deploys without attaching debugger. `al publish --incremental` uses RAD API to deploy only changed objects. `al publish --deps` publishes full dependency tree. Server authentication works (OAuth, Windows, NavUserPassword). All variants available as LSP execute commands and Zed tasks.
- **Fail Criteria**: Publish succeeds but returns no confirmation. Auth failure gives generic error instead of specific auth guidance. RAD publish sends full app instead of delta.
- **Estimated Complexity**: High

**T613: Project Scaffolding**
- **Name**: Implement `al.newProject` — scaffold new AL project from template
- **Files**: `crates/al-core/src/scaffold.rs` (new), `crates/al-cli/src/main.rs`
- **Dependencies**: T601
- **Pass Criteria**: `al new` creates a new AL project with: app.json (prompted or defaulted), .gitignore, launch.json/debug.json, src/ directory. Templates for common project types (app, test, library). `al.newProject` LSP command triggers via Zed.
- **Fail Criteria**: Generated app.json has invalid format. Template produces non-compilable project.
- **Estimated Complexity**: Low

**T614: Permission Set Generation**
- **Name**: Implement `al.generatePermissionSet` — auto-generate from extension objects
- **Files**: `crates/al-core/src/permissions.rs` (new), `crates/al-cli/src/main.rs`
- **Dependencies**: T203 (workspace index for object enumeration)
- **Pass Criteria**: `al permissions --al` generates AL PermissionSet object covering all tables, pages, reports, codeunits in workspace. `al permissions --xml` generates XML format. Permissions derived from actual object types (RIMD for tables, X for codeunits, etc.). Both LSP command and CLI available.
- **Fail Criteria**: Missing an object from the workspace. Wrong permission type for object kind.
- **Estimated Complexity**: Medium

**T615: Snapshot Debugging**
- **Name**: Implement snapshot debugging init/finish/list via BC server API
- **Files**: `crates/al-core/src/snapshots.rs` (new), `crates/al-cli/src/main.rs`
- **Dependencies**: T612 (BC server client must exist)
- **Pass Criteria**: `al snapshot init` starts snapshot debugging on BC server. `al snapshot finish` downloads snapshot data. `al snapshot list` shows active snapshots. Snapshot data stored locally for later inspection.
- **Fail Criteria**: Snapshot init succeeds but no indication to user. Snapshot data download fails silently.
- **Estimated Complexity**: High

**T616: CPU Profiling**
- **Name**: Implement CPU profile generation via BC profiler
- **Files**: `crates/al-core/src/profiler.rs` (new), `crates/al-cli/src/main.rs`
- **Dependencies**: T612 (BC server client)
- **Pass Criteria**: `al profile` collects runtime profiling data from BC server. Profile file generated in standard format. Hotspot data available for inlay hints integration. `al profile --clear` removes cached profile data.
- **Fail Criteria**: Profile data not correlated to source locations. Large profile files with no summary.
- **Estimated Complexity**: High

---

## Phase 4: Advanced Intelligence & Performance
**WP7: Symbol Indexing & Semantic Optimization**
- **Goal**: Scale analysis to large AL projects.
- **Scope**: Refactor `al-symbols` for better composition. Optimize `al-semantic` bridge management (caching, process lifetime) within `al-core`.
- **Zed Impact**: Instant symbol navigation across massive projects.
- **Agent Impact**: 100% accurate symbol resolution for cross-package dependencies.

### Recommended Task Sequence

**T701: Symbol Index Performance Audit**
- **Name**: Profile symbol index on large project (50+ .app packages)
- **Files**: `crates/al-symbols/src/` (read-only analysis), `docs/proof_of_functionality.toml`
- **Dependencies**: T203 (Workspace owns SymbolIndex)
- **Pass Criteria**: Profiling report exists showing: index build time, memory usage, query latency per operation (search, by-id, events, composed). Bottlenecks identified with specific file:line references.
- **Fail Criteria**: No measurable data collected. "It seems fast enough" without numbers.
- **Estimated Complexity**: Medium

**T702: Symbol Composition Refactor**
- **Name**: Optimize composed view generation for table/page extensions
- **Files**: `crates/al-symbols/src/model.rs`, `crates/al-symbols/src/composition.rs` (or equivalent)
- **Dependencies**: T701
- **Pass Criteria**: `composed()` query for a heavily-extended table (10+ extensions) completes in <5ms. Memory allocation reduced by caching composed views. Cache invalidated correctly when a workspace file changes.
- **Fail Criteria**: Composed view returns stale data after file edit. Memory usage grows unboundedly.
- **Estimated Complexity**: High

**T703: Semantic Bridge Caching**
- **Name**: Cache .NET bridge responses for builtins and type information
- **Files**: `crates/al-core/src/semantic.rs`, `crates/al-semantic/src/lib.rs`
- **Dependencies**: T403
- **Pass Criteria**: Builtin type queries cached in-memory (e.g., `Record` methods, `Text` methods). Cache hit rate >90% for typical editing session. Bridge process not queried for previously-seen types. Cache expires on toolchain version change.
- **Fail Criteria**: Cache serves stale data after toolchain update. Memory grows without bound from cached entries.
- **Estimated Complexity**: Medium

**T704: Incremental Workspace Scanning**
- **Name**: Implement file-watcher-driven incremental re-index
- **Files**: `crates/al-core/src/workspace.rs`, `crates/al-core/src/symbols.rs`
- **Dependencies**: T203
- **Pass Criteria**: When a workspace .al file changes on disk, only that file is re-parsed and re-indexed (not the entire workspace). Symbol index updates in <50ms for a single file change. New files detected, deleted files removed.
- **Fail Criteria**: Full re-index triggered on any file change. Deleted file's symbols persist in index.
- **Estimated Complexity**: High

---

**WP8: Caching & Observability**
- **Goal**: Meet performance targets and add system transparency.
- **Scope**: Implement disk caching for symbols/ASTs. Integrate `al-diag` for request tracing and latency monitoring.
- **Zed Impact**: Sub-10ms hover/completion; fast warm starts.

### Recommended Task Sequence

**T801: Disk Cache for Symbol Packages**
- **Name**: Persist parsed symbol index to disk for fast warm starts
- **Files**: `crates/al-core/src/symbols.rs`, `crates/al-symbols/src/cache.rs` (or new)
- **Dependencies**: T203
- **Pass Criteria**: On first open, parsed symbol data written to `~/.cache/al-lsp/index/`. On subsequent opens, index loaded from disk in <500ms (vs. >5s for parsing all .app files). Cache invalidated when .app file modification time changes.
- **Fail Criteria**: Stale cache served after package update. Cache file corrupted on crash.
- **Estimated Complexity**: High

**T802: Request Tracing with al-diag**
- **Name**: Integrate al-diag tracing layer into al-core and al-lsp
- **Files**: `crates/al-diag/src/lib.rs`, `crates/al-core/src/lib.rs`, `crates/al-lsp/src/server.rs`
- **Dependencies**: T302 (queries in al-core)
- **Pass Criteria**: Every LSP/daemon request logged with: method, duration_ms, result_size, cache_hit. Logs written to SQLite via al-diag. `al-cli diag` command queries the trace database. Tracing overhead <1ms per request.
- **Fail Criteria**: Tracing adds >5ms latency. SQLite write blocks request response. Log rotation missing (unbounded growth).
- **Estimated Complexity**: Medium

**T803: Latency Budget Verification**
- **Name**: Verify all interactive queries meet latency targets
- **Files**: `docs/proof_of_functionality.toml`, `crates/al-test-harness/tests/performance.rs` (new)
- **Dependencies**: T802
- **Pass Criteria**: Measured on the Debar test project: hover <10ms, completions <20ms, definition <10ms, document symbols <5ms, semantic tokens <15ms. Results recorded in evidence log.
- **Fail Criteria**: Any interactive query exceeds 2x its target. No measurement methodology documented.
- **Estimated Complexity**: Medium

---

## Phase 5: AL Insight (The Killer App)
**WP9: Insight Graph & Agent Discovery Engine**
- **Goal**: Build the publisher-subscriber and call-graph engine.
- **Scope**: Implement `al-core::insight` using `petgraph`. Ensure the CLI/MCP/TUI can perform complex traces (e.g., event chain) in a single call.
- **Zed Impact**: Visual call graphs and event traces in `al-explorer`.
- **Agent Impact**: Context-dense traces that replace manual file scanning.

### Recommended Task Sequence

**T901: Insight Module Skeleton**
- **Name**: Create insight module with graph data structures
- **Files**: `crates/al-core/src/insight/mod.rs`, `crates/al-core/src/insight/graph.rs`, `crates/al-core/src/insight/index.rs`, `crates/al-core/src/insight/search.rs`
- **Dependencies**: T302 (queries must be in al-core)
- **Pass Criteria**: `petgraph` added to al-core dependencies. `InsightGraph` struct defined with node types (Object, Procedure, Event, Subscriber) and edge types (Calls, Publishes, SubscribesTo, Extends). Graph builds from SymbolIndex without errors.
- **Fail Criteria**: Graph data structure cannot represent circular event chains. petgraph version conflict with other deps.
- **Estimated Complexity**: Medium

**T902: Event Chain Tracing**
- **Name**: Implement event publisher-to-subscriber chain resolution
- **Files**: `crates/al-core/src/insight/search.rs`
- **Dependencies**: T901
- **Pass Criteria**: Given an event publisher name, returns the full chain: publisher -> all subscribers -> procedures they call -> events those publish -> recursive. Cycle detection prevents infinite loops. Single query returns complete chain in <100 tokens (JSON). Tested against Debar project with known event chains.
- **Fail Criteria**: Misses a subscriber in the chain. Infinite loop on circular subscription. Output exceeds 500 tokens for a simple 3-hop chain.
- **Estimated Complexity**: High

**T903: Call Graph Construction**
- **Name**: Build procedure-level call graph across workspace and packages
- **Files**: `crates/al-core/src/insight/index.rs`
- **Dependencies**: T901
- **Pass Criteria**: Call graph includes: direct procedure calls, event subscriptions as edges, trigger invocations. "Who calls this procedure?" returns accurate results. Graph built incrementally (file change updates only affected nodes).
- **Fail Criteria**: Cross-package calls not represented. Graph rebuild from scratch on every file change.
- **Estimated Complexity**: High

**T904: Insight Daemon Queries**
- **Name**: Expose insight queries via daemon JSON-RPC protocol
- **Files**: `crates/al-lsp/src/daemon.rs`, `crates/al-core/src/queries/insight.rs` (new)
- **Dependencies**: T902, T903, T303
- **Pass Criteria**: `al tables "Sales-Post" --json` returns tables with CRUD operations. `al callgraph "Sales-Post" --proc PostDocument --depth 3 --json` returns transitive call chain with table touches. `al intercept "Sales-Post" --field "Sales Header" --json` returns events with var parameters. `al subscribers "Sales-Post" --json` returns all subscribers grouped by event. MCP equivalents produce identical results. All schemas match `docs/agentic-schemas.md`. All scenarios in `docs/agent-scenarios.md` pass.
- **Fail Criteria**: CLI query returns empty when daemon has the data. Different result from CLI vs MCP for same query. Missing table operations (e.g., procedure calls Customer.Modify but table not in results). Callgraph stops at object boundary instead of following cross-codeunit calls.
- **Estimated Complexity**: Medium

**T905: Table Impact Analysis**
- **Name**: Implement `al tables` — trace Record variable usage through procedure calls
- **Files**: `crates/al-core/src/insight/search.rs`
- **Dependencies**: T903 (call graph), T901
- **Pass Criteria**: Given a codeunit/procedure, returns all tables with CRUD operations (get, find, findset, findfirst, findlast, insert, modify, delete, modifyall, deleteall, setrange, setfilter, calcsums, calcfields). Operations detected by resolving Record variable types and matching method calls. Cross-procedure tracing follows call graph edges. Schema matches `docs/agentic-schemas.md` `/al-tables`.
- **Fail Criteria**: Misses a table that is accessed through a local variable in a called sub-procedure. Reports operations that don't actually occur. Cannot distinguish read vs write operations.
- **Estimated Complexity**: High

**T906: Event Interception Discovery**
- **Name**: Implement `al intercept` — find events where behavior can be altered
- **Files**: `crates/al-core/src/insight/search.rs`
- **Dependencies**: T902 (event chain), T901
- **Pass Criteria**: Returns integration and business events with their parameter lists. Events with `var` parameters are flagged as modifiable. `--field` filter narrows to events whose parameters include that table type. `--proc` filter narrows to events published within that procedure. Schema matches `docs/agentic-schemas.md` `/al-intercept`.
- **Fail Criteria**: Misses events with var parameters. Doesn't distinguish integration vs business event types. Field filter doesn't resolve table names from Record parameter types.
- **Estimated Complexity**: Medium

**T907: Source Extraction Command**
- **Name**: Implement `al source` — targeted source code extraction via CLI/MCP
- **Files**: `crates/al-core/src/queries/source.rs` (new), `crates/al-lsp/src/daemon.rs`
- **Dependencies**: T302 (queries in al-core)
- **Pass Criteria**: Workspace objects: tree-sitter range extraction returns exact procedure/trigger source with line range. Package objects with source in .app: extracts source from ZIP. Package objects without source: renders complete outline from SymbolReference.json with full procedure signatures (params + return types), field definitions (id + name + type), keys, enum values, event declarations. `src` field accurately indicates `workspace`, `package`, or `outline`. Schema matches `docs/agentic-schemas.md` `/al-source`.
- **Fail Criteria**: Returns empty for a procedure that exists. Outline missing parameter types or return types that are in SymbolReference.json. Tree-sitter range off by even one line.
- **Estimated Complexity**: Medium

**T908: Remove generate_al Fallback**
- **Name**: Replace `generate_al()` in `al-symbols/src/virtual_file.rs` with proper `render_outline()` that uses all SymbolReference.json data
- **Files**: `crates/al-symbols/src/virtual_file.rs`
- **Dependencies**: T907 (source extraction must be working first)
- **Pass Criteria**: `generate_al()` deleted. `render_outline()` produces AL code with: full procedure signatures (all parameters with types, var/out modifiers, return type), field declarations (id, name, type), key declarations, enum values with ordinals, event declarations, global variables. `allow_outline_fallback` flag removed — outline rendering is the standard output for packages without source, not a degraded mode. No concept of "fallback" anywhere in the codebase.
- **Fail Criteria**: Any procedure rendered without its parameter types. Any field rendered without its type. The word "fallback" appears in code comments or variable names related to outline rendering.
- **Estimated Complexity**: Low

---

**WP10: Explorer & TUI Transformation**
- **Goal**: Provide a rich UI for workspace exploration.
- **Scope**: Refactor `al-explorer` into a modular TUI consuming `al-core` data.
- **Zed Impact**: High-density object/event designer accessible via Zed task.

### Recommended Task Sequence

**T1001: Explorer as Daemon Client**
- **Name**: Rewrite al-explorer to connect to al-lsp daemon
- **Files**: `crates/al-explorer/src/main.rs` (rewrite), `crates/al-explorer/src/client.rs` (new), `crates/al-explorer/src/app.rs` (new), `crates/al-explorer/Cargo.toml`
- **Dependencies**: T303 (daemon mode), T304 (can reuse client code pattern from al-cli)
- **Pass Criteria**: `al-explorer` has ZERO compile-time dependency on al-core. Connects to daemon socket. Object list, symbol search, and event views all populated from daemon queries. `cargo tree -p al-explorer` shows no path to al-core.
- **Fail Criteria**: Any import of al-core in al-explorer. TUI renders but shows no data.
- **Estimated Complexity**: Medium

**T1002: Object Browser View**
- **Name**: Implement main object list with filtering and detail pane
- **Files**: `crates/al-explorer/src/ui.rs` (new), `crates/al-explorer/src/actions.rs` (new)
- **Dependencies**: T1001
- **Pass Criteria**: Left pane shows objects grouped by type (Table, Page, Codeunit, etc.) with counts. Fuzzy filter narrows list in real-time. Right pane shows object details (fields/methods/properties). Arrow keys and enter navigate. `q` quits cleanly.
- **Fail Criteria**: UI freezes during initial load. Filter does not update instantly (<100ms).
- **Estimated Complexity**: Medium

**T1003: Event/Call Graph View**
- **Name**: Add event chain and call graph visualization to explorer
- **Files**: `crates/al-explorer/src/ui.rs`, `crates/al-explorer/src/app.rs`
- **Dependencies**: T1002, T904 (insight queries must be available)
- **Pass Criteria**: Tab switches to "Events" view. Selecting an event publisher shows subscriber chain as a tree. "Calls" view shows call graph for selected procedure. Graph navigation with expand/collapse.
- **Fail Criteria**: Graph view crashes on circular references. Tree rendering garbled for deep chains (>5 levels).
- **Estimated Complexity**: High

---

## Phase 6: Validation & Release
**WP11: Integration & Release Engineering**
- **Goal**: Finalize for production.
- **Scope**: Exhaustive integration testing via `al-test-harness`. Finalize deterministic build/release pipeline.
- **Files**: `crates/al-test-harness/*`, `Makefile`, `docs/release.md`.

### Recommended Task Sequence

**T1101: Comprehensive Integration Test Suite**
- **Name**: Write end-to-end tests covering every daemon query via the harness
- **Files**: `crates/al-test-harness/tests/integration_full.rs` (new)
- **Dependencies**: T302, T303 (all queries via daemon)
- **Pass Criteria**: Every al-core query has at least one harness test via both stdio (LSP) and socket (daemon) transports. Test count >60. All pass on CI. Adversarial tests included (malformed requests, concurrent requests, large files).
- **Fail Criteria**: Any query type untested. Tests pass only on developer machine.
- **Estimated Complexity**: High

**T1102: Zed Fidelity Test Suite**
- **Name**: Validate every LSP feature in real Zed environment
- **Files**: `crates/al-test-harness/tests/zed_fidelity.rs` (new), `docs/proof_of_functionality.toml`
- **Dependencies**: T1101
- **Pass Criteria**: Checklist of 20+ Zed operations (open file, hover, go-to-def, completions, rename, format, diagnostics, etc.) each verified in a real Zed window against the Debar project. Results recorded in evidence log with pass/fail and screenshots where relevant.
- **Fail Criteria**: Harness claims pass but Zed shows different behavior. Any Priority-0 regression unfixed.
- **Estimated Complexity**: High

**T1103: Build and Release Pipeline**
- **Name**: Deterministic build, packaging, and release automation
- **Files**: `Makefile` (or `justfile`), `.github/workflows/release.yml` (new), `extension.toml`
- **Dependencies**: T602 (WASM entry finalized)
- **Pass Criteria**: Single command builds: al-lsp (native), zed-al (WASM), al-cli, al-explorer, al-mcp. Versioning derived from git tags. WASM artifact size <5MB. Release artifacts include checksums. CI runs full test suite before release.
- **Fail Criteria**: Build requires manual steps not captured in automation. WASM build fails on CI but passes locally.
- **Estimated Complexity**: Medium

**T1104: Documentation and Extension Packaging**
- **Name**: Finalize extension.toml, README, and publishing metadata
- **Files**: `extension.toml`, `languages/al/config.toml`
- **Dependencies**: T1103
- **Pass Criteria**: Extension installs from Zed extension marketplace (or local .vsix equivalent). All metadata (name, description, version, language support, grammar, LSP config) correct. First-run experience documented: install extension, open AL project, features work.
- **Fail Criteria**: Extension fails to load in Zed. Language not recognized after install.
- **Estimated Complexity**: Low

**T1105: Final Surgical Audit**
- **Name**: Complete workspace cleanup and architecture validation
- **Files**: All crates, `docs/progress.md`, `docs/proof_of_functionality.toml`
- **Dependencies**: T1104
- **Pass Criteria**: No dead code (unused modules, unreachable functions). No logic leakage (business logic in thin adapters). Evidence log complete for all WPs with dual-pass entries. `cargo clippy --workspace` clean. `cargo tree` confirms dependency direction: only al-lsp -> al-core, no reverse.
- **Fail Criteria**: Any `#[allow(dead_code)]` without documented justification. Logic found in al-cli/al-explorer/al-mcp that should be in al-core.
- **Estimated Complexity**: Medium
