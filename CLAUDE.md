# CLAUDE.md

This file is the **single source of truth** for AI agents working on this codebase. Every rule here is non-negotiable. Violating these rules wastes human review time and creates revert commits.

---

## Quick Reference

```sh
cargo check --workspace --exclude zed-al     # compile check (ALWAYS run before committing)
cargo build --workspace --exclude zed-al     # build all native crates
cargo test  --workspace --exclude zed-al     # run all tests
cargo clippy --workspace --exclude zed-al -- -D warnings  # lint (must pass CI)
cargo fmt --all                               # format
cargo test -p al-syntax                      # test a single crate
cargo test -p al-lsp --test e2e             # single test file
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
│ ENTRY POINTS (3 transports, same business logic)                    │
│                                                                     │
│  zed-al (WASM)  →  al-lsp --stdio    ┐                            │
│  al-cli         →  al-lsp daemon      ├→  al-core (ALL logic)     │
│  al-explorer    →  al-lsp daemon      │      ├→ al-syntax          │
│                                       │      ├→ al-symbols         │
│                                       │      ├→ al-semantic        │
│                                       │      ├→ al-dap-client      │
│                                       │      └→ al-daemon-client   │
│                                       │                             │
│                                       └─→ al-daemon-client (IPC)   │
└─────────────────────────────────────────────────────────────────────┘
```

### Crate Responsibilities

| Crate | Lines | Role | Key Types |
|-------|-------|------|-----------|
| **al-core** | ~32K | ALL business logic, queries, state | `Workspace`, `DocumentStore`, `SymbolIndex`, `InsightGraph` |
| **al-syntax** | ~9K | Parsing, formatting, linting, type resolution | `AlParser`, `TypeResolver`, `LanguageData` |
| **al-symbols** | ~6K | `.app` file reading, NuGet, symbol index | `AppReader`, `SymbolIndex`, `NugetClient` |
| **al-semantic** | ~1K | .NET CLR bridge (CodeAnalysis) | `SemanticBridge` |
| **al-lsp** | ~7K | LSP/daemon/DAP server (transport only) | `AlServer`, daemon handlers |
| **al-dap-client** | ~3K | Debug Adapter Protocol client | `BcDebugSession`, DAP framing |
| **al-daemon-client** | ~450 | Shared IPC types | `DaemonClient`, socket path |
| **al-cli** | ~4K | CLI tool | clap commands |
| **al-explorer** | ~2K | TUI symbol browser | ratatui app |
| **zed-al** | ~340 | WASM extension for Zed | `AlExtension` |

### The One Rule of Architecture

**All business logic lives in `al-core`.** The query functions in `al-core/src/queries/` take `&Workspace` + position and return **transport-agnostic types**. `al-lsp` converts results to LSP types at the boundary. If you're writing logic that manipulates AL code, symbols, or project state, it goes in `al-core`, not `al-lsp`.

### al-lsp Server Modes

| Mode | Arg | Transport | Client |
|------|-----|-----------|--------|
| LSP | `--stdio` (default) | tower-lsp over stdin/stdout | Zed editor |
| Daemon | `daemon --project <path>` | JSON-RPC over Unix socket | al-cli, al-explorer |
| DAP | `--dap` | Debug Adapter Protocol over stdio | Zed debugger |

Socket path: `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock`. Daemon auto-shuts down after 30min idle.

---

## Dependency Rules (ENFORCED)

```
al-lsp ──→ al-core ──→ al-syntax
                    ──→ al-symbols
                    ──→ al-semantic
                    ──→ al-dap-client
                    ──→ al-daemon-client
```

### Hard Constraints

1. **al-syntax, al-symbols, al-semantic** must NEVER depend on each other or on al-core
2. **al-daemon-client** must NEVER depend on al-core
3. **zed-al** is completely isolated — no compile-time dependency on any native crate
4. Dependencies flow **downward only** — no cycles, no upward imports
5. **al-lsp** must not contain business logic — only transport conversion

**Before adding a dependency**, check this table. If your change would create an upward or lateral dependency, restructure it.

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
| Keywords, built-in functions, types | `al-syntax::LanguageData` (loads from `tree-sitter-al/data/` JSON files) |
| Object types, fields, events | `al-symbols` (reads `.app` packages at runtime) |
| Semantic info, error codes | `al-semantic` bridge (queries .NET CLR at runtime) |

If the extraction pipeline lacks what you need, **update the generator** at `tree-sitter-al/generator/tools/al-extract/` — do NOT create a hardcoded constant.

### Known Existing Violations (to be fixed)

- `crates/al-syntax/src/symbols.rs:430` — `PAGE_CONTROL_KEYWORDS` (use `language_data::page_controls()`)
- `crates/al-syntax/src/formatting.rs:419` — `SINGLE_STMT_OPENERS` (use `language_data::keywords().control`)

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
// In query functions (al-core/src/queries/): return Option or Result
pub fn hover(workspace: &Workspace, uri: &Url, pos: Position) -> Option<HoverResult> { ... }

// In server handlers (al-lsp): convert to LSP errors
// NEVER unwrap() in request handlers — return an error response

// In library code (al-syntax, al-symbols): use Result with thiserror
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

## Known Bugs (from code review 2026-03-29)

See `docs/CODE_REVIEW.md` for full details. Key bugs to be aware of:

1. **UTF-16 position bug** — `al-syntax/src/navigation.rs:9` and `type_resolver.rs:181` pass `Position.character` (UTF-16) directly as byte offset to tree-sitter. Breaks on non-ASCII.
2. **Stale line array in rename** — `al-cli/src/commands/lsp.rs:889` computes line offsets once then mutates content. Multi-edit rename corrupts files.
3. **Blocking I/O in async** — `al-lsp/src/daemon/mod.rs:568` uses `std::fs::read_to_string` in async handler. Blocks tokio.
4. **LSP types in al-core** — `al-core/src/file_index.rs:93` and `resolution.rs` use `tower_lsp::lsp_types` in core data structures.
5. **process::exit in tokio tasks** — `al-lsp/src/main.rs:39,74` bypasses Drop destructors.

---

## Key Gotchas

- `.app` files: `SymbolReference.json` has **UTF-8 BOM prefix** (3 bytes: `0xEF, 0xBB, 0xBF`), uses `EnumTypes` not `Enums`, `Kind` field is integer in newer BC versions
- NuGet feed: `dynamicssmb2.pkgs.visualstudio.com` (NOT `dynamicssmb` — easy typo)
- tree-sitter `braced_block` **excludes action triggers** — text-based fallback in `TypeResolver::collect_action_trigger_vars()`
- Without ALTool/.NET SDK: syntax-only features work; no semantic analysis, compilation, or debugging
- `tower-lsp` poisoned locks: use `.unwrap_or_else(|e| e.into_inner())` pattern (already established)
- Semantic bridge: all .NET CLR calls are Mutex-serialized on a blocking thread with 30s timeout
- `InsightGraph` is lazily built — don't assume it exists on first access

---

## Test Infrastructure

### E2E Tests (al-test-harness)

Spawns the real `al-lsp` binary over stdio. Test fixture: `crates/al-test-harness/data/test_al_project/`.

```rust
let client = LspClient::spawn(project_root).await;  // full handshake, polls workspace/symbol 30s
client.open_file("src/MyCodeunit.al").await;          // waits for publishDiagnostics (5s)
```

Test files: `e2e.rs`, `regression.rs`, `real_world.rs`, `zed_fidelity.rs`, `zed_simulation.rs`, `completeness.rs`, `data_driven.rs`, `edit_lifecycle.rs`, `integration_full.rs`, `performance.rs`, `transport.rs`.

### Integration Tests (al-lsp/tests/)

Tests al-syntax + al-symbols together without LSP transport. Faster, no binary spawn.

### Running Tests

```sh
cargo test --workspace --exclude zed-al           # all tests
cargo test -p al-core                             # single crate
cargo test -p al-lsp --test e2e                  # single test file
cargo test -p al-lsp --test e2e -- test_name     # single test
RUST_LOG=debug cargo test -p al-lsp --test e2e   # with logging
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
- `tree-sitter-al/data/` — JSON data files loaded by `al-syntax::LanguageData`
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

## Commit Standards

- Every commit must compile (`cargo check`) and pass tests (`cargo test`)
- Use conventional commits: `feat(crate):`, `fix(crate):`, `refactor(crate):`, `chore:`, `docs:`
- NO `WIP` commits on shared branches
- One logical change per commit — don't bundle unrelated fixes
- Run `cargo fmt --all` and `cargo clippy --workspace --exclude zed-al -- -D warnings` before committing

---

## Common Agent Mistakes (Learn From History)

These have all happened and been reverted. Don't repeat them:

1. **Hardcoding AL keywords** — Use `LanguageData` / `al-symbols` / `al-semantic`
2. **Making out-of-scope changes** — Only touch files related to the task
3. **Breaking the WASM build** — `zed-al` is isolated; don't add native dependencies to root `Cargo.toml`
4. **Changing extension API version** — Stable Zed rejects unreleased API versions
5. **Recursive tree-sitter traversal** — Use iterative with explicit stack
6. **Holding DashMap refs across await** — Clone data, drop ref, then await
7. **Adding business logic to al-lsp** — All logic goes in al-core queries
8. **Forgetting UTF-16 conversion** — LSP positions are UTF-16, Rust strings are UTF-8
9. **Editing generated tree-sitter files** — Edit grammar.js/generators, not src/ or bindings/. Commit inside submodule, then update ref
10. **Bundling fixes into mega-commits** — One logical change per commit
