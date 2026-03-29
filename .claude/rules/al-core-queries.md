---
paths:
  - "crates/al-core/src/queries/**/*.rs"
  - "crates/al-core/src/resolution.rs"
  - "crates/al-core/src/workspace.rs"
---

# al-core Query Rules

You are editing the **business logic layer**. All LSP features are implemented here as query functions.

## Query Function Pattern

```rust
pub fn my_query(workspace: &Workspace, uri: &Url, position: Position) -> Option<MyResult> {
    // 1. Get document text and parse tree
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    // 2. Convert UTF-16 position to byte offset / tree-sitter Point
    // 3. Find relevant tree-sitter node
    // 4. Resolve types/symbols via workspace
    // 5. Return transport-agnostic result type
}
```

## Hard Rules

- **Return transport-agnostic types** — define result structs in the query module or `queries/mod.rs`. NEVER return `tower_lsp::lsp_types::*` from query functions.
- **Convert UTF-16 to bytes** — `Position.character` is UTF-16. Use `rope.utf16_cu_to_byte()` or equivalent before indexing strings.
- **Iterative tree-sitter traversal** — use explicit `Vec<Node>` stack, not recursion.
- **Short DashMap borrows** — clone data out immediately, drop the ref, then proceed. Never hold a DashMap guard across `.await`.
- **No hardcoded AL values** — use `workspace.builtins` / `LanguageData` / `al-symbols` index.
