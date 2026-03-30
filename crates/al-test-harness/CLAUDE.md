# al-test-harness — LSP E2E Test Harness

Spawns the real `al-lsp` binary over stdio for end-to-end LSP testing.

## Key Types

- `LspClient` — owns the child process, async writer, pending-request map, notification channel, document versions

## Public API

- `spawn(project_root)` / `connect(socket_path, project_root)` — constructors (`connect` is unimplemented, blocked on T303)
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
