# v3 Design Philosophy

**An efficient native Rust wrapper around the .NET AL SDK.**

Hard dependency on .NET SDK + ALTool. No VS Code. No Cursor. No fallbacks. No embedded JSON snapshots. If ALTool isn't installed, the extension tells you to install it and stops.

## Core Principles

1. **ALTool is the single source of truth** — all language data comes from the installed .NET SDK tool
2. **Rust for speed, .NET for correctness** — tree-sitter parsing + Rust index for <5ms responses; .NET CodeAnalysis for official diagnostics
3. **No fallbacks, no tech debt** — if a dependency is missing, fail clearly with actionable error
4. **AI-agent first-class** — CLI with JSON output, low token cost, structured queries
5. **Zed-native** — designed for Zed's extension model, not ported from VS Code

## Architecture

```
zed-al (WASM) → launches → al-lsp (native binary, LSP over stdio)
                              ├── al-syntax     (tree-sitter, fast)
                              ├── al-symbols    (.app extraction, NuGet)
                              ├── al-semantic   (.NET bridge, CodeAnalysis)
                              ├── al-discovery  (find ALTool, app.json)
                              └── al-dap        (debug adapter)
```

## Performance Model

- **Instant (<5ms):** syntax errors, formatting, folding, semantic tokens, document symbols
- **Fast (<50ms):** hover, completion, definition, references (Rust index lookup)
- **Async (100ms-2s):** .NET analyzer diagnostics, compilation, type resolution
