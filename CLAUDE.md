# Zed AL Extension — v3

## Project Overview
Zed extension for AL (Microsoft Dynamics 365 Business Central) development.
**v3 approach:** efficient native Rust wrapper around the .NET AL SDK (ALTool).

## Architecture
- `zed-al` (WASM) launches `al-lsp` (native binary) over stdio
- 6 library crates: al-discovery, al-syntax, al-symbols, al-semantic, al-dap
- 2 binary crates: al-lsp, al-cli
- 2 .NET bridge projects: AlSemantic, AlDap (subprocess, JSON-RPC)

## Key Rules
1. **ALTool is required** — no fallbacks, no embedded snapshots
2. **Never reference VS Code or Cursor** — this is Zed-native
3. **Rust for speed, .NET for correctness** — tree-sitter for parsing, CodeAnalysis for semantics
4. **Two-phase diagnostics** — instant syntax errors, async analyzer results
5. **.NET is subprocess-isolated** — crashes don't take down the LSP

## Crate Dependencies
```
al-discovery  ← nothing
al-syntax     ← nothing
al-symbols    ← al-discovery
al-semantic   ← al-discovery
al-dap        ← al-discovery
al-lsp        ← al-syntax, al-symbols, al-semantic, al-discovery, al-dap
al-cli        ← al-syntax, al-symbols, al-semantic, al-discovery
zed-al        ← zed_extension_api
```

## Tree-sitter Grammar
- Submodule at `tree-sitter-al/` (symlink NOT needed in v3)
- Build via `build.rs` in al-syntax

## Testing
- Unit tests in each crate
- Integration tests spawn al-lsp binary
- Test AL project in `test_al_project/`

## File Conventions
- `languages/al/`, `snippets/`, `themes/` — standard Zed extension paths, DO NOT move
- `.app` format: 40-byte NAVX header + ZIP containing SymbolReference.json
- ALTool location: `~/.dotnet/tools/.store/microsoft.dynamics.businesscentral.development.tools*/`
