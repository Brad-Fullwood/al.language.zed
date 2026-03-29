## Focus Areas
- Dependency direction violations between crates
- Hardcoded AL language values — must use LanguageData or al-symbols
- Business logic in al-lsp that belongs in al-core/src/queries/
- tower_lsp types leaking into al-core query function returns
- UTF-16/byte offset conversion in LSP position code
- Duplicate implementations across crate boundaries
- unwrap()/expect() in library code (not tests)
- Recursive tree-sitter traversal (must be iterative with explicit stack)
- DashMap refs held across await points (deadlock risk)
- Scope creep — changes to files unrelated to the task
- Missing negative tests (only happy-path coverage)

## Skip
- Generated tree-sitter files in tree-sitter-al/src/ and tree-sitter-al/bindings/
- WASM-specific code in src/ (zed-al root crate, separate build target)
- Test fixture files in crates/al-test-harness/data/
- .NET bridge generated bindings in crates/al-semantic/bridge/
- node_modules anywhere
- docs/CODE_REVIEW.md (this is review output, not reviewable code)
