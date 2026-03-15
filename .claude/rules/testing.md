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
- Tests run against real AL test project fixture

## Fixture Requirement (zed_simulation tests)
The 37 `test_fixture_*` tests in `zed_simulation.rs` require a real AL project on disk.
They **silently skip** if the fixture is absent — not a failure.

Configure the fixture path via environment variable:
```bash
AL_TEST_PROJECT_PATH=/path/to/AL/project cargo test -p al-test-harness
```

Default path (machine-specific, only works on Brad's machine):
(default: hardcoded local path — set `AL_TEST_PROJECT_PATH` to override)

The fixture must contain `app.json` at its root. The `e2e.rs` and `data_driven.rs` tests
use `test_al_project/` which IS in the repo and always run.

## Test-First Development
Use superpowers:test-driven-development. Write the failing test first, then implement until it passes.

## What Must Pass Before Completion
1. `cargo check --workspace --exclude zed-al` — compiles
2. `cargo test --workspace --exclude zed-al` — tests pass
3. `cargo clippy --workspace --exclude zed-al` — no warnings
