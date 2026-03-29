# al-cli — CLI Front-End (~4K lines)

Binary name: `al` (not `al-cli`). Zero business logic — every command is a thin wrapper that calls `DaemonClient::request(method, params)`.

## Modules

| File | Purpose |
|------|---------|
| main.rs | Entry point, `Cli` (clap), `Commands` enum (~50 subcommands), dispatch |
| commands/mod.rs | Shared helpers: `connect()`, `run_command()`, `project_root()`, `print_json()` |
| commands/lsp.rs | LSP-facing commands (hover, definition, completions, lint, format, rename, etc.) |
| commands/build.rs | Compile/package and XLF translation sub-commands |
| commands/debug.rs | Debug session, snapshot, CPU profile sub-commands |
| commands/insight.rs | Trace, entrypoints, graph, dead code, impact, suggest_event |

## Adding a New Command

1. Add variant to `Commands` enum in `main.rs`
2. Add the match arm calling `DaemonClient::request("method", params)`
3. That's it — all logic lives in al-core, exposed through the daemon

## Gotchas

- `Lint` command joins multi-part `file` args with space to handle Zed's `$ZED_FILE` splitting
- `run_command()` covers the happy path; commands with non-standard exit codes remain manual
