# Code Boundary Rules (Always Loaded)

## Crate Layers
1. **Thin adapters** (al-cli, al-explorer, al-mcp, zed-al): Pure JSON-RPC clients. Zero business logic, no compile-time dependency on al-core or analysis libs.
2. **Server** (al-lsp): Sole binary. LSP (stdio) + daemon (Unix socket). Routes to al-core.
3. **Core** (al-core): All state, queries, orchestration. Only al-lsp imports it.
4. **Analysis libs** (al-syntax, al-symbols, al-semantic, al-dap-client): Specialized, standalone. No upward dependencies. No protocol awareness.
5. **Protocol** (al-protocol): JSON-RPC types ONLY. No domain types, no logic. Shared by al-lsp, al-core, and thin adapters.

## Dependency Direction

```
zed-al (WASM)   ->  al-lsp (stdio)
al-cli          ->  al-lsp daemon (runtime JSON-RPC)
al-explorer     ->  al-lsp daemon (runtime JSON-RPC)
al-mcp          ->  al CLI binary (subprocess, no compile-time al-* deps)
al-lsp          ->  al-core, al-protocol, al-dap-client
al-core         ->  al-syntax, al-symbols, al-semantic, al-dap-client, al-protocol
```

Circular dependencies are forbidden. One Code Path: a query via CLI, Zed, MCP, or Explorer hits the same al-lsp -> al-core code path.

Analysis libs MUST NOT depend on al-protocol. Domain types (AlToolchain, AppDependency, BcServerConfig) belong in al-core or the analysis lib that owns them — NOT in al-protocol.

## Import Rules by Crate

| Crate | MAY import | MUST NOT import |
|-------|-----------|-----------------|
| al-lsp | al-core, al-protocol, al-dap-client | al-cli, al-explorer, al-mcp |
| al-core | al-syntax, al-symbols, al-semantic, al-dap-client, al-protocol | al-lsp, al-cli, al-explorer, al-mcp |
| al-syntax | tree-sitter, ropey, tower-lsp | al-core, al-lsp, al-protocol, al-symbols, al-semantic |
| al-symbols | serde | al-core, al-lsp, al-protocol, al-syntax, al-semantic |
| al-semantic | netcorehost, tokio | al-core, al-lsp, al-protocol, al-syntax, al-symbols |
| al-dap-client | tokio, serde | al-core, al-lsp, al-protocol, al-syntax, al-symbols, al-semantic |
| al-protocol | serde, std | al-core, al-lsp, al-syntax, al-symbols, al-semantic, al-dap-client |
| al-cli | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| al-explorer | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| al-mcp | (none — shells out to `al` binary) | al-core, al-protocol, al-syntax, al-symbols, al-semantic |
| zed-al | zed_extension_api | al-core, al-syntax, al-symbols, al-semantic, al-protocol |

## Adapter Patterns

**al-cli**: Thin adapter. Imports al-protocol for shared JSON-RPC types, connects to al-lsp daemon via JSON-RPC at runtime.

**al-explorer**: Thin adapter. Connects to al-lsp daemon via JSON-RPC at runtime. Imports al-protocol only. Currently violates this by importing al-symbols directly (ISSUE-017: must be refactored).

**al-mcp**: MCP server that wraps the `al` CLI binary. Runs `al <command> --json` as subprocesses. Zero compile-time al-* dependencies.

**zed-al**: WASM extension on wasm32-wasip1 target. Zed spawns al-lsp as a child process and communicates via stdio. No Unix sockets, filesystem, or native dependencies.

## al-protocol Contract
al-protocol is types-only: JSON-RPC Request, Response, RpcError, method names, error codes. It MUST NOT contain:
- Domain types (AlToolchain, AlProject, AppDependency, BcServerConfig)
- Discovery logic (find_project, find_toolchain)
- Any function that does I/O, filesystem access, or computation

Current violations tracked in issues.toml (ISSUE-014, ISSUE-021).

## al-syntax tower-lsp Dependency
al-syntax imports tower-lsp for `Position`, `Range`, `DocumentSymbol`, `SymbolKind`,
`FoldingRange`, and `FoldingRangeKind` types. This coupling is pervasive (6+ files) and
`navigation.rs` re-exports `Position` publicly. Accepted as a practical dependency since
al-syntax exclusively serves the LSP stack. Future refactoring could define native types
(ISSUE-013). New code in al-syntax MAY use tower-lsp types.

## Verification
Run `cargo tree -p <crate>` to verify no forbidden transitive dependencies exist.

## Feature Scope
Read `.claude/data/features.toml` for in-scope features, command mappings, and scope decisions. Check before implementing new features or deciding what is out of scope.
