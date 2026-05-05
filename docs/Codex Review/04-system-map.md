# System Map

Status: restored 2026-05-04 from the completed fresh pass.

## Current Workspace Shape

The current manifests show a consolidated architecture:

- Root package: `zed-al`, a `cdylib` WASM Zed extension (`Cargo.toml:6`, `Cargo.toml:12`).
- Workspace crates: every direct child of `crates/*` except excluded paths (`Cargo.toml:1`).
- `al-core`: native core plus `al-lsp` binary (`crates/al-core/Cargo.toml:7`).
- `al-protocol`: shared daemon IPC data/client crate.
- `al-explorer`: TUI plus CLI commands, depends on `al-protocol`.
- `al-test-harness`: E2E tests over `al-lsp`.
- `al-zed-test`: standalone live Zed automation helpers.

## Runtime Paths

- Zed extension host:
  - Root `zed-al` WASM extension registers language server `al-lsp`, debug adapter `al`, debug locator `al`, snippets, schemas, themes, and grammar metadata.
  - `src/lib.rs` resolves the `al-lsp` binary through configured path, legacy proxy/cached/PATH/download logic, then supplies initialization options to `al-lsp`.
  - `src/dap.rs` maps Zed debug configs to `al-lsp --dap` over stdio.
- Native LSP:
  - `crates/al-core/src/bin/al-lsp.rs` is the native binary.
  - Normal LSP mode serves Zed over stdio.
  - Daemon mode serves line-delimited JSON-RPC-like messages over Unix sockets using `al-protocol`.
  - DAP modes support native BC debug and EditorServices proxy flows.
- CLI/TUI:
  - `al-explorer` is both TUI and CLI client.
  - The old `al-cli` command surface is folded into `al-explorer`.
  - `al-protocol::DaemonClient` is Unix-only, so current `al-explorer` is effectively Unix-only despite CI/release Windows matrices.
  - Daemon autostart is client-side and currently lacks a per-project startup lock.
  - The daemon and CLI share an absolute-path contract, but several CLI commands still forward relative user input unchanged.
- Parser/grammar:
  - Native `al-core` builds from `tree-sitter-al`.
  - Zed grammar metadata pins an external revision in `extension.toml`.
  - `grammars/al` is a checked-in grammar asset copy.
  - Generator flow lives under `tree-sitter-al/generator`.
- Semantic bridge:
  - `al-core` includes a C# bridge project under `crates/al-core/bridge`.
  - The Rust side can call CodeAnalysis for semantic diagnostics, hover/completion fallback, built-ins, and compiler-related metadata.

## Validation Baseline

- Native workspace check passes.
- WASM extension check/build passes.
- Formatting passes.
- Full workspace tests fail at `al-test-harness` hover parameter integration.
- Strict Clippy fails for current Rust `1.95.0`.
- Windows `al-explorer` target check fails because daemon client is Unix-only.
- Direct `tree-sitter build` from `tree-sitter-al` fails because `src/grammar.json` is missing; generator validation passes in a temp copy.
- Microsoft AL tool is installed and runnable, but `al-core` discovery still reports ALTool missing in LSP startup logs.

## Documentation Drift Noted

`CLAUDE.md` appears to be the current architecture source: it says `al-lsp`, `al-syntax`, `al-symbols`, and `al-semantic` are folded into `al-core` modules (`CLAUDE.md:70`, `CLAUDE.md:79`, `CLAUDE.md:109`). `README.md` still presents several of those as separate crates (`README.md:125` through `README.md:140`) and shows older dependency rules (`README.md:160` through `README.md:173`). This is captured as F-035.

Additional drift captured in detailed findings:

- Release workflow still references removed package names.
- README and Zed tasks still assume this repo owns the plain `al` command name.
- Debug schema/snippets expose request and setting surfaces not actually routed by the adapter.
- CLI cache docs and implementation disagree with the daemon `clearCache` endpoint and active cache directory.
