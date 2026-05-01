# zed-al — WASM Extension (~720 lines)

The Zed IDE WASM extension. Compiled to `cdylib` targeting `wasm32-wasip1`. Bridges Zed and the `al-lsp` language server.

## Quick Reference

```sh
cargo build -p zed-al --target wasm32-wasip1 --release  # build WASM extension
make install                                              # build + symlink into Zed
```

## Modules

| File | Purpose |
|------|---------|
| lib.rs | `AlExtension` (implements `zed::Extension`), `merge_json()`, extension registration |
| dap.rs | DAP adapter integration: `get_dap_binary()`, `dap_request_kind()`, `dap_config_to_scenario()` |
| discovery.rs | `find_proxy_path()` — looks for bundled proxy binary |
| platform.rs | Platform/architecture helpers for asset name construction |
| settings.rs | `apply_al_settings_to_config()` — maps Zed settings to al-lsp init options |

## Binary Resolution (language_server_command)

4-step priority: (1) user-configured path, (2) cached download, (3) `PATH` lookup, (4) GitHub release download

## Critical Constraints

- **WASM sandbox** — no `std::process`, no filesystem writes, no syscalls unavailable in WASM
- **Completely isolated** — no compile-time dependency on any native workspace crate
- `merge_json()` is inlined (not shared) because sharing would require a WASM-compatible dep
- `zed_extension_api` is a git dep on Zed's `main` branch — breaking changes upstream break this with no warning
- Build: `cargo build -p zed-al --target wasm32-wasip1 --release`
- **Never include in workspace commands** — always `--exclude zed-al`
