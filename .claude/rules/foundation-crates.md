---
paths:
  - "crates/al-core/src/syntax/**/*.rs"
  - "crates/al-core/src/symbols/**/*.rs"
  - "crates/al-core/src/semantic/**/*.rs"
---

# Foundation Module Rules

You are editing the syntax / symbols / semantic foundation modules of `al-core`.

## Module Constraints

- These foundations should not call each other across module boundaries:
  - `syntax` does not import `symbols` or `semantic`
  - `symbols` does not import `syntax` or `semantic`
  - `semantic` does not import `syntax` or `symbols`
- Cross-cutting orchestration goes in `al_core` higher-level modules
  (`workspace`, `queries`, `server`, etc.), not inside these foundations
- The `lsp_types::*` types are forbidden here — convert at the
  `al_core::server` boundary or via `al_core::syntax_lsp` helpers

## syntax Specifics

- Use `LanguageData` for any AL language knowledge
- NEVER add `const` arrays of keywords, builtins, types, or object kinds
- Tree-sitter traversal must be iterative (explicit stack), not recursive
- `Position.character` from LSP is UTF-16 — convert before using as byte offset

## symbols Specifics

- `.app` files have a 40-byte NAVX header before the ZIP
- `SymbolReference.json` has UTF-8 BOM prefix
- `EnumTypes` not `Enums` in the JSON schema
- `Kind` is integer in newer BC versions

## semantic Specifics

- All .NET CLR calls must go through the Mutex in `SemanticBridge`
- Never call `DotNetHost` methods directly from multiple threads
- 30-second timeout on all CLR calls
- Bridge initialisation is lazy — check `Option<SemanticBridge>` at every call site
