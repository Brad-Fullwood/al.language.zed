# Architecture Rules (Always Loaded)

## Crate Layers
1. **Thin adapters** (al-cli, al-explorer, al-mcp, zed-al): Zero business logic. No compile-time dependency on al-core, al-syntax, al-symbols, or al-semantic.
2. **Server** (al-lsp): Sole binary. LSP (stdio) + daemon (Unix socket). Routes to al-core.
3. **Core** (al-core): All state, queries, orchestration. Only al-lsp imports it.
4. **Analysis libs** (al-syntax, al-symbols, al-semantic, al-diag): Specialized, standalone. No upward dependencies.

## Dependency Direction
```
al-lsp -> al-core -> al-syntax, al-symbols, al-semantic, al-diag
al-cli / al-explorer / al-mcp -> al-lsp daemon (runtime JSON-RPC only)
zed-al (WASM) -> al-lsp process (runtime stdio only)
```
Circular dependencies are forbidden. Thin adapters communicate with al-lsp via JSON-RPC exclusively.

## One Code Path
A query via CLI, Zed, MCP, or Explorer hits the same al-lsp -> al-core code path. There is ONE implementation of each query.
