# al-test-harness — LSP E2E Test Harness

Spawns the real `al-lsp` binary over stdio for end-to-end LSP testing.

## Quick Reference

```sh
cargo test -p al-test-harness                           # all E2E tests (spawns al-lsp binary)
cargo test -p al-test-harness --test e2e                # core E2E tests
cargo test -p al-test-harness --test regression         # regression tests
cargo test -p al-test-harness --test zed_fidelity       # Zed parity tests
cargo test -p al-test-harness --test e2e -- test_name   # single test
RUST_LOG=debug cargo test -p al-test-harness --test e2e # with logging
```

**Requires:** `al-lsp` binary built first (`cargo build -p al-lsp`).

## Test Files

| File | Focus |
|------|-------|
| e2e.rs | Core LSP feature tests |
| regression.rs | Bug regression tests |
| real_world.rs | Real-world AL file tests |
| zed_fidelity.rs | Zed editor parity |
| zed_simulation.rs | Zed workflow simulation |
| completeness.rs | Feature completeness |
| data_driven.rs | Data-driven test cases |
| edit_lifecycle.rs | Document edit lifecycle |
| integration_full.rs | Full integration scenarios |
| performance.rs | Performance benchmarks |
| transport.rs | Transport-level tests |

## Key Types

- `LspClient` — owns the child process, async writer, pending-request map, notification channel, document versions

## Public API

- `spawn(project_root)` — spawns al-lsp over stdio with full LSP handshake
- `connect(socket_path, project_root)` — daemon socket transport (unimplemented)
- `open_file()`, `change_file()`, `close_file()` — document lifecycle (waits for publishDiagnostics, 5s timeout)
- `hover()`, `completion()`, `definition()`, `references()`, `document_symbols()`, `semantic_tokens()`, etc. — LSP queries
- `drain_notifications()`, `drain_diagnostics()` — notification inspection
- `shutdown()` — graceful teardown

## Protocol Helpers (protocol.rs)

`hover_content()`, `completion_labels()`, `symbol_names()`, `semantic_token_data()`, `definition_uri()`, `definition_start_line()` — typed assertion helpers

## Gotchas

- `initialize()` polls `workspace/symbol` waiting for symbol index readiness — timeout defaults to 60s, configurable via `AL_TEST_INIT_TIMEOUT` env var (seconds)
- `open_file()` / `change_file()` block until `publishDiagnostics` arrives (5s timeout)
- Query errors return `None`/empty vec — intentional test ergonomics
- Binary discovery: `target/debug/al-lsp` → `target/release/al-lsp` → `PATH`
- Test fixture: `data/test_al_project/`
