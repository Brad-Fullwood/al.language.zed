# Zed AL Extension

Custom Rust language server for AL (Microsoft Dynamics 365 Business Central) in Zed. Not proxying Microsoft's — building our own.

## Architecture

```
al-cli / al-explorer / al-mcp  →  al-lsp daemon (Unix socket)  →  al-core  →  al-syntax
zed-al (WASM)                   →  al-lsp (stdio)               →           →  al-symbols
                                                                             →  al-semantic
```

- `al-lsp`: sole server binary. LSP mode (stdio) + daemon mode (Unix socket)
- `al-core`: all state, queries, orchestration. Only al-lsp imports it. **Does not exist yet — this is the refactor target.**
- `al-cli`, `al-explorer`, `al-mcp`: pure JSON-RPC clients. Zero compile-time dependency on al-core.
- `al-semantic`: in-process .NET CLR hosting via `netcorehost`. AlBridge.dll calls CodeAnalysis.dll via reflection. NOT a subprocess.
- `al-dap`: DAP proxy — spawns EditorServices.Host, patches messages bidirectionally.

## Current Violations

al-cli and al-explorer currently import analysis libraries directly. They must be rewritten as daemon clients (T304, T1001).

## Build & Test

- `make build` — all Rust + .NET bridges
- `cargo test -p al-test-harness` — LSP integration tests against Debar project
- `al-semantic/build.rs` compiles AlBridge.dll via `dotnet build`
- `al-syntax/build.rs` compiles tree-sitter C parser via `cc`
- ALTool: `dotnet tool install --global Microsoft.Dynamics.BusinessCentral.Development.Tools`

## Key Gotchas

- `.app` format: 40-byte NAVX header + ZIP. `SymbolReference.json` has UTF-8 BOM, `EnumTypes` not `Enums`, `Kind` as integer in newer versions.
- NuGet feed: `dynamicssmb2` (NOT `dynamicssmb`).
- tree-sitter `braced_block` excludes action triggers — text-based fallback in TypeResolver.
- tree-sitter-al is a submodule. All language data extracted from CodeAnalysis.dll — NO VS Code extension dependency.
- Without ALTool: no semantic analysis, compilation, or debugging. Syntax features still work.
- EditorServices.Host required for debugging only. Must come from ALTool or explicit config, not VS Code.
- 5 implicit BC dependency GUIDs hardcoded in `al-discovery`.

## Docs & Planning

- `plan.md` — task sequences with IDs, file ownership, pass/fail criteria
- `docs/crates-map.md` — crate responsibilities and target module layout
- `docs/architecture.md` — full architecture, .NET bridge, data flows, error taxonomy
- `docs/agentic-schemas.md` — CLI/MCP/slash command JSON output schemas
- `docs/agent-scenarios.md` — agent discovery scenarios (insight engine test cases)
- `docs/adversarial-atlas.md` — stress tests and fidelity gaps
- `docs/feature-scope.md` — feature scope: MS parity, community tools, beyond-parity
- `docs/market-research.md` — competitive landscape
- `docs/insight.md` — insight engine: graph model, capabilities, CLI surface
- `docs/settings.md` — authoritative settings mapping (MS → Zed)
- `docs/lsp-feature-matrix.md` — LSP request migration mapping
- `docs/performance-plan.md` — latency and memory targets
- `docs/release.md` — build and release pipeline
- `docs/progress.md` — milestone tracker and evidence log
