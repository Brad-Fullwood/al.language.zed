# al-explorer — TUI Symbol Browser + CLI (~7K lines)

Full-screen terminal UI for browsing AL symbols, plus a clap-based CLI for scripted use. Uses `ratatui` + `crossterm`. All data comes from the al-lsp daemon via `al-protocol` JSON-RPC — no dependency on any analysis crate.

## Quick Reference

```sh
cargo build -p al-explorer                  # build (no tests — UI-only crate)
cargo run -p al-explorer -- --project .     # launch TUI (requires running al-lsp daemon)
```

## Modules

| File | Purpose |
|------|---------|
| main.rs | TUI entry point, ratatui layout, event loop, keyboard/mouse handling, 4 view modes |
| types.rs | Local symbol types (`SymbolEntry`, `ObjectKind`, `SymbolIndex`, member types) |
| cli/ | Scripted command-line subcommands (clap) for non-interactive use |

## View Modes

`ObjectBrowser` / `EventChain` / `CallGraph` / `Profiler`

## Panes

`Search` / `Packages` / `Objects` / `Details`

## Constraints

- **Edition 2024** — only crate in workspace using it (all others: 2021)
- Types in `types.rs` intentionally duplicate `al-core::symbols` to avoid compile-time dependency on al-core
- Synchronous daemon client — no async runtime
- Dependencies: `al-protocol`, `ratatui`, `crossterm`, `clap`, `serde`/`serde_json`
