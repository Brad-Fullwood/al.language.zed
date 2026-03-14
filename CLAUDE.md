# Zed AL Extension

Custom Rust language server for AL (Microsoft Dynamics 365 Business Central) in Zed.

## Architecture

```
al-cli / al-explorer / al-mcp  ->  al-lsp daemon (Unix socket)  ->  al-core  ->  al-syntax
zed-al (WASM)                   ->  al-lsp (stdio)               ->           ->  al-symbols
                                                                              ->  al-semantic
```

- **al-lsp**: sole server binary. LSP (stdio) + daemon (Unix socket). Routes to al-core.
- **al-core**: all state, queries, orchestration. Only al-lsp imports it. **Does not exist yet.**
- **al-protocol**: JSON-RPC types crate shared by al-lsp and thin adapters. Types only.
- **Thin adapters** (al-cli, al-explorer, al-mcp, zed-al): pure JSON-RPC clients. Zero al-core dependency.
- **al-semantic**: in-process .NET CLR via `netcorehost`. NOT a subprocess.

## Build & Test

```bash
cargo test --workspace --exclude zed-al
cargo clippy --workspace --exclude zed-al
```

## Key Gotchas

- `.app`: 40-byte NAVX header + ZIP. `SymbolReference.json` has UTF-8 BOM, `EnumTypes` not `Enums`.
- NuGet feed: `dynamicssmb2` (NOT `dynamicssmb`).
- tree-sitter `braced_block` excludes action triggers — text-based fallback in TypeResolver.
- Without ALTool: syntax features work, no semantic/compilation/debugging.

## Skills

`/start-work` to begin a session. `/test`, `/check`, `/adversarial`, `/supervise`, `/audit`, `/pof`, `/report`, `/fix-infra`, `/ci`.

## Docs

- `docs/plan.md` — task sequences with IDs, deps, pass/fail criteria
- `docs/architecture.md` — full architecture, .NET bridge, error taxonomy
- `docs/crates-map.md` — crate responsibilities and target module layout
- `docs/agentic-schemas.md` — CLI/MCP JSON output schemas
