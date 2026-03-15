# Zed AL Extension

Custom Rust language server for AL (Microsoft Dynamics 365 Business Central) in Zed.

## Architecture

```
al-cli / al-explorer / al-mcp  ->  al-lsp daemon (Unix socket)  ->  al-core  ->  al-syntax
zed-al (WASM)                   ->  al-lsp (stdio)               ->           ->  al-symbols
                                                                              ->  al-semantic
                                                                              ->  al-diag
                                                                              ->  al-dap-client
```

| Crate | Role |
|-------|------|
| al-lsp | Sole server binary. LSP (stdio) + daemon (Unix socket) |
| al-core | All state, queries, orchestration |
| al-protocol | Shared JSON-RPC types. Types only, no logic |
| al-syntax | Parser, type resolver, tree-sitter |
| al-symbols | Symbol index for .app packages |
| al-semantic | In-process .NET CLR via `netcorehost` |
| al-dap-client | AL debug engine. Headless DAP control of EditorServices.Host |
| al-diag | Diagnostic tracing and logging (SQLite-backed) |
| al-test-harness | LSP integration + data-driven tests (dev only) |
| al-cli, al-explorer, al-mcp, zed-al | Thin adapters: pure JSON-RPC clients |

Build commands: `.claude/rules/testing.md`. Dependency rules: `.claude/rules/code-boundaries.md`.

## Key Gotchas

- `.app`: 40-byte NAVX header + ZIP. `SymbolReference.json` has UTF-8 BOM, `EnumTypes` not `Enums`.
- NuGet feed: `dynamicssmb2` (NOT `dynamicssmb`).
- tree-sitter `braced_block` excludes action triggers — text-based fallback in TypeResolver.
- Without ALTool: syntax features work, no semantic/compilation/debugging.
- Symbol cache: `~/.cache/al-lsp/packages/` — auto-downloaded from NuGet on first open.
- LSP positions are UTF-16 code units — convert to byte offsets before slicing Rust strings.
- `builtins` behind `RwLock` — use `.read().ok()?` not `.unwrap()` to avoid poison panics.

## Development Workflow

`/start-work` to begin a session. It handles everything: health check, task selection, implementation, completion, and looping to the next task.

### What you can tell the agent

| You want to... | Say |
|---|---|
| Start implementing tasks | `/start-work` |
| Check if the project is healthy | `/check-health` |
| See what's done and what's next | `/show-progress` |
| Log a bug or issue | `/report-issue <description>` |
| Work on fixing open issues | `/fix-issues` or `/fix-issues ISSUE-013` |
| Stress-test specific code | `/find-bugs` |

### How the workflow runs (automatic)

1. `/start-work` — health check (STOP issues, compilation, boundaries, deferred bugs), find next task
2. Agent implements with TDD
3. `/complete-task` — tests, proof, progress update, bug finder (called automatically)
4. At WP boundaries, `/review-milestone` runs automatically before starting the next WP
5. Loop back to step 1

### New features
`superpowers:brainstorming` → `superpowers:writing-plans` → `superpowers:subagent-driven-development`.

## Enforcement

- **Architecture**: 8 hookify boundary rules block wrong-direction imports on every edit (including al-protocol in analysis libs)
- **Completion**: 3 hookify stop rules warn if stopping without `cargo test`, `cargo check`, or proof evidence
- **Workflow**: hookify warns on direct tasks.toml completion edits
- **Methodology**: superpowers enforces TDD, verification-before-completion
- **Code quality**: hookify bare `Ok(())` rule (currently disabled — too many false positives)
- **Session start**: `/start-work` checks STOP issues, compilation, deferred bugs, and boundary violations
- **WP gate**: `/start-work` auto-reviews completed WPs before starting the next
- **Deferred bugs**: `.claude/data/issues.toml` tracks bugs — auto-resolved when blocking task completes

## Data Files

All agent data in `.claude/data/` (TOML):

| File | Contents | Discovered via |
|---|---|---|
| `tasks.toml` | Tasks, WPs, completion status | `/start-work`, `/complete-task`, `/show-progress` |
| `issues.toml` | Issues + deferred bugs | `/fix-issues`, `/report-issue`, `/start-work`, adversarial agent |
| `proof.toml` | Proof of functionality evidence | `/complete-task`, `/review-milestone` |
| `schemas.toml` | CLI/MCP JSON output schemas | `agentic-output` rule (when implementing CLI commands) |
| `features.toml` | Feature scope, command mappings | `code-boundaries` rule (when checking scope) |
| `settings.toml` | MS VS Code → Zed settings mapping | `agentic-output` rule (when implementing settings) |
| `adversarial-atlas.toml` | Stress tests + fidelity gaps | adversarial agent (test catalog) |
| `task-index.toml` | Task lookup index | `/start-work` (auto-generated) |
