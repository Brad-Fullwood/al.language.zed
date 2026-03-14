# Agent Infrastructure Rationale

Design decisions for `.claude/` configuration. Reference for future maintainers.

## Rules Restructuring

### Problem
The original 7 rules totaled ~340 lines, all loaded unconditionally. Claude Code adherence degrades with context length. Agents received rules about thin adapters when editing tests, and testing protocols when editing CLI code.

### Solution
Consolidated into 6 rules: 1 always-loaded (~18 lines) + 5 conditionally-loaded via `paths:` frontmatter (~120 lines total). Always-loaded context dropped from ~340 to ~18 lines (95% reduction).

| Rule | Scope | Loads When |
|---|---|---|
| `architecture.md` | Always | Every session (essential context) |
| `thin-adapters.md` | `crates/al-cli/**`, `crates/al-explorer/**`, `crates/al-mcp/**`, `crates/zed-al/**` | Editing adapter code |
| `testing.md` | `crates/al-test-harness/**`, `docs/proof_of_functionality.toml`, `**/tests/**` | Writing tests or PoF entries |
| `code-boundaries.md` | `crates/*/src/**/*.rs`, `crates/*/Cargo.toml` | Editing any Rust source |
| `zed-fidelity.md` | `crates/al-lsp/**`, `crates/al-test-harness/**`, `crates/zed-al/**` | Working on LSP or harness |
| `agentic-output.md` | `crates/al-cli/**`, `crates/al-mcp/**` | Working on CLI or MCP |

### What was removed
- `adversarial-evolution.md`: Overlapped with `docs/adversarial-atlas.md` and `testing.md`. The "when to add tests" table and adversarial mandate are now in `testing.md`. The stress test catalog stays in `docs/adversarial-atlas.md`.
- `maintenance-governance.md`: Became the `/audit` skill and `auditor` agent. Process checklists are better as executable skills than static rules.
- `library-responsibilities.md`: Ownership table now in `code-boundaries.md` (concise). Full reference in `docs/crates-map.md`.
- `validation-protocol.md`: PoF format and dual-pass requirement now in `testing.md`. Test categories table kept.

## Agent Design

### test-runner (haiku)
**Why haiku**: Purely mechanical — run cargo test, parse output, report results. No judgment needed. Haiku is 20x cheaper than Opus and sufficient for this.
**Why separate agent**: Tests are the most frequent agent action. A dedicated agent with minimal maxTurns (8) prevents runaway debugging sessions.

### guardian (sonnet)
**Why sonnet**: Needs to interpret Cargo.toml structure, understand import patterns, and synthesize findings. More than pattern matching (haiku can't do this well) but doesn't need architectural judgment (opus is overkill).
**Why separate agent**: Architecture validation is independent from implementation. Running it as a separate agent protects the main context from verbose cargo tree output.

### auditor (sonnet)
**Why sonnet**: Follows a checklist but needs to read PoF entries and verify they're complete. Moderate judgment required.
**Why separate agent**: Audit is a milestone event, not per-task. The checklist is well-defined, making it ideal for a dedicated agent.

### Why no "implementer" agent
Implementation work requires deep context about the codebase, architecture decisions, and cross-crate interactions. This is Opus-tier work that should stay in the main conversation, not delegated to a sub-agent that lacks context.

## Skills Design

### /test
**Why a skill, not just "run cargo test"**: Standardizes the test command (always excludes zed-al WASM target), handles different invocation patterns (full suite, single crate, test filter), and ensures consistent output formatting.

### /check
**Why context: fork + guardian agent**: Compliance checking produces verbose output (cargo tree, grep results) that would pollute the main context. Forking to the guardian agent keeps the main conversation clean.

### /pof
**Why a skill**: The TOML format is exact and easy to get wrong. A skill ensures the template is correct every time, preventing malformed entries that would fail the auditor.

### /audit
**Why context: fork + auditor agent**: Same reasoning as /check — audits are heavyweight operations that produce substantial output.

## Hook: check-thin-adapter.sh

### Why this specific hook
The thin-adapter architecture rule is the most critical and most frequently violated constraint. An agent under pressure to "just make it work" will add `use al_core::` to al-cli. The hook catches this before the edit lands, with a clear explanation of why it's blocked.

### Why only PreToolUse on Edit|Write
- PostToolUse would be too late — the file is already written.
- Checking every Bash command would be too broad and slow.
- The specific matcher `Edit|Write` catches both editing existing files and creating new ones.

### What the hook does NOT check
- It doesn't validate crate-internal boundaries (e.g., al-core using al-syntax correctly).
- It doesn't check for _indirect_ dependencies via transitive imports.
- For those, use the `/check` skill or guardian agent.

## CLAUDE.md Design

### Target: ~45 lines
Previous: 57 lines. Reduced by cutting "Current Violations" (process, not reference) and trimming the docs list to essentials. Added agents/skills section so agents know what tools are available.

### What stays in CLAUDE.md vs rules
- **CLAUDE.md**: Architecture overview, build commands, key gotchas, doc pointers, available tools. Things EVERY agent session needs.
- **Rules**: Specific enforcement constraints scoped to relevant files. Things agents need ONLY when working in specific areas.

## Additional Hooks

### SessionStart/compact re-injection
When Claude Code compacts conversation context (to stay within the context window), critical architectural constraints can be silently dropped. A `SessionStart` hook with `compact` matcher re-injects the thin-adapter rule as a system message. Pattern from Anthropic's official best practices.

### Cargo credential deny rules
Following the Trail of Bits pattern, `.claude/settings.json` includes deny rules for `~/.cargo/credentials.toml`, `~/.cargo/credentials`, and `~/.cargo/registry/`. Prevents accidental credential exposure during agent operations.

### Skills with `disable-model-invocation: true`
The `/audit` and `/pof` skills have side effects (modifying files). `disable-model-invocation: true` prevents Claude from auto-triggering them — they only run when the user explicitly invokes them. Pattern from Anthropic's official skills documentation.

## Documentation Fixes

### ST-06 in adversarial-atlas.md
Fixed factual error: the .NET bridge was described as a killable subprocess, but it is in-process CLR hosting via `netcorehost`. The test scenario and pass criteria were updated to reflect the actual failure mode (CLR exceptions, state corruption, graceful degradation to syntax-only mode).

## Decisions on External Resources

### Adopted patterns
- **Trail of Bits hook approach**: Inspired the PreToolUse hook for architectural enforcement. Their pattern of "block before damage" rather than "detect after" is the right model. Also adopted their credential deny-list pattern.
- **Rules scoping via paths:**: Standard Claude Code feature, confirmed as the recommended approach by claudefa.st, Builder.io, and HumanLayer guides.
- **SessionStart/compact re-injection**: From Anthropic's official best practices. Context compaction can drop critical constraints.
- **`disable-model-invocation: true`**: From Anthropic's official skills docs. Side-effect skills should never auto-trigger.

### Adopted from external resources
- **rust-analyzer-mcp**: Installed via `cargo install rust-analyzer-mcp`. Configured in `.mcp.json`. Provides 10 tools for semantic Rust navigation (symbols, definition, references, hover, completion, format, code_actions, diagnostics, workspace_diagnostics, set_workspace). Enables agents to navigate across the multi-crate workspace with full type awareness.
- **Agent teams** (team-lead agent): Created `.claude/agents/team-lead.md` with crate-level ownership protocol and conflict prevention rules. Teams are experimental but the configuration is ready for WP5+ parallel work.
- **Performance benchmark hooks**: Added `PostToolUse` hook (`check-perf-impact.sh`) that fires asynchronously when editing performance-critical code paths. Advisory only (doesn't block), reminds agents of latency targets from `docs/performance-plan.md`.

### Not adopted
- **ccswarm/parallel-worktrees**: Native `isolation: worktree` in agent files covers this without external tools.
- **codebase-memory-mcp**: Overkill for ~10 crate workspace.
- **SkillsMP/skillkit**: Generic skills don't match our specialized workflow.

## Agent-Optimized Documentation Layer

### Implementation: `.claude/constraints.toml`
Created a TOML index of all architectural constraints, each with:
- `id`: unique identifier (ARCH-001, TEST-001, PERF-001, etc.)
- `rule`: the constraint in one sentence
- `tags`: categories (architecture, testing, quality, performance, agentic, zed)
- `crates`: which crates this applies to
- `enforced_by`: how it's enforced (hook, agent, benchmark, test, manual)
- `doc`: where the full rule is documented

This supplements (not replaces) the human-readable rules. Agents can grep this file to find relevant constraints for their current task without reading all docs.

## Architectural Decisions Made

### JSON-RPC shared types: `al-protocol` crate
Decision: create a standalone `al-protocol` crate with only serde structs for the daemon JSON-RPC protocol. Thin adapters depend on this (types only, no logic), preserving the zero-al-core-dependency mandate while providing type safety. Updated in `docs/crates-map.md`.

### Insight CLI naming: flat top-level commands
Decision: `al trace`, `al tables`, `al callgraph`, `al intercept`, `al subscribers` — not `al insight <subcommand>`. The `al insight` prefix adds a word to every command for no value. `docs/agentic-schemas.md` (authoritative) and `docs/agent-scenarios.md` already use the flat form. Updated `docs/insight.md` to match.

### `al suggest-event` schema
Added missing schema to `docs/agentic-schemas.md` and corresponding MCP tool entry. Returns event suggestions with `why` explanation and ready-to-paste `example` subscriber attribute.

### CLAUDE.md casing
Renamed `claude.md` to `CLAUDE.md` (uppercase). The lowercase version worked but violated convention and risked future breakage on case-sensitive filesystems. Updated all references in `plan.md`, `docs/progress.md`.
