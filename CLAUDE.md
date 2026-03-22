# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

All native crates exclude the WASM extension (`zed-al`) which requires `wasm32-wasip1`:

```sh
cargo check --workspace --exclude zed-al     # quick compile check
cargo build --workspace --exclude zed-al     # build all native crates
cargo test --workspace --exclude zed-al      # run all tests
cargo clippy --workspace --exclude zed-al    # lint
cargo fmt --all                               # format (excludes zed-al automatically)
cargo test -p al-syntax                      # test a single crate
cargo test -p al-lsp --test e2e             # run a single test file
cargo build -p zed-al --target wasm32-wasip1 --release  # WASM extension
```

`make build` builds all Rust crates + .NET bridges. `make install` also symlinks binaries into `~/.local/bin` and the extension into Zed's extension directory.

**Prerequisites:** Rust stable toolchain, .NET SDK (auto-downloaded if not present).

## Architecture

```
zed-al (WASM extension)  →  al-lsp (stdio)              →  al-core  →  al-syntax
al-cli / al-explorer     →  al-lsp daemon (Unix socket)           →  al-symbols
al-mcp                   →  al CLI binary (subprocess)            →  al-semantic
                                                          al-core →  al-dap-client
```

**al-lsp** is the sole server binary with three modes selected by args:
- **LSP mode** (`--stdio`, default): tower-lsp over stdio, how Zed connects
- **Daemon mode** (`daemon --project <path>`): JSON-RPC over Unix socket at `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock`, serves CLI and explorer. Auto-shuts down after 30min idle.
- **DAP mode** (`--dap`): Debug Adapter Protocol for AL debugging

**al-core** owns all state via `Workspace`: `DocumentStore` (rope + parse tree per file), `SymbolIndex` (DashMap-backed), `SemanticBridge` (.NET CLR), `FileIndex`, `InsightGraph`. All query functions in `al-core/src/queries/` take `&Workspace` + position and return transport-agnostic types. al-lsp converts to LSP types at the boundary.

**al-symbols** parses `.app` files (40-byte NAVX header + ZIP containing `SymbolReference.json`) and builds the symbol index. Symbols auto-downloaded from NuGet on first open, cached at `~/.cache/al-lsp/packages/`.

**al-semantic** hosts the .NET CLR in-process via `netcorehost` for CodeAnalysis integration. All CLR calls are Mutex-serialized on a blocking thread with 2s timeout.

**al-daemon-client** contains shared IPC types (socket path computation, JSON-RPC types, `DaemonClient`). Used by al-cli, al-explorer, and al-core.

**zed-al** is a standalone WASM crate implementing `zed_extension_api::Extension`. No compile-time dependency on native crates.

**al-mcp** shells out to the `al` CLI binary — no compile-time dependency on any al-* crate.

## Dependency Rules

Dependencies flow downward only:

```
al-lsp → al-core → al-syntax (parsing, formatting, type resolution)
                 → al-symbols (symbol index, .app reading, NuGet)
                 → al-semantic (.NET bridge)
                 → al-dap-client (debug adapter)
                 → al-daemon-client (IPC types)
```

**al-syntax**, **al-symbols**, and **al-semantic** must never depend on each other or on al-core. al-daemon-client must not depend on al-core. al-mcp and zed-al are isolated.

## Key Gotchas

- `.app` files: `SymbolReference.json` has UTF-8 BOM prefix, uses `EnumTypes` not `Enums`, `Kind` field is integer in newer BC versions
- NuGet feed hostname: `dynamicssmb2.pkgs.visualstudio.com` (NOT `dynamicssmb`)
- tree-sitter `braced_block` excludes action triggers — text-based fallback in `TypeResolver::collect_action_trigger_vars()`
- Without ALTool/.NET SDK: syntax-only features work; no semantic analysis, compilation, or debugging
- LSP positions are UTF-16 code units — convert to byte offsets before slicing Rust strings
- `RwLock` poison recovery: use `.read().ok()?` pattern, not `.unwrap()`
- Cargo doesn't always detect transitive dependency changes — `touch` source files to force rebuild

## Concurrency Patterns

- `tokio::sync::RwLock` for async-context fields (toolchain, project, config)
- `tokio::sync::Mutex` for semantic bridge (exclusive CLR access)
- `std::sync::RwLock` for sync-only fields (builtins, package_info) — never held across `.await`
- `DashMap` for all concurrent map lookups (symbol index, documents, file index)

## Test Infrastructure

End-to-end tests use `al-test-harness` which spawns the real `al-lsp` binary over stdio:
- `LspClient::spawn(project_root)` — full LSP handshake, polls `workspace/symbol` up to 30s for readiness
- `open_file()` waits for `publishDiagnostics` (5s timeout)
- Test fixture project: `crates/al-test-harness/data/test_al_project/`
- E2E test files: `crates/al-test-harness/tests/` (e2e.rs, regression.rs, real_world.rs, zed_fidelity.rs, etc.)

Integration tests in `crates/al-lsp/tests/integration.rs` test al-syntax + al-symbols together without LSP transport.

## Logging

al-lsp logs to both stderr (`RUST_LOG` env var) and `~/.local/share/al-lsp/logs/al-lsp.log` (INFO level). Parent process monitoring exits al-lsp if its parent (Zed) dies.

## Tree-sitter Grammar

`tree-sitter-al/` contains a custom AL tree-sitter grammar owned by this project. Key rule: **never edit generated output files directly** — always modify the generators and let them produce the output.

- `grammar.js` — the grammar definition
- `queries/` — highlight, indent, fold, and text-object queries
- `generator/tools/al-gen/` — generates grammar rules from AL syntax docs
- `generator/tools/al-extract/` — extracts AL syntax information for the generator
- `tests/` and `data/` — test corpus and reference data

When AL syntax changes or the grammar needs updating, modify the generator tools or `grammar.js`, then regenerate.
