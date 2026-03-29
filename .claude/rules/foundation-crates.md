---
paths:
  - "crates/al-syntax/src/**/*.rs"
  - "crates/al-symbols/src/**/*.rs"
  - "crates/al-semantic/src/**/*.rs"
---

# Foundation Crate Rules

You are editing a **leaf crate**. These crates must remain independent.

## Dependency Constraints

- al-syntax, al-symbols, and al-semantic must NEVER depend on each other
- They must NEVER depend on al-core
- If you need shared types, propose a shared leaf crate — don't create cross-dependencies

## al-syntax Specifics

- Use `LanguageData` (from `language_data.rs`) for any AL language knowledge
- NEVER add `const` arrays of keywords, builtins, types, or object kinds
- Tree-sitter traversal must be iterative (explicit stack), not recursive
- `Position.character` from LSP is UTF-16 — convert before using as byte offset

## al-symbols Specifics

- `.app` files have a 40-byte NAVX header before the ZIP
- `SymbolReference.json` has UTF-8 BOM prefix
- `EnumTypes` not `Enums` in the JSON schema
- `Kind` is integer in newer BC versions

## al-semantic Specifics

- All .NET CLR calls must go through the Mutex in SemanticBridge
- Never call DotNetHost methods directly from multiple threads
- 30-second timeout on all CLR calls
