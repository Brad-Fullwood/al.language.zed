# al-explorer — TUI Symbol Browser (~2K lines)

Full-screen terminal UI for browsing AL symbols. Uses `ratatui` + `crossterm`. All data comes from the al-lsp daemon via JSON-RPC — no dependency on any analysis crate.

## Modules

| File | Purpose |
|------|---------|
| main.rs | TUI entry point, ratatui layout, event loop, keyboard/mouse handling, 4 view modes |
| types.rs | Local symbol types (`SymbolEntry`, `ObjectKind`, `SymbolIndex`, member types) |

## View Modes

`ObjectBrowser` / `EventChain` / `CallGraph` / `Profiler`

## Panes

`Search` / `Packages` / `Objects` / `Details`

## Constraints

- **Edition 2024** — only crate in workspace using it (all others: 2021)
- Types in `types.rs` intentionally duplicate `al-symbols` to avoid compile-time dependency (ISSUE-017)
- Synchronous daemon client — no async runtime
- Dependencies: `al-daemon-client`, `ratatui`, `crossterm`, `serde`/`serde_json`
