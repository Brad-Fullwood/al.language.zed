---
name: run-al-language-zed
description: Smoke-test the AL toolchain's native surfaces — al-explorer (CLI/TUI), al-lsp (language server + MCP) — against the bundled fixture project, as Rust integration tests. Use to run, build, or smoke-test the al-lsp/al-explorer CLI/LSP/MCP/TUI surfaces (no GUI, no container). For verifying a change in the actual editor, use /run-al-extension-in-zed instead.
---

# Run the AL native surfaces (al-explorer / al-lsp)

Fast, deterministic smoke tests for the **native** surfaces — no GUI, no
container. They live as **Rust integration tests in the `al-test-harness`
crate** (the test logic is Rust; only the GUI harness needs shell). Use these
for parser / symbols / semantic / LSP-protocol / CLI changes; use
`/run-al-extension-in-zed` to confirm a change in the real editor.

## Run (agent path)

The binaries must be built first (the tests spawn `target/debug/al-lsp` and
`al-explorer`):

```bash
cargo build -p al-lsp --bin al-lsp -p al-explorer
cargo test  -p al-test-harness --test cli_smoke --test mcp_stdio --test tui_smoke
```

- `cli_smoke` — drives `al-explorer` subcommands against the fixture (`parse`,
  `symbols`, `metrics`, `lint`, `format`, `search`, `tests`, `dead-code`,
  `sql-scan`, `diag`) and asserts on output.
- `mcp_stdio` — initializes `al-lsp mcp` and asserts the AL tools are exposed.
- `tui_smoke` — drives the `al-explorer` TUI in a PTY (via `portable-pty`),
  renders it with a `vt100` parser, and asserts the fixture objects appear.

The full LSP stdio surface (hover, documentSymbol, completion, rename, …) is
already covered by the crate's existing `LspClient` suite — run all of it with:

```bash
cargo test -p al-test-harness
```

## What this verifies vs. the GUI harness

- **This skill (native):** the `al-lsp` LSP protocol, the `al-explorer` CLI/TUI,
  and the MCP server, driven directly — fast, and the right tool for
  protocol/CLI/logic changes.
- **`/run-al-extension-in-zed` (GUI e2e):** the WASM extension loading in real
  Zed, grammar highlighting, and `al-lsp` spawned by the editor — the full
  integration path, plus a VS Code reference comparison.
