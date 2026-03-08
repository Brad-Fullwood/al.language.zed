# Architecture

## Crate Structure

| Crate | Type | Purpose |
|-------|------|---------|
| `zed-al` | WASM cdylib | Zed extension glue — launches al-lsp binary |
| `al-lsp` | Binary | LSP server (tower-lsp) — the main brain |
| `al-syntax` | Library | Tree-sitter parsing, formatting, lint |
| `al-symbols` | Library | .app parsing, symbol index, NuGet |
| `al-semantic` | Library | .NET CodeAnalysis bridge (subprocess) |
| `al-discovery` | Library | Find ALTool, app.json, .alpackages |
| `al-dap` | Library | Debug Adapter Protocol |
| `al-cli` | Binary | AI-agent CLI interface |

## Dependency Graph

```
al-discovery  ← depends on nothing
al-syntax     ← depends on nothing
al-symbols    ← depends on al-discovery
al-semantic   ← depends on al-discovery
al-dap        ← depends on al-discovery
al-lsp        ← depends on al-syntax, al-symbols, al-semantic, al-discovery, al-dap
al-cli        ← depends on al-syntax, al-symbols, al-semantic, al-discovery
zed-al        ← depends on zed_extension_api (launches al-lsp binary)
```

## Data Flow

### LSP Request Flow
```
Zed Editor
  → stdio → al-lsp (tower-lsp)
    → al-syntax (tree-sitter parse, <1ms)
    → al-symbols (DashMap lookup, <5ms)
    → al-semantic (.NET subprocess, async)
  ← stdio ← response
```

### Two-Phase Diagnostics
1. **Instant:** al-syntax parses → syntax errors + native lint rules → publish immediately
2. **Async:** al-semantic → .NET CodeAnalysis analyzers → publish when ready

### .NET Bridge Communication
```
al-semantic (Rust) ──JSON-RPC/stdio──► AlSemantic (C#) ──loads──► CodeAnalysis.dll
                   ◄──JSON-RPC/stdio──
```
- One long-lived subprocess per workspace
- Auto-killed after idle timeout
- Crash isolated — LSP continues without semantic features
