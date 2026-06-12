# CLAUDE.md

Reference for AI agents working on this codebase. Rules here are non-negotiable.

---

## Quick Reference

```sh
cargo check --workspace --exclude zed-al     # compile check (run before committing)
cargo build --workspace --exclude zed-al     # build all native crates
cargo test  --workspace --exclude zed-al     # run all tests
cargo clippy --workspace --exclude zed-al -- -D warnings
cargo fmt --all
cargo test -p al-core                        # most tests live in al-core
cargo test -p al-core --test e2e             # one test file
cargo build -p zed-al --target wasm32-wasip1 --release  # WASM extension
make build                                    # all Rust crates + .NET bridges
make install                                  # build + symlink into PATH + Zed
```

**Prerequisites:** Rust stable, .NET SDK (auto-downloaded if absent).

**Always exclude `zed-al`** from workspace commands — it targets `wasm32-wasip1` and will fail on the host triple.

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
│  │  al-protocol — daemon IPC types                          │      │
│  └─────────────────────────────────────────────────────────┘      │
│                          ▲                                          │
│                          │ depends on                               │
│  ┌─────────────────────────────────────────────────────────┐      │
│  │  al-explorer — unified TUI + CLI client                  │      │
│  └─────────────────────────────────────────────────────────┘      │
│                                                                     │
│  zed-al (WASM, isolated, no compile-time native deps)              │
└─────────────────────────────────────────────────────────────────────┘
```

### Crate Responsibilities

| Crate | Role | Key Modules / Types |
|-------|------|---------------------|
| **al-core** | All business logic + LSP binary | `Workspace`, `DocumentStore`; `syntax::{AlParser, TypeResolver, LanguageData}`; `symbols::{AppReader, SymbolIndex, NugetClient}`; `semantic::SemanticBridge`; `server::AlServer`; `dap::*`; `[[bin]] al-lsp` |
| **al-protocol** | Daemon IPC types (shared between al-core daemon and al-explorer) | `DaemonClient`, request/response enums |
| **al-explorer** | Unified TUI + CLI client | ratatui app + `cli::*` clap commands |
| **zed-al** | WASM extension for Zed | `AlExtension` |
| **al-test-harness** | E2E tests over the real `al-lsp` binary | `LspClient` |
| **al-zed-test** | Live tests against real Zed | — |

### The One Rule of Architecture

**Query functions in `al-core::queries::*` return transport-agnostic types — never `lsp_types::*`.**
The `al-core::server` module converts to LSP types at the boundary. Not compiler-enforced (al-lsp folded into al-core), so it's a coding discipline. PRs introducing `lsp_types::*` into a `queries::*` function signature will be rejected.

### al-lsp Server Modes (binary lives inside al-core)

| Mode | Arg | Transport | Client |
|------|-----|-----------|--------|
| LSP | `--stdio` (default) | tower-lsp over stdin/stdout | Zed editor |
| Daemon | `daemon --project <path>` | JSON-RPC over Unix socket (al-protocol types) | al-explorer |
| DAP | `--dap` | Debug Adapter Protocol over stdio | Zed debugger |

Socket path: `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock`. Daemon auto-shuts down after 30 min idle.

---

## Dependency Rules

```
al-explorer  ──→  al-protocol
al-core      ──→  al-protocol
zed-al       (isolated, WASM)
```

1. **`al-explorer` depends only on `al-protocol`.** Never on `al-core`. Pulling al-core into the client would drag tree-sitter, the .NET CLR, and tower-lsp into a TUI binary.
2. **`al-core` may depend on `al-protocol`.** Never the reverse — al-protocol must stay a tiny types-only crate.
3. **`zed-al` is completely isolated** — no compile-time dependency on any native crate.
4. **No upward dependencies, no cycles.**

Before adding a dependency, check this rule. If your change would create a wrong-direction dependency, restructure it.

---

## CRITICAL: No Hardcoded Language Values

**NEVER hardcode AL language keywords, built-in functions, object types, or any language-specific lists.**

AL is a living language — Microsoft updates it with every Business Central release. Hardcoded lists become stale immediately.

### Violations

```rust
const AL_BUILTIN_FUNCTIONS: &[&str] = &["Message", "Error", ...];
const AL_KEYWORDS: &[&str] = &["begin", "end", "procedure", ...];
const TRIGGER_NAMES: &[&str] = &["OnRun", "OnValidate", ...];
const AL_DATA_TYPES: &[&str] = &["Integer", "Text", "Code", ...];
let builtins = vec!["Record", "Codeunit", "Page", ...];
match kind { "table" | "page" | "codeunit" => ... }  // matching AL object types
```

### What to use instead

| Need | Source |
|------|--------|
| Keywords, built-in functions, types | `al_core::syntax::LanguageData` (loads from `tree-sitter-al/data/` JSON) |
| Object types, fields, events | `al_core::symbols` (reads `.app` packages at runtime) |
| Semantic info, error codes | `al_core::semantic` bridge (queries .NET CLR at runtime) |

If the extraction pipeline lacks what you need, **update the generator** at `tree-sitter-al/generator/tools/al-extract/` — do not introduce a hardcoded constant.

---

## Scope Discipline

1. **Only change what was requested.** Fixing a bug in `completions.rs` does not license refactoring `hover.rs`, adding docstrings to `definition.rs`, or "cleaning up" unrelated code.
2. **Never rename or restructure files** unless explicitly asked.
3. **Never change public API signatures** unless the task requires it.
4. **Never add dependencies** to Cargo.toml without explicit approval.
5. **Never modify CI/CD** (`.github/workflows/`) without explicit approval.
6. **Never modify the tree-sitter grammar** (`tree-sitter-al/`) — submodule with its own process.
7. **Never create new crates** — the architecture is intentional.
8. **WIP commits are not acceptable** — every commit should compile and pass tests.

Avoid the "while I'm here" anti-pattern. Make a separate PR if you genuinely believe something else needs changing.

---

## Code Patterns

### Position handling (UTF-16 ↔ bytes)

LSP positions are UTF-16 code units. Rust strings are UTF-8 bytes. **Always convert.**

```rust
// CORRECT
let line = rope.line(position.line as usize);
let byte_offset = rope.line_to_byte(position.line as usize)
    + line.utf16_cu_to_byte(position.character as usize);

// WRONG (bug for non-ASCII)
let byte_offset = position.character as usize;
```

### Tree-sitter traversal

Use iterative traversal with an explicit stack, not recursion. AL files can nest deeply enough to blow the stack.

```rust
let mut cursor = node.walk();
let mut stack = vec![node];
while let Some(current) = stack.pop() {
    // process current
    stack.extend(current.named_children(&mut cursor));
}
```

### Error handling

- Query functions (`al_core::queries::*`) return `Option` or `Result` with native types.
- Server handlers (`al_core::server::*`) convert to LSP errors. Never `unwrap()` in a request handler.
- Library modules (`syntax`, `symbols`) use `Result` with `thiserror`. Never panic in library code.

### DashMap access

```rust
// CORRECT — short-lived borrow
let name = workspace.symbols.get(&key).map(|entry| entry.name.clone());

// WRONG — holding a DashMap ref across an await deadlocks the shard
let entry = workspace.symbols.get(&key);
some_async_operation().await;
drop(entry);
```

---

## Key Gotchas

- `.app` files: `SymbolReference.json` has a UTF-8 BOM (3 bytes: `0xEF 0xBB 0xBF`), uses `EnumTypes` not `Enums`, `Kind` is integer in newer BC versions.
- NuGet feed: `dynamicssmb2.pkgs.visualstudio.com` (NOT `dynamicssmb` — easy typo).
- tree-sitter `braced_block` excludes action triggers — text-based fallback in `al_core::syntax::TypeResolver::collect_action_trigger_vars()`.
- Without ALTool / .NET SDK: syntax-only features work; no semantic analysis, compilation, or debugging.
- `tower-lsp` poisoned locks: use `.unwrap_or_else(|e| e.into_inner())` (established pattern).
- Semantic bridge: all .NET CLR calls are Mutex-serialized on a blocking thread with a 30 s timeout.
- `InsightGraph` is lazily built — don't assume it exists on first access.

---

## Test Infrastructure

### E2E tests (`al-test-harness`)

Spawns the real `al-lsp` binary (inside al-core) over stdio. Fixture: `crates/al-test-harness/data/test_al_project/`.

```rust
let client = LspClient::spawn(project_root).await;  // full handshake, polls workspace/symbol (60 s default, AL_TEST_INIT_TIMEOUT to override)
client.open_file("src/MyCodeunit.al").await;        // waits for publishDiagnostics (5 s)
```

### Integration tests (`al-core/tests/`)

Tests `al-core`'s `syntax` + `symbols` modules together without LSP transport. Faster, no binary spawn.

### Running

```sh
cargo test --workspace --exclude zed-al            # all tests
cargo test -p al-core                              # the core crate
cargo test -p al-core --test e2e                   # single test file
cargo test -p al-core --test e2e -- test_name      # single test
RUST_LOG=debug cargo test -p al-core --test e2e    # with logging
```

---

## Logging

- **stderr** — controlled by `RUST_LOG` (e.g. `RUST_LOG=al_core=debug`).
- **File** — `~/.local/share/al-lsp/logs/al-lsp.log` (INFO, always on).
- **Parent monitoring** — al-lsp exits if its parent (Zed) dies.

---

## Tree-sitter Grammar

`tree-sitter-al/` is a **git submodule** with its own repository and build process.

### Edit

- `tree-sitter-al/grammar.js`
- `tree-sitter-al/generator/tools/al-gen/` — rule generators
- `tree-sitter-al/generator/tools/al-extract/` — AL syntax extraction
- `tree-sitter-al/queries/` — highlight, indent, fold, textobject queries
- `tree-sitter-al/data/` — JSON files consumed by `al_core::syntax::LanguageData`
- `tree-sitter-al/tests/` — corpus and reference data

### Do not edit

- `tree-sitter-al/src/` — generated parser (regenerate instead)
- `tree-sitter-al/bindings/` — generated (regenerate instead)
- `tree-sitter-al/node_modules/`

### Submodule workflow

```sh
# inside the submodule
cd tree-sitter-al && git add -A && git commit -m "..." && git push

# then bump the parent reference
cd .. && git add tree-sitter-al && git commit -m "chore: update tree-sitter-al submodule"
```

This ensures `git submodule update --init --recursive` fetches the correct revision.

---

## Validation Standard — "tested" means real projects

Fixture projects and hermetic e2e suites are NOT sufficient evidence that a
user-facing feature works. Fixtures are small, ASCII, space-free, and have
no real symbol packages; an entire batch of basic-operation failures
(format, compile, download symbols — see `Feedback.md`, 2026-06-12) shipped
behind green fixtures.

**Before claiming any user-facing behavior is "tested and working":**

```sh
scripts/validate-real-projects.sh        # the real-project gate (must pass)
AL_VALIDATE_NETWORK=1 scripts/validate-real-projects.sh   # + symbol downloads
```

The gate exercises the exact user entry points (CLI commands as the Zed
tasks invoke them — through the shell, with paths containing spaces) against
real AL projects. Defaults point at local real projects; override with
`AL_VALIDATE_PROJECT_A` / `AL_VALIDATE_PROJECT_B`.

Rules:
1. Test the **user-visible operation** (Zed task / TUI keypress / CLI
   invocation), not just the underlying library function.
2. Any new path-handling code must be exercised with a path containing
   spaces.
3. A feature that fails this gate is **broken**, regardless of unit-test
   coverage. Never report it otherwise.

---

## Commit Standards

- Every commit must compile (`cargo check`) and pass tests (`cargo test`).
- Conventional commits: `feat(crate):`, `fix(crate):`, `refactor(crate):`, `chore:`, `docs:`.
- No WIP commits on shared branches.
- One logical change per commit — don't bundle unrelated fixes.
- Run `cargo fmt --all` and `cargo clippy --workspace --exclude zed-al -- -D warnings` before committing.

---

## Common Agent Mistakes (Learn From History)

1. **Hardcoding AL keywords** — use `al_core::syntax::LanguageData` / `al_core::symbols` / `al_core::semantic`.
2. **Out-of-scope changes** — only touch files related to the task.
3. **Breaking the WASM build** — `zed-al` is isolated; don't add native deps to root `Cargo.toml`.
4. **Changing extension API version** — stable Zed rejects unreleased API versions.
5. **Recursive tree-sitter traversal** — use iterative with explicit stack.
6. **Holding DashMap refs across await** — clone, drop, then await.
7. **`lsp_types::*` in `al_core::queries::*` signatures** — conversion happens in `al_core::server`.
8. **Forgetting UTF-16 conversion** — LSP positions are UTF-16, Rust strings are UTF-8.
9. **Editing generated tree-sitter files** — edit `grammar.js` / generators, regenerate, commit in submodule, then bump parent ref.
10. **Mega-commits** — one logical change per commit.
