# Code Boundary Rules (Always Loaded)

## Crate Layers
1. **WASM extension** (zed-al): Connects to al-lsp via stdio. No compile-time al-* dependencies.
2. **Server** (al-lsp): Sole server binary. LSP (stdio) + daemon (Unix socket). Routes to al-core. Also imports al-dap-client directly and al-diag (optional, feature-gated).
3. **Core** (al-core): All state, queries, orchestration. Only al-lsp imports it.
4. **Analysis libs** (al-syntax, al-symbols, al-semantic, al-diag, al-dap-client): Specialized, standalone. No upward dependencies (no al-core, no al-lsp). al-symbols, al-semantic, and al-dap-client import al-protocol for shared domain types (ISSUE-021: known violation, being refactored out).
5. **Protocol** (al-protocol): Shared types AND discovery logic. Contains JSON-RPC types plus `find_project()`, `find_toolchain()`, `AlToolchain`, `AlProject`, `AppDependency`, `BcServerConfig`. Shared by all crates.

## Dependency Direction

```
zed-al (WASM)   ->  al-lsp (stdio)
al-cli          ->  al-lsp daemon (runtime JSON-RPC)
al-mcp          ->  al CLI binary (subprocess, no compile-time al-* deps)
al-explorer     ->  al-symbols (direct, in-process — bypasses al-lsp)
al-lsp          ->  al-core, al-protocol, al-dap-client, al-diag (optional)
al-core         ->  al-syntax, al-symbols, al-semantic, al-dap-client, al-protocol
al-symbols      ->  al-protocol (domain types — ISSUE-021)
al-semantic     ->  al-protocol (domain types — ISSUE-021)
al-dap-client   ->  al-protocol (domain types — ISSUE-021)
al-syntax       ->  tree-sitter, ropey, tower-lsp (Position/Range types — ISSUE-013)
al-diag         ->  rusqlite, tracing (zero al-* dependencies)
al-protocol     ->  serde, tracing, urlencoding (zero al-* dependencies)
```

Circular dependencies are forbidden.

Note: al-cli routes through al-lsp for most queries. al-explorer directly uses al-symbols,
creating a second implementation of symbol browsing (intentional: TUI does not need a running
daemon). al-mcp is a thin MCP wrapper around the `al` CLI binary with no shared types.

## Import Rules by Crate

| Crate | MAY import | MUST NOT import |
|-------|-----------|-----------------|
| al-lsp | al-core, al-protocol, al-dap-client, al-diag | al-cli, al-explorer, al-mcp |
| al-core | al-syntax, al-symbols, al-semantic, al-dap-client, al-protocol | al-lsp, al-cli, al-explorer, al-mcp, al-diag |
| al-syntax | tree-sitter, ropey, tower-lsp | al-core, al-lsp, al-protocol, al-symbols, al-semantic |
| al-symbols | al-protocol, serde, dashmap, dirs, zip, memmap2 | al-core, al-lsp, al-syntax, al-semantic |
| al-semantic | al-protocol, netcorehost, tokio | al-core, al-lsp, al-syntax, al-symbols |
| al-dap-client | al-protocol, tokio, serde | al-core, al-lsp, al-syntax, al-symbols, al-semantic |
| al-diag | rusqlite, tracing | al-core, al-lsp, al-protocol, al-symbols, al-semantic, al-syntax |
| al-protocol | serde, tracing, urlencoding, std | al-core, al-lsp, al-syntax, al-symbols, al-semantic, al-diag, al-dap-client |
| al-cli | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| al-explorer | al-protocol, al-symbols | al-core, al-syntax, al-semantic, al-lsp |
| al-mcp | (none — shells out to `al` binary) | al-core, al-protocol, al-syntax, al-symbols, al-semantic |
| zed-al | zed_extension_api | al-core, al-syntax, al-symbols, al-semantic, al-protocol |

## Adapter Patterns

**al-cli**: Thin adapter. Imports al-protocol for shared types, connects to al-lsp daemon via
JSON-RPC at runtime. Some local operations (cache clearing, file walking for format-all) are
done directly in-process (ISSUE-020: soft boundary violation, known and accepted).

**al-mcp**: MCP server that wraps the `al` CLI binary. Runs `al <command> --json` as subprocesses.
Zero compile-time al-* dependencies. Changes to CLI output automatically propagate to MCP.

**al-explorer**: TUI symbol browser. Imports al-symbols directly and constructs its own
SymbolIndex. Does NOT connect to al-lsp daemon — intentional for standalone TUI use. This
creates a second implementation of symbol browsing (ISSUE-017: documented violation).

**zed-al**: WASM extension on wasm32-wasip1 target. Zed spawns al-lsp as a child process and
communicates via stdio. No Unix sockets, filesystem, or native dependencies.

## al-protocol Contract
al-protocol is a shared foundation crate. It contains:
- JSON-RPC types: `Request`, `Response`, `RpcError`, method names, error codes
- Domain types: `AlToolchain`, `AlProject`, `AppManifest`, `AppDependency`, `BcServerConfig`, `NuGetFeed`
- Discovery logic: `find_project()`, `find_toolchain()`, `find_launch_config()`, `nuget_feeds()`

Domain types in al-protocol exist so al-symbols, al-semantic, al-dap-client, and al-cli can share
them without depending on al-core. This is a pragmatic tradeoff vs. the pure types-only ideal.
Future refactoring may move domain types into the crates that own them (ISSUE-021).

## al-diag Crate
al-diag is a SQLite-backed structured tracing and logging layer. It records LSP requests,
resolution steps, type inference decisions, and timing data. It is NOT a diagnostic analysis
crate — it has no AL parsing or analysis capability. It depends only on rusqlite and tracing.
al-lsp imports it as an optional dependency (feature = "diagnostics", default-on).
al-core does NOT import al-diag.

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
