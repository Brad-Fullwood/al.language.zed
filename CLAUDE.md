# Zed AL Extension

Custom Rust language server for AL (Microsoft Dynamics 365 Business Central) in Zed.

## Architecture

```
al-cli / al-explorer / al-mcp  ->  al-lsp daemon (Unix socket)  ->  al-core  ->  al-syntax
zed-al (WASM)                   ->  al-lsp (stdio)               ->           ->  al-symbols
                                                                              ->  al-semantic
                                                                              ->  al-diag
```

- **al-lsp**: Sole server binary. LSP (stdio) + daemon (Unix socket). Routes to al-core.
- **al-core**: All state, queries, orchestration. Only al-lsp imports it.
- **al-protocol**: JSON-RPC types shared by al-lsp and thin adapters. Types only.
- **al-syntax**: Parser, type resolver, tree-sitter integration. Standalone.
- **al-symbols**: Symbol index for .app packages (SymbolReference.json). Standalone.
- **al-semantic**: In-process .NET CLR via `netcorehost`. NOT a subprocess.
- **al-dap**: Debug adapter protocol proxy for EditorServices.Host.
- **al-diag**: Diagnostic analysis. May depend on al-syntax only.
- **Thin adapters** (al-cli, al-explorer, al-mcp, zed-al): Pure JSON-RPC clients. Zero al-core dependency.

## Build & Test

```bash
cargo test --workspace --exclude zed-al
cargo clippy --workspace --exclude zed-al
cargo check --workspace --exclude zed-al  # quick compile check
```

zed-al requires WASM target — excluded from workspace commands.

## Key Gotchas

- `.app`: 40-byte NAVX header + ZIP. `SymbolReference.json` has UTF-8 BOM, `EnumTypes` not `Enums`.
- NuGet feed: `dynamicssmb2` (NOT `dynamicssmb`).
- tree-sitter `braced_block` excludes action triggers — text-based fallback in TypeResolver.
- Without ALTool: syntax features work, no semantic/compilation/debugging.
- Symbol cache: `~/.cache/al-lsp/packages/` — auto-downloaded from NuGet on first open.

## Development Workflow

**Zero custom executable code.** Plugins enforce methodology. Agent does the work. Hookify blocks bad patterns.

`/start-work` to begin a session.

### Task lifecycle
1. `/start-work` — compile check, find next task from `docs/plan.md`
2. Implement with `superpowers:test-driven-development`
3. `/pof <task_id> <wp_name>` — run tests fresh, write PoF entry with real output
4. Update `docs/progress.md` — mark task `[x]`
5. `/adversarial` — spawn edge-case testing in background
6. Continue to next task (never stop between tasks)

### New features
`superpowers:brainstorming` → `superpowers:writing-plans` → `superpowers:subagent-driven-development` or `/feature-dev`.

## Enforcement

- **Architecture**: 8 hookify boundary rules block wrong-direction imports on every edit
- **Completion**: 2 hookify Stop rules require `cargo test` and `cargo check` before stopping
- **Methodology**: superpowers enforces TDD, verification-before-completion, and Iron Law
- **Code quality**: hookify warns on bare `Ok(())`

## Skills

`/start-work`, `/adversarial`, `/pof`, `/ci`, `/fix-infra`, `/report`, `/audit`

Also: `/feature-dev`, `/code-review`, `/hookify`, and the full superpowers suite.

## Docs

- `docs/plan.md` — task sequences with IDs, deps, pass/fail criteria
- `docs/architecture.md` — full architecture, .NET bridge, error taxonomy
- `docs/crates-map.md` — crate responsibilities and target module layout
- `docs/agentic-schemas.md` — CLI/MCP JSON output schemas
