---
paths:
  - "crates/al-core/src/server/**/*.rs"
  - "crates/al-core/src/dap/**/*.rs"
  - "crates/al-core/src/bin/al-lsp.rs"
---

# LSP / Daemon / DAP Transport Rules

You are editing the **transport layer** of `al-core`. The `[[bin]] al-lsp`
target lives at `crates/al-core/src/bin/al-lsp.rs`.

## Hard Rules

- **No business logic in transport code.** If you're reading tree-sitter nodes,
  resolving types, or looking up symbols — that belongs in `al_core::queries::*`.
- Pattern: receive LSP/daemon/DAP request → call `al_core::queries::*` → convert
  result to wire-format response → return.
- **`lsp_types::*` lives only in transport code.** Never in `al_core::queries::*`
  function signatures — they return transport-agnostic types.
- **No blocking I/O in async handlers.** Use `tokio::fs` or `spawn_blocking`,
  never `std::fs` in async code paths.
- **No `unwrap()` in request handlers.** Return an LSP error response instead.

## Daemon Mode

- Daemon dispatch handlers (`server::daemon::*`) follow the same rule: call
  `al_core::queries::*`, convert to JSON-RPC response using `al_protocol` types.
- The daemon uses line-delimited JSON-RPC, not Content-Length framing.
- Socket path: `$XDG_RUNTIME_DIR/al-lsp/<fnv1a_hash(project_path)>.sock`

## DAP Mode

- DAP framing patches missing `seq` field and string→bool launch args from BC's
  EditorServices.Host before forwarding.
- All proxying logic stays in `al_core::dap` (BC framing) and
  `al_core::server::dap_mode` (LSP-side wiring) — no business logic.
