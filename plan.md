# Zed AL Extension Master Plan (PM First Prompt)
## You Are the PM (Read This First)
1. You own execution. Keep scope, order, and quality on track.
2. Before any code changes, read `crates-map.md` and all files in `docs/`.
3. Create `/task` and write one task file per Work Package using `task/_template.md`.
4. **MANDATORY STARTUP**: As your absolute first action, you MUST create `claude.md` at the project root AND a `.claude/rules/` directory. 
   - `claude.md`: Your expert distillation of the roadmap, architectural goals, and high-level strategy.
   - `.claude/rules/`: Detailed markdown files documenting EVERY constraint, mandate, and "must/must not" rule discussed, including strict test procedures and adversarial requirements.
   - **Skills & Agents**: Evaluate and document whether specialized Claude skills or dedicated sub-agent definitions are required to automate the adversarial testing or high-density discovery workflows.
   - **Gating**: You are strictly prohibited from proceeding to ANY other task until this "Constitution" is complete and you have validated that no rule or constraint has been missed.
5. **Milestone Audits**: Every 3–5 tasks (or at the end of every Work Package), you MUST perform a **Surgical Audit**. You are prohibited from starting new work until the project is "Cleaned and Signed Off" (see Governance below).
6. Update `docs/progress.md` after every milestone with evidence.

## Governance & Audit Gates
At every Milestone Audit, the PM must:
1. **Surgical Cleanup**: Delete all unneeded code, obsolete files, and redundant directories. Remove all temporary artifacts, debug logs, or "cruft" introduced during implementation. The workspace must remain lean and focused.
2. **Architectural Audit**: Manually review the last 5 tasks for "architectural drift." Ensure logic hasn't leaked from `al-core` into the binaries.
3. **Log Interrogation**: Audit `docs/proof_of_functionality.toml`. Every task must have a valid Red-to-Green trace.
4. **Formal Sign-off**: Append a "Milestone Sign-off" entry to `docs/progress.md` certifying the integrity of the work before proceeding to the next Work Package.

## Strategic Guardrails
...

- **The "Thin Adapter" Rule**: Binaries and the WASM entry point (`zed-al`) contain ZERO business logic.
- **Unified Analysis Engine**: All logic lives in `al-core` for 1:1 consistency between Zed and Agents.
- **Dual-Pass Validation & Centralized Evidence**: For every feature, agents must perform multiple validation passes. All results (Failures and Successes) must be outputted to the **Centralized Evidence TOML** (`docs/proof_of_functionality.toml`).
- **Agentic Validation & Adversarial Evolution**: Agents MUST NOT ask the user to test. Every task requires a "Proof of Functionality" (PoF). The `al-test-harness` is an adversarial system; it is constantly improved to "break" the current implementation.
- **Zero-Tolerance for Zed Regression**: It is considered a **complete project failure** if the harness or an agent claims a feature works but it fails in the actual Zed environment. 
- **Persistent Adversarial Agent**: A sub-agent is **constantly deployed** to WPX to hunt for regressions and interrogate the TOML log for suspicious "Success-only" trends.

## Quality Gates (Non‑Negotiable)
1. **Constitution Established**: `claude.md` and `.claude/rules/` must be complete and validated before any implementation work begins.
2. **Red-to-Green TOML Evidence**: Every Task must append a structured entry to `docs/proof_of_functionality.toml` showing the `al-test-harness` failing as expected before passing. **No Red, No Merge.**
3. **Zed-Fidelity Simulation**: All LSP changes MUST be verified by the `al-test-harness` LSP simulator using real-world `.al` project fixtures.
4. **Adversarial Feedback Loop**: The harness must be updated alongside every feature to include negative test cases.

---

# Execution Phases & Work Packages

## Phase 0: The Adversarial Foundation
**WP0: Project Constitution & The Adversarial Harness**
- **Goal**: Establish the project's rules and build its "Immune System."
- **Scope**: 
    - Create `claude.md` and `.claude/rules/` as the project's rulebook.
    - Evaluate and define required Claude Skills or Agents for the workspace.
    - Validate that no constraints or mandates have been missed.
    - Build an LSP simulator with high Zed-fidelity in `al-test-harness`.
    - Implement the **Centralized Evidence Log** system in TOML.
- **Gating**: No other work packages may start until WP0 is verified.

**WPX: Continuous Adversarial Evolution (Persistent)**
- **Goal**: A sub-agent is **permanently deployed** to break the code and interrogate the log.
- **Scope**: Identify where the harness lacks Zed-fidelity. Interrogate the `proof_of_functionality.log` for tasks with insufficient negative testing.
- **Feedback**: Discovered gaps are reported to the PM as immediate blocking issues.

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
