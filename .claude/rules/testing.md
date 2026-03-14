# Testing Rules (Always Loaded)

## Commands
```bash
cargo test --workspace --exclude zed-al           # all tests
cargo test -p al-lsp                               # LSP integration tests
cargo test -p al-core                              # core unit tests
cargo clippy --workspace --exclude zed-al          # lint
cargo check --workspace --exclude zed-al           # quick compile check
```

zed-al requires wasm32-wasip1 target — always excluded from workspace commands.

## Test Harness (al-test-harness)
- Spawns real al-lsp binary, communicates via LSP protocol
- `open_file()` waits for `publishDiagnostics` (not fixed sleep)
- `initialize()` polls `workspace/symbol` until non-empty (30s timeout)
- Tests run against real Debar project fixture

## Test-First Development
Use superpowers:test-driven-development. Write the failing test first, then implement until it passes.

## What Must Pass Before Completion
1. `cargo check --workspace --exclude zed-al` — compiles
2. `cargo test --workspace --exclude zed-al` — tests pass
3. `cargo clippy --workspace --exclude zed-al` — no warnings
