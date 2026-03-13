# Release and Packaging

This document defines the build, packaging, and release flow for the Zed AL extension.

## Build Prerequisites
1. Rust toolchain (stable).
2. `wasm32-wasip2` target installed: `rustup target add wasm32-wasip2`.
3. .NET SDK and AL toolchain installed for semantic features.

## Build Steps
1. Build binaries:
   1. `cargo build -p al-lsp --release`
   2. `cargo build -p al-cli --release`
   3. `cargo build -p al-explorer --release`
2. Build Zed extension WASM:
   1. `cargo build -p zed-al --target wasm32-wasip2 --release`
3. Regenerate grammar assets (if needed):
   1. Run the `tree-sitter-al` generator.
   2. Sync queries to `languages/al`.

## Packaging
1. Publish `al-lsp`, `al-cli`, and `al-explorer` binaries as release artifacts.
2. Publish `extension.wasm` and `extension.toml`.
3. Record version compatibility between `zed-al` and `al-lsp`.

## Release Checklist
1. All tests pass (see `plan.md` Phase 6).
2. Grammar assets regenerated and committed.
3. Snippets and tasks updated and referenced in `extension.toml`.
4. `docs/settings.md` and `docs/feature-scope.md` updated.
