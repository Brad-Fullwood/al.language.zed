# Zed AL Extension

Custom Rust language server for AL (Microsoft Dynamics 365 Business Central) in Zed.

## Architecture

```
al-cli / al-explorer / al-mcp  ->  al-lsp daemon (Unix socket)  ->  al-core  ->  al-syntax
zed-al (WASM)                   ->  al-lsp (stdio)               ->           ->  al-symbols
                                                                              ->  al-semantic
                                                                              ->  al-diag
```

| Crate | Role | Dependencies |
|-------|------|-------------|
| al-lsp | Sole server binary. LSP (stdio) + daemon (Unix socket) | al-core, al-dap, al-protocol |
| al-core | All state, queries, orchestration | al-syntax, al-symbols, al-semantic, al-diag |
| al-protocol | Shared JSON-RPC types. Types only, no logic | serde, std |
| al-syntax | Parser, type resolver, tree-sitter | tree-sitter, std |
| al-symbols | Symbol index for .app packages | serde, std |
| al-semantic | In-process .NET CLR via `netcorehost` | netcorehost, std |
| al-dap | DAP proxy for EditorServices.Host | al-core |
| al-diag | Diagnostic analysis | al-syntax |
| al-cli, al-explorer, al-mcp, zed-al | Thin adapters: pure JSON-RPC clients | al-protocol only |

## Build & Test

```bash
cargo test --workspace --exclude zed-al        # all tests
cargo clippy --workspace --exclude zed-al      # lint
cargo check --workspace --exclude zed-al       # quick compile check
cargo test -p al-core                          # core unit tests only
cargo test -p al-test-harness --test data_driven  # LSP integration tests
```

zed-al requires `wasm32-wasip1` target — always excluded from workspace commands.

## Key Gotchas

- `.app`: 40-byte NAVX header + ZIP. `SymbolReference.json` has UTF-8 BOM, `EnumTypes` not `Enums`.
- NuGet feed: `dynamicssmb2` (NOT `dynamicssmb`).
- tree-sitter `braced_block` excludes action triggers — text-based fallback in TypeResolver.
- Without ALTool: syntax features work, no semantic/compilation/debugging.
- Symbol cache: `~/.cache/al-lsp/packages/` — auto-downloaded from NuGet on first open.
- LSP positions are UTF-16 code units — convert to byte offsets before slicing Rust strings.
- `builtins` behind `RwLock` — use `.read().ok()?` not `.unwrap()` to avoid poison panics.

## Development Workflow

**Zero custom executable code.** Plugins enforce methodology. Agent does the work. Hookify blocks bad patterns.

`/start-work` to begin a session.

### Task lifecycle
1. `/start-work` — compile check, check deferred-issues, find next task from `docs/plan.md`
2. Implement with `superpowers:test-driven-development`
3. `/pof <task_id> <wp_name>` — run tests fresh, write PoF entry with real output
4. Update `docs/progress.md` — mark task `[x]`
5. `/adversarial` — spawn agent that finds bugs, fixes what it can, defers the rest
6. Continue to next task (never stop between tasks)

### Adversarial auto-fix loop
The adversarial agent (background) finds bugs in completed work. It fixes bugs directly when possible. Bugs that need a different WP are appended to `.claude/deferred-issues.toml`. `/start-work` checks deferred issues at session start — when a blocking task completes, deferred bugs become active work.

### New features
`superpowers:brainstorming` → `superpowers:writing-plans` → `superpowers:subagent-driven-development` or `/feature-dev`.

## Enforcement

- **Architecture**: 8 hookify boundary rules block wrong-direction imports on every edit
- **Completion**: 2 hookify Stop rules require `cargo test` and `cargo check` before stopping
- **Methodology**: superpowers enforces TDD, verification-before-completion, and Iron Law
- **Code quality**: hookify warns on bare `Ok(())`
- **Deferred bugs**: `.claude/deferred-issues.toml` tracks bugs that can't be fixed yet — auto-resolved when their blocking task completes

## Skills

| Skill | Purpose |
|-------|---------|
| `/start-work` | Begin session: health check → find task → implement → complete |
| `/adversarial` | Find bugs, fix what's fixable, defer the rest |
| `/pof` | Create Proof of Functionality entry with real test output |
| `/ci` | Infrastructure health check (hookify, compilation, tests, config) |
| `/fix-infra` | Spawn infra-fixer for broken .claude/ files |
| `/report` | Progress report with metrics |
| `/audit` | Deep WP-level audit via supervisor agent |

Also: `/feature-dev`, `/code-review`, `/hookify`, `/commit`, and the full superpowers suite.

## Docs

- `docs/plan.md` — 44 tasks across WP0-WP11 with IDs, deps, pass/fail criteria
- `docs/architecture.md` — full architecture, .NET bridge, error taxonomy
- `docs/crates-map.md` — crate responsibilities and target module layout
- `docs/agentic-schemas.md` — CLI/MCP JSON output schemas
- `docs/proof_of_functionality.toml` — centralized evidence log (adversarial + fidelity passes)
