# Thin Adapter Mandate
**Status: Non-Negotiable**

`al-cli`, `al-explorer`, `al-mcp`, and `zed-al` are **Thin Adapters**. They are pure JSON-RPC clients with **zero compile-time dependency on `al-core`**.

`al-lsp` is the **sole server binary**. It is NOT a thin adapter — it is the routing layer between thin adapters and `al-core`.

### Architecture:
```
al-cli / al-explorer / al-mcp  →  al-lsp (daemon, Unix socket)  →  al-core
zed-al (WASM)                   →  al-lsp (process, stdio)       →  al-core
```

### Rules:
1. **Zero Business Logic in Adapters**: Thin adapters contain ONLY:
   - **al-cli**: Argument parsing (clap), daemon connection, JSON-RPC request/response, output formatting (JSON/Text), exit codes.
   - **al-explorer**: Daemon connection, JSON-RPC streaming, TUI rendering (ratatui), keyboard input.
   - **al-mcp**: Daemon connection, MCP-to-JSON-RPC translation, MCP tool registration.
   - **zed-al**: `zed::Extension` trait implementation, `process::Command` to spawn al-lsp, slash command formatting.
2. **No Compile-Time Core Dependency**: al-cli, al-explorer, and al-mcp must NOT have `al-core` in their `Cargo.toml` dependencies. They communicate with al-lsp via JSON-RPC only.
3. **Consistency**: A query via CLI and a query via Zed hit the exact same al-lsp → al-core code path. There is ONE code path for analysis.
4. **al-lsp is the server**: It owns tower-lsp transport (LSP mode), Unix socket listener (daemon mode), DAP proxy, and request routing to `al-core::queries`.

### What Violates This Rule (examples):
- `al-cli/Cargo.toml` containing `al-core = { path = "..." }`.
- Any `use al_syntax::` or `use al_symbols::` in al-cli, al-explorer, or al-mcp.
- Hover logic, completion filtering, or symbol resolution in any thin adapter.
- al-cli constructing analysis results instead of forwarding JSON-RPC responses.

### Verification:
1. Check `Cargo.toml` of al-cli, al-explorer, al-mcp — they must NOT depend on al-core, al-syntax, al-symbols, or al-semantic.
2. Their only analysis-related dependency should be shared JSON-RPC type definitions (protocol schema).
