# Code Boundary Rules (Always Loaded)

## Crate Layers
1. **Thin adapters** (al-cli, al-explorer, al-mcp, zed-al): Pure JSON-RPC clients. Zero business logic, no compile-time dependency on al-core or analysis libs.
2. **Server** (al-lsp): Sole binary. LSP (stdio) + daemon (Unix socket). Routes to al-core.
3. **Core** (al-core): All state, queries, orchestration. Only al-lsp imports it.
4. **Analysis libs** (al-syntax, al-symbols, al-semantic, al-diag, al-dap-client): Specialized, standalone. No upward dependencies. No protocol awareness.
5. **Protocol** (al-protocol): JSON-RPC types ONLY. No domain types, no logic. Shared by al-lsp, al-core, and thin adapters.

## Dependency Direction
```
al-lsp -> al-core -> al-syntax, al-symbols, al-semantic, al-diag, al-dap-client
al-cli / al-explorer / al-mcp -> al-lsp daemon (runtime JSON-RPC only)
zed-al (WASM) -> al-lsp process (runtime stdio only)
```
Circular dependencies are forbidden. One Code Path: a query via CLI, Zed, MCP, or Explorer hits the same al-lsp -> al-core code path.

Analysis libs MUST NOT depend on al-protocol. Domain types (AlToolchain, AppDependency, BcServerConfig) belong in al-core or the analysis lib that owns them — NOT in al-protocol.

## Import Rules by Crate

| Crate | MAY import | MUST NOT import |
|-------|-----------|-----------------|
| al-lsp | al-core, al-protocol | — |
| al-core | al-syntax, al-symbols, al-semantic, al-diag, al-dap-client, al-protocol | al-lsp, al-cli, al-explorer, al-mcp |
| al-syntax | tree-sitter, ropey | al-core, al-lsp, al-protocol, al-symbols, al-semantic |
| al-symbols | serde | al-core, al-lsp, al-protocol, al-syntax, al-semantic |
| al-semantic | netcorehost, tokio | al-core, al-lsp, al-protocol, al-syntax, al-symbols |
| al-dap-client | tokio, serde | al-core, al-lsp, al-protocol, al-syntax, al-symbols, al-semantic |
| al-diag | rusqlite, tracing | al-core, al-lsp, al-protocol, al-symbols, al-semantic, al-syntax |
| al-protocol | serde, std | al-core, al-lsp, al-syntax, al-symbols, al-semantic, al-diag, al-dap-client |
| al-cli | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| al-explorer | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| al-mcp | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| zed-al | zed_extension_api | al-core, al-syntax, al-symbols, al-semantic, al-protocol |

## Thin Adapter Constraints
- **al-protocol only**: Adapters depend on al-protocol for shared JSON-RPC types. No logic.
- **zed-al is WASM**: wasm32-wasip1 target. No Unix sockets, filesystem, or native dependencies.
- Adapters: parse input -> JSON-RPC request -> al-lsp -> format response for display.

## al-protocol Contract
al-protocol is types-only: JSON-RPC Request, Response, RpcError, method names, error codes. It MUST NOT contain:
- Domain types (AlToolchain, AlProject, AppDependency, BcServerConfig)
- Discovery logic (find_project, find_toolchain)
- Any function that does I/O, filesystem access, or computation

Violations are tracked in issues.toml (ISSUE-014, ISSUE-021).

## Verification
Run `cargo tree -p <crate>` to verify no forbidden transitive dependencies exist.

## Feature Scope
Read `.claude/data/features.toml` for in-scope features, command mappings, and scope decisions. Check before implementing new features or deciding what is out of scope.
