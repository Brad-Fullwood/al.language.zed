# System Map

Status: in progress.

## Current Workspace Shape

The current manifests show a consolidated architecture:

- Root package: `zed-al`, a `cdylib` WASM Zed extension (`Cargo.toml:6`, `Cargo.toml:12`).
- Workspace crates: every direct child of `crates/*` except excluded paths (`Cargo.toml:1`).
- `al-core`: native core plus `al-lsp` binary (`crates/al-core/Cargo.toml:7`).
- `al-protocol`: shared daemon IPC data/client crate.
- `al-explorer`: TUI plus CLI commands, depends on `al-protocol`.
- `al-test-harness`: E2E tests over `al-lsp`.
- `al-zed-test`: standalone live Zed automation helpers.

## Documentation Drift Noted

`CLAUDE.md` appears to be the current architecture source: it says `al-lsp`, `al-syntax`, `al-symbols`, and `al-semantic` are folded into `al-core` modules (`CLAUDE.md:70`, `CLAUDE.md:79`, `CLAUDE.md:109`). `README.md` still presents several of those as separate crates (`README.md:125` through `README.md:140`) and shows older dependency rules (`README.md:160` through `README.md:173`). This is tracked as review context and may become a finding after checking whether user-facing docs are expected to reflect the current refactor.
