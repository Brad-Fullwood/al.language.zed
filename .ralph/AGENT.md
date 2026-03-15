# Agent Build Instructions

## Build Commands

```bash
# Quick compile check (always exclude zed-al WASM target)
cargo check --workspace --exclude zed-al

# Run all tests
cargo test --workspace --exclude zed-al

# Lint
cargo clippy --workspace --exclude zed-al

# Core unit tests only
cargo test -p al-core

# LSP integration tests (spawns real al-lsp binary)
cargo test -p al-test-harness --test data_driven
```

## Key Constraints

- `zed-al` requires `wasm32-wasip1` target — **always exclude** from workspace commands
- Pipe cargo output through `| tail -30` to keep output concise
- al-test-harness spawns real al-lsp binary, communicates via LSP protocol
- Tests run against real Debar project fixture

## Notes
- `.app` files: 40-byte NAVX header + ZIP. `SymbolReference.json` has UTF-8 BOM.
- NuGet feed: `dynamicssmb2` (NOT `dynamicssmb`).
- Symbol cache: `~/.cache/al-lsp/packages/`
- LSP positions are UTF-16 code units — convert to byte offsets before slicing Rust strings.
