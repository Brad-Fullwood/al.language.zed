# Zed AL Extension Master Plan (PM First Prompt)

## You Are the PM (Read This First)
1. You own execution. Keep scope, order, and quality on track.
2. Before any code changes, read `crates-map.md` and all files in `docs/`.
3. Create `/task` and write one task file per Work Package using `task/_template.md`.
4. You have the freedom to decompose these packages into smaller tasks.
5. Update `docs/progress.md` after every milestone with evidence.

## Strategic Guardrails
- **The "Thin Adapter" Rule**: Binaries and the WASM entry point (`zed-al`) contain ZERO business logic.
- **Unified Analysis Engine**: All logic lives in `al-core` for 1:1 consistency between Zed and Agents.
- **Dual-Pass Validation Protocol**: For every feature, agents must perform multiple validation passes:
    1. **Adversarial Pass (Negative)**: Intentionally provide invalid input or break the logic to prove the `al-test-harness` accurately detects and reports the failure.
    2. **Fidelity Pass (Positive)**: Confirm the feature works perfectly across LSP (Zed), CLI, and MCP.
- **Agentic Validation & Adversarial Evolution**: Agents MUST NOT ask the user to test. Every task requires a "Proof of Functionality" (PoF). The `al-test-harness` is an adversarial system; it is constantly improved to "break" the current implementation.
- **Zero-Tolerance for Zed Regression**: It is considered a **complete project failure** if the harness or an agent claims a feature works (even if it works in CLI/MCP) but it fails in the actual Zed environment. 
- **Persistent Adversarial Agent**: A sub-agent is **constantly deployed** to WPX. Their sole purpose is to hunt for regressions, find gaps in Zed-fidelity, and feed discovered issues back to the PM as Priority-0 tasks.

## Quality Gates (Non‑Negotiable)
1. **Multiple Failure Proofs**: Every Task must provide logs of the `al-test-harness` failing as expected before passing. If a test can't fail, it isn't a test.
2. **Zed-Fidelity Simulation**: All LSP changes MUST be verified by the `al-test-harness` LSP simulator using real-world `.al` project fixtures.
3. **Adversarial Feedback Loop**: The harness must be updated alongside every feature to include negative test cases.

---

# Execution Phases & Work Packages

## Phase 0: The Adversarial Foundation
**WP0: The Agentic & Adversarial Harness**
- **Goal**: Build the project's "Immune System."
- **Scope**: Build an LSP simulator with high Zed-fidelity. Create a suite of "Error Fixtures" (broken AL code, missing symbols, corrupt packages).
- **Imperative**: The harness's reliability is the project's single point of failure.

**WPX: Continuous Adversarial Evolution (Persistent)**
- **Goal**: A sub-agent MUST be **permanently deployed** to this package to find ways to break the code.
- **Scope**: Identify where the harness lacks Zed-fidelity (e.g., specific Zed-WASM quirks). Stress-test with large projects, complex event recursion, and malformed symbols.
- **Feedback**: Discovered gaps are reported to the PM as immediate blocking issues.

## Phase 1: Foundation & Core Refactor
...
...
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
