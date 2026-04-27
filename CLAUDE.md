# CLAUDE.md

This file is the **single source of truth** for AI agents working on this codebase. Every rule here is non-negotiable. Violating these rules wastes human review time and creates revert commits.

---

## ⚠️ Refactor in Progress: Crate Consolidation

**The workspace is mid-refactor — collapsing 10 native crates into 4.**
Plan: `~/.claude/plans/i-am-thinking-about-playful-bentley.md`

**Target layout (what this file describes):**

| Crate | Status |
|-------|--------|
| `al-core` | Absorbing al-syntax, al-symbols, al-semantic, al-lsp, al-dap-client; ships `[[bin]] al-lsp` |
| `al-protocol` | Renamed from al-daemon-client |
| `al-explorer` | Absorbing al-cli (TUI default + clap subcommands) |
| `zed-al` | Unchanged (WASM) |
| `al-test-harness`, `al-zed-test` | Unchanged |

**Stage progress:**
- [x] **Stage 1** — CLAUDE.md / hooks / skills updated to target layout
- [x] **Stage 2** — `al-daemon-client` → `al-protocol` rename
- [x] **Stage 3** — `al-dap-client` folded into al-core (now `al_core::dap`)
- [x] **Stage 4** — `al-syntax` folded into al-core (now `al_core::syntax`; LSP-bridge helpers at `al_core::syntax_lsp`)
- [ ] Stage 5 — `al-symbols` folded into al-core
- [ ] Stage 6 — `al-semantic` folded into al-core
- [ ] Stage 7 — `al-lsp` folded into al-core as `[[bin]]`
- [ ] Stage 8 — `al-cli` folded into al-explorer
- [ ] Stage 9 — Final docs/hooks reconciliation (this banner removed)

**While the refactor is in flight:** the crate names below describe the *target* state. The repo's `Cargo.toml` reflects in-flight reality. If hooks/agents seem out of sync with on-disk crates, check this banner — the docs lead, the code is catching up.

---

## Quick Reference

```sh
cargo check --workspace --exclude zed-al     # compile check (ALWAYS run before committing)
cargo build --workspace --exclude zed-al     # build all native crates
cargo test  --workspace --exclude zed-al     # run all tests
cargo clippy --workspace --exclude zed-al -- -D warnings  # lint (must pass CI)
cargo fmt --all                               # format
cargo test -p al-core                        # test the core crate (covers syntax/symbols/semantic/server)
cargo test -p al-core --test e2e             # single test file
cargo build -p zed-al --target wasm32-wasip1 --release  # WASM extension (separate target)
make build                                    # all Rust crates + .NET bridges
make install                                  # build + symlink into PATH + Zed
```

**Prerequisites:** Rust stable, .NET SDK (auto-downloaded if absent).

**Always exclude `zed-al`** from workspace commands — it requires `wasm32-wasip1` and will fail on the default target.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│ ENTRY POINTS                                                        │
│                                                                     │
│  Zed editor      →  al-lsp (binary inside al-core) --stdio         │
│  Zed debugger    →  al-lsp --dap                                   │
│  al-explorer     →  al-lsp daemon (over al-protocol IPC)           │
│  zed-al (WASM)   →  spawns al-lsp                                  │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────┐      │
│  │  al-core — ALL logic + binaries                          │      │
│  │  ├─ syntax    (parsing, formatting, linting, types)      │      │
│  │  ├─ symbols   (.app reading, NuGet, symbol index)        │      │
│  │  ├─ semantic  (.NET CLR bridge — CodeAnalysis)          │      │
│  │  ├─ server    (LSP/daemon transport conversion)          │      │
│  │  ├─ dap       (Debug Adapter Protocol framing + proxy)   │      │
│  │  ├─ queries/  (transport-agnostic LSP feature impls)     │      │
│  │  └─ bin/al-lsp.rs (the binary entry point)               │      │
│  └─────────────────────────────────────────────────────────┘      │
│                          ▲                                          │
│                          │ depends on                               │
│  ┌─────────────────────────────────────────────────────────┐      │
│  │  al-protocol — daemon IPC types (~450 lines)             │      │
│  └─────────────────────────────────────────────────────────┘      │
│                          ▲                                          │
│                          │ depends on                               │
│  ┌─────────────────────────────────────────────────────────┐      │
│  │  al-explorer — unified TUI + CLI client                  │      │
│  │  ├─ TUI mode (default, no args)                          │      │
│  │  └─ cli/  (clap subcommands for scripted use)            │      │
│  └─────────────────────────────────────────────────────────┘      │
│                                                                     │
│  zed-al (WASM, isolated, no compile-time native deps)              │
└─────────────────────────────────────────────────────────────────────┘
```

### Crate Responsibilities

| Crate | Approx Lines | Role | Key Modules / Types |
|-------|--------------|------|---------------------|
| **al-core** | ~55K | All business logic + LSP binary | `Workspace`, `DocumentStore`; `syntax::{AlParser, TypeResolver, LanguageData}`; `symbols::{AppReader, SymbolIndex, NugetClient}`; `semantic::SemanticBridge`; `server::AlServer`; `dap::*`; `[[bin]] al-lsp` |
| **al-protocol** | ~450 | Daemon IPC types (shared between al-core daemon and al-explorer) | `DaemonClient`, request/response enums |
| **al-explorer** | ~6K | Unified TUI + CLI client | ratatui app + `cli::*` clap commands |
| **zed-al** | ~620 | WASM extension for Zed | `AlExtension` |
| **al-test-harness** | n/a | E2E tests over the real `al-lsp` binary | `LspClient` |
| **al-zed-test** | n/a | Live tests against real Zed | — |

### The One Rule of Architecture

**Query functions in `al-core::queries::*` return transport-agnostic types — never `lsp_types::*`.**
The `al-core::server` module converts to LSP types at the boundary. This rule is no longer compiler-enforced (al-lsp folded into al-core), so it's a coding discipline. PRs introducing `lsp_types::*` into a `queries::*` function signature will be rejected.

### al-lsp Server Modes (binary lives inside al-core)

| Mode | Arg | Transport | Client |
|------|-----|-----------|--------|
| LSP | `--stdio` (default) | tower-lsp over stdin/stdout | Zed editor |
| Daemon | `daemon --project <path>` | JSON-RPC over Unix socket (al-protocol types) | al-explorer |
| DAP | `--dap` | Debug Adapter Protocol over stdio | Zed debugger |

Socket path: `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock`. Daemon auto-shuts down after 30min idle.

---

## Dependency Rules (ENFORCED)

```
al-explorer  ──→  al-protocol
al-core      ──→  al-protocol
zed-al       (isolated, WASM)
```

### Hard Constraints

1. **`al-explorer` depends only on `al-protocol`.** Never on `al-core`. Pulling al-core into the client would drag in tree-sitter, the .NET CLR (semantic bridge), and tower-lsp — for a TUI binary.
2. **`al-core` may depend on `al-protocol`.** Never the reverse — al-protocol must stay a tiny types-only crate.
3. **`zed-al` is completely isolated** — no compile-time dependency on any native crate.
4. **No upward dependencies, no cycles.**

The old per-module rules (al-syntax / al-symbols / al-semantic must not depend on each other) are gone — they're modules now, not crates. Module-level discipline is enforced by code review, not the compiler.

**Before adding a dependency**, check this rule. If your change would create a wrong-direction dependency, restructure it.

---

## CRITICAL: No Hardcoded Language Values

**NEVER hardcode AL language keywords, built-in functions, object types, or any language-specific lists.** This is the #1 cause of agent reverts in this project.

AL is a living language — Microsoft updates it with every Business Central release. Hardcoded lists become stale immediately.

### What Counts as a Violation

```rust
// ALL OF THESE ARE VIOLATIONS:
const AL_BUILTIN_FUNCTIONS: &[&str] = &["Message", "Error", ...];
const AL_KEYWORDS: &[&str] = &["begin", "end", "procedure", ...];
const TRIGGER_NAMES: &[&str] = &["OnRun", "OnValidate", ...];
const AL_DATA_TYPES: &[&str] = &["Integer", "Text", "Code", ...];
let builtins = vec!["Record", "Codeunit", "Page", ...];
match kind { "table" | "page" | "codeunit" => ... }  // if matching AL object types
```

### What to Use Instead

| Need | Source |
|------|--------|
| Keywords, built-in functions, types | `al_core::syntax::LanguageData` (loads from `tree-sitter-al/data/` JSON files) |
| Object types, fields, events | `al_core::symbols` (reads `.app` packages at runtime) |
| Semantic info, error codes | `al_core::semantic` bridge (queries .NET CLR at runtime) |

If the extraction pipeline lacks what you need, **update the generator** at `tree-sitter-al/generator/tools/al-extract/` — do NOT create a hardcoded constant.

### Hook Enforcement

A PostToolUse hook in `.claude/settings.json` blocks any `.rs` file containing hardcoded AL value patterns. If you hit this block, you're doing it wrong — use `LanguageData` or the symbol index.

---

## Scope Discipline (READ THIS)

This section exists because agents have repeatedly caused problems by making changes beyond what was asked.

### Rules

1. **Only change what was requested.** If asked to fix a bug in `completions.rs`, do NOT also refactor `hover.rs`, add docstrings to `definition.rs`, or "clean up" unrelated code.
2. **Never rename or restructure files** unless explicitly asked.
3. **Never change public API signatures** unless the task specifically requires it.
4. **Never add dependencies** to Cargo.toml without explicit approval.
5. **Never modify CI/CD** (`.github/workflows/`) without explicit approval.
6. **Never modify the tree-sitter grammar** (`tree-sitter-al/`) — it's a submodule with its own process.
7. **Never create new crates** — the architecture is intentional.
8. **WIP commits are not acceptable** — every commit should compile and pass tests.

### The "While I'm Here" Anti-Pattern

Do NOT:
- "While fixing this bug, I also improved error messages in 5 other files"
- "I noticed some code style inconsistencies and fixed those too"
- "I added type annotations to functions that didn't have them"
- "I refactored the match statement to be more idiomatic"

These create noise in code review, risk regressions, and waste reviewer time. Make a separate PR if you genuinely believe something else needs changing.

---

## Code Patterns

### Position Handling (UTF-16 ↔ Bytes)

LSP positions use UTF-16 code units. Rust strings are UTF-8 bytes. **Always convert.**

```rust
// CORRECT: Convert LSP position to byte offset via rope
let line = rope.line(position.line as usize);
let byte_offset = rope.line_to_byte(position.line as usize)
    + line.utf16_cu_to_byte(position.character as usize);

// WRONG: Treating character as byte offset
let byte_offset = position.character as usize;  // BUG for non-ASCII
```

### Tree-sitter Traversal

Use iterative traversal with explicit stack, not recursion (avoids stack overflow on deeply nested AL):

```rust
// PREFERRED: Iterative
let mut cursor = node.walk();
let mut stack = vec![node];
while let Some(current) = stack.pop() {
    // process current
    stack.extend(current.named_children(&mut cursor));
}

// AVOID: Recursive (stack overflow risk)
fn walk(node: Node) {
    for child in node.named_children(&mut node.walk()) {
        walk(child);  // can blow stack on deep nesting
    }
}
```

### Error Handling

```rust
// In query functions (al_core::queries::*): return Option or Result with native types
pub fn hover(workspace: &Workspace, uri: &Url, pos: Position) -> Option<HoverResult> { ... }

// In server handlers (al_core::server::*): convert to LSP errors
// NEVER unwrap() in request handlers — return an error response

// In library modules (syntax, symbols): use Result with thiserror
// NEVER panic in library code
```

### DashMap Access

```rust
// CORRECT: Short-lived borrows
let name = workspace.symbols.get(&key).map(|entry| entry.name.clone());

// WRONG: Holding DashMap ref across await points (deadlock)
let entry = workspace.symbols.get(&key);
some_async_operation().await;  // DEADLOCK: still holding DashMap shard lock
drop(entry);
```

---

## Key Gotchas

- `.app` files: `SymbolReference.json` has **UTF-8 BOM prefix** (3 bytes: `0xEF, 0xBB, 0xBF`), uses `EnumTypes` not `Enums`, `Kind` field is integer in newer BC versions
- NuGet feed: `dynamicssmb2.pkgs.visualstudio.com` (NOT `dynamicssmb` — easy typo)
- tree-sitter `braced_block` **excludes action triggers** — text-based fallback in `al_core::syntax::TypeResolver::collect_action_trigger_vars()`
- Without ALTool/.NET SDK: syntax-only features work; no semantic analysis, compilation, or debugging
- `tower-lsp` poisoned locks: use `.unwrap_or_else(|e| e.into_inner())` pattern (already established)
- Semantic bridge: all .NET CLR calls are Mutex-serialized on a blocking thread with 30s timeout
- `InsightGraph` is lazily built — don't assume it exists on first access

---

## Test Infrastructure

### E2E Tests (al-test-harness)

Spawns the real `al-lsp` binary (which lives inside al-core) over stdio. Test fixture: `crates/al-test-harness/data/test_al_project/`.

```rust
let client = LspClient::spawn(project_root).await;  // full handshake, polls workspace/symbol (60s default, AL_TEST_INIT_TIMEOUT to override)
client.open_file("src/MyCodeunit.al").await;          // waits for publishDiagnostics (5s)
```

Test files: `e2e.rs`, `regression.rs`, `real_world.rs`, `zed_fidelity.rs`, `zed_simulation.rs`, `completeness.rs`, `data_driven.rs`, `edit_lifecycle.rs`, `integration_full.rs`, `performance.rs`, `transport.rs`.

### Integration Tests (al-core/tests/)

Tests al-core's `syntax` + `symbols` modules together without LSP transport. Faster, no binary spawn.

### Running Tests

```sh
cargo test --workspace --exclude zed-al           # all tests
cargo test -p al-core                             # the core crate (most tests live here now)
cargo test -p al-core --test e2e                  # single test file
cargo test -p al-core --test e2e -- test_name     # single test
RUST_LOG=debug cargo test -p al-core --test e2e   # with logging
```

---

## Logging

- **stderr**: controlled by `RUST_LOG` env var (e.g., `RUST_LOG=al_core=debug`)
- **File**: `~/.local/share/al-lsp/logs/al-lsp.log` (INFO level, always on)
- **Parent monitoring**: al-lsp exits if its parent process (Zed) dies

---

## Tree-sitter Grammar

`tree-sitter-al/` is a **git submodule** with its own repository and build process.

### What You CAN Edit

- `tree-sitter-al/grammar.js` — the grammar definition
- `tree-sitter-al/generator/tools/al-gen/` — grammar rule generators
- `tree-sitter-al/generator/tools/al-extract/` — AL syntax extraction tools
- `tree-sitter-al/queries/` — highlight, indent, fold, text-object queries
- `tree-sitter-al/data/` — JSON data files loaded by `al_core::syntax::LanguageData`
- `tree-sitter-al/tests/` — test corpus and reference data

### What You MUST NOT Edit

- `tree-sitter-al/src/` — **generated files** (parser.c, etc.) — regenerate instead
- `tree-sitter-al/bindings/` — **generated bindings** — regenerate instead
- `tree-sitter-al/node_modules/` — dependencies

### Submodule Workflow

When you modify files in tree-sitter-al:

1. Make your changes in the submodule directory
2. **Commit and push inside the submodule**: `cd tree-sitter-al && git add -A && git commit -m "..." && git push`
3. **Update the submodule reference in the parent**: `cd .. && git add tree-sitter-al && git commit -m "chore: update tree-sitter-al submodule"`
4. This ensures `git submodule update --init --recursive` will fetch the correct revision

---

## Enforcement System

This project uses automated hooks to enforce quality. You cannot bypass these.

### Stop Gate (runs when you finish)

When you've changed Rust files and try to finish, the stop hook validates:
1. **Compilation** — `cargo check --workspace --exclude zed-al` must pass
2. **Clippy** — `cargo clippy --workspace --exclude zed-al -- -D warnings` must pass
3. **Formatting** — `cargo fmt --all -- --check` must pass

If any check fails, you'll be blocked and must fix the issues before finishing.

### Review Gate (runs when you finish)

After the stop gate passes, the review gate checks:
1. **No hardcoded AL values** in changed files
2. **Tests exist** for significant code changes (>20 lines of source without test changes triggers a warning)
3. **No LSP types in `al_core::queries::*` return types** (the transport boundary, now a coding rule)
4. **No new `.unwrap()` calls** in non-test code

### Test Quality Gate (runs when you write test files)

When you write or edit test files (`*/tests/*.rs`), the hook checks that you have both:
- **Positive tests** — verify correct behavior
- **Negative tests** — verify error handling (`.is_err()`, `.is_none()`, `#[should_panic]`, etc.)

Happy-path-only test files are blocked. Name negative tests clearly: `test_*_invalid_*`, `test_*_missing_*`, `test_*_error_*`.

### Enforced Workflows

Use `/fix-issue` or `/implement` to get a structured workflow that enforces:
1. Understand the problem first (read code, identify root cause)
2. Write a failing test BEFORE implementing the fix
3. Implement the minimal fix
4. Verify with positive AND negative tests
5. Provide a proof-of-work summary

---

## Automatic Workflow Integration (MANDATORY)

Commands, skills, and agents in this project are **not optional tools you wait to be asked to use**. You MUST invoke them automatically when the situation matches. The user should never have to tell you to use them.

### Task-Level Skills (auto-matched by superpowers)

When the user's request matches these patterns, invoke the skill **before doing anything else**:

| User says something like... | Invoke |
|-----------------------------|--------|
| "fix ...", "bug in ...", "broken ..." | `/fix-issue` |
| "add ...", "implement ...", "build ..." | `/implement` |
| "change the grammar", "tree-sitter ..." | `/grammar-change` |
| "add a new query/feature to LSP" | `/add-query` or `/add-feature` |
| "what's the architecture", "how does X work" | `/architecture` |
| "diagnose ...", "not working at runtime" | `/diagnose` |

### During Implementation (invoke automatically at the right step)

| When... | Invoke | Why |
|---------|--------|-----|
| You finished writing code | Spawn **al-tester** agent | Verify tests pass independently |
| You completed all changes | Spawn **al-reviewer** agent | Independent quality review |
| You completed all changes | `/scope-check` | Verify no out-of-scope files touched |
| You modified any `Cargo.toml` | `/dep-check` | Validate dependency direction rules |
| You're about to declare work done | `/review` | Full quality review before finishing |

### Before Committing (enforced by PreToolUse hook)

**Always run `/before-commit` before any `git commit` command.** A PreToolUse hook blocks `git commit` if formatting is not clean — but you should run the full `/before-commit` checklist proactively rather than relying on the hook to catch issues.

### On-Demand Commands

These are invoked when the user explicitly asks, or when the situation clearly calls for them:

| Situation | Command |
|-----------|---------|
| "check everything", "run CI", "validate" | `/check` |
| "find duplicates", "any duplicate code" | `/dedup` |
| "audit", "security check" | `/audit` |
| "test this crate" | `/test-crate <crate>` |
| CI is failing | `/fix-ci` |
| "deep review", "exhaustive review", "ultrareview" | `/review-all` (Review Department) |
| "design a refactor", "architect this task" | `/arch-plan <handoff.json>` |
| "implement these findings", "fix the review output" | `/dev-implement <handoff.json>` |
| "ship this batch", "prepare a PR" | `/release-prep` |
| "run the full loop", "autonomous review→fix→ship" | `/loop` (Operations Overseer) |

---

## Agentic Loop (Review → Arch → Dev → Release)

Heavy-weight workflow for exhaustive, reviewer-led improvement cycles.
`/review-all` replaces what Anthropic's cloud `/ultrareview` used to do,
and the remaining departments close the loop so findings actually get
acted on.

Map: [`.claude/docs/agentic/organization.md`](.claude/docs/agentic/organization.md)

State directory contract:
[`.claude/docs/agentic/state-dir.md`](.claude/docs/agentic/state-dir.md)
(always under `.agentic/<run-id>/`, gitignored).

Key behaviours to know when working with the loop:

- **Everything is file-based.** Departments communicate via JSON/JSONL
  artefacts in `.agentic/<run-id>/`, never via in-memory context.
- **Schemas are versioned.** See [`.claude/docs/agentic/schema-version.md`](.claude/docs/agentic/schema-version.md).
- **Departments are independently invocable.** A human can run
  `/dev-implement` on a handoff file produced last week without
  re-running Review.
- **Nightly Routine.** `review-all-nightly` runs `/review-all` on `dev`
  at 03:00 local. Disable via `/routines`.
- **Persistent human-readable log.** `docs/agentic-log.md` summarises
  every `/loop` run against the branch.

Day-to-day `/review`, `/audit`, `/before-commit` are unchanged — they
are fast gates; the agentic loop is the heavy workflow.

---

## Commit Standards

- Every commit must compile (`cargo check`) and pass tests (`cargo test`)
- Use conventional commits: `feat(crate):`, `fix(crate):`, `refactor(crate):`, `chore:`, `docs:`
- NO `WIP` commits on shared branches
- One logical change per commit — don't bundle unrelated fixes
- Run `cargo fmt --all` and `cargo clippy --workspace --exclude zed-al -- -D warnings` before committing

---

## Common Agent Mistakes (Learn From History)

These have all happened and been reverted. Don't repeat them:

1. **Hardcoding AL keywords** — Use `al_core::syntax::LanguageData` / `al_core::symbols` / `al_core::semantic`
2. **Making out-of-scope changes** — Only touch files related to the task
3. **Breaking the WASM build** — `zed-al` is isolated; don't add native dependencies to root `Cargo.toml`
4. **Changing extension API version** — Stable Zed rejects unreleased API versions
5. **Recursive tree-sitter traversal** — Use iterative with explicit stack
6. **Holding DashMap refs across await** — Clone data, drop ref, then await
7. **Putting `lsp_types::*` in `al_core::queries::*` signatures** — Conversion happens in `al_core::server`
8. **Forgetting UTF-16 conversion** — LSP positions are UTF-16, Rust strings are UTF-8
9. **Editing generated tree-sitter files** — Edit grammar.js/generators, not src/ or bindings/. Commit inside submodule, then update ref
10. **Bundling fixes into mega-commits** — One logical change per commit
