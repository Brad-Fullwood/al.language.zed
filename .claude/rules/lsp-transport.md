---
paths:
  - "crates/al-lsp/src/**/*.rs"
---

# LSP Transport Layer Rules

You are editing the **transport layer**. This crate converts between wire formats and al-core types.

## Hard Rules

- **No business logic here.** If you're reading tree-sitter nodes, resolving types, or looking up symbols — that belongs in `al-core/src/queries/`.
- The correct pattern is: receive LSP request → call `al_core::queries::*` → convert result to LSP response type → return.
- **No blocking I/O in async handlers.** Use `tokio::fs` or `spawn_blocking`, never `std::fs` in async code paths.
- **No `unwrap()` in request handlers.** Return an LSP error response instead.

## Daemon Mode

- Daemon dispatch handlers in `daemon/` follow the same rule: call al-core queries, convert to JSON-RPC response.
- The daemon uses line-delimited JSON-RPC, not Content-Length framing.
- Socket path: `$XDG_RUNTIME_DIR/al-lsp/<fnv1a_hash(project_path)>.sock`
