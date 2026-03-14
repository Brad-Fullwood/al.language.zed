---
paths:
  - "crates/al-cli/**"
  - "crates/al-explorer/**"
  - "crates/al-mcp/**"
  - "crates/zed-al/**"
---
# Thin Adapter Enforcement

You are editing a **thin adapter** crate. These rules are non-negotiable.

## Forbidden
- `al-core`, `al-syntax`, `al-symbols`, or `al-semantic` in `Cargo.toml` dependencies.
- `use al_core::`, `use al_syntax::`, `use al_symbols::`, `use al_semantic::` in any .rs file.
- Any hover, completion, formatting, symbol resolution, or analysis logic.
- Constructing analysis results — adapters forward JSON-RPC responses only.

## Allowed Content Per Crate
| Crate | Allowed |
|---|---|
| al-cli | Argument parsing (clap), daemon connection, JSON-RPC, output formatting, exit codes |
| al-explorer | Daemon connection, JSON-RPC streaming, TUI rendering (ratatui), keyboard input |
| al-mcp | Daemon connection, MCP-to-JSON-RPC translation, MCP tool registration |
| zed-al | `zed::Extension` trait, `process::Command` to spawn al-lsp, slash command formatting |

## Verification
Run `/check` or the guardian agent to verify: `cargo tree` shows no al-core dependency, `grep` finds no forbidden imports.
