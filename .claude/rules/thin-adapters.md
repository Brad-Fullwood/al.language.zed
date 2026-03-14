# Thin Adapter Rules (Always Loaded)

## What Are Thin Adapters
al-cli, al-explorer, al-mcp, and zed-al are pure JSON-RPC clients. They format user input into requests, send them to al-lsp, and format the response for display.

## Hard Rules
- **Zero al-core dependency**: `cargo tree -p al-cli` (or al-explorer, al-mcp) must show NO path to al-core, al-syntax, al-symbols, al-semantic, or al-diag.
- **No business logic**: Adapters do not parse AL, resolve symbols, run diagnostics, or manage workspace state.
- **al-protocol only**: Thin adapters may depend on al-protocol for shared JSON-RPC type definitions. al-protocol contains types only — no logic.
- **zed-al is WASM**: Compiled to wasm32-wasip1. Cannot use Unix sockets, filesystem, or native dependencies. Communicates via stdio only.

## What Adapters DO
- Parse CLI args or MCP tool calls → JSON-RPC request
- Connect to al-lsp daemon (Unix socket) or spawn al-lsp (stdio)
- Format JSON-RPC response → user-facing output (text, JSON, TUI)
