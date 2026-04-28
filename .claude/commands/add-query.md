Guide for adding a new LSP query function.

This is the correct pattern for adding a new query to the AL language server.

## Steps

1. **Create the query function in al-core** (`crates/al-core/src/queries/`)
   - Function signature: `pub fn my_query(workspace: &Workspace, uri: &Url, position: Position) -> Option<MyResult>`
   - Return transport-agnostic types (NOT LSP types)
   - Register in `crates/al-core/src/queries/mod.rs`

2. **Wire it in `al_core::server`** (`crates/al-core/src/server/handlers.rs` or the relevant handler file)
   - Convert LSP request params to `al_core::queries` types (use `al_core::syntax_lsp` helpers)
   - Call the query function
   - Convert the result to LSP response types
   - Handle None/errors gracefully

3. **Register the LSP capability** in `crates/al-core/src/server/lsp.rs` (ServerCapabilities)

4. **If daemon mode needs it**, add a dispatch handler in `crates/al-core/src/server/daemon/lsp_dispatch.rs`

5. **Add tests**
   - Unit test in `crates/al-core/src/queries/` (test the query function directly)
   - E2E test in `crates/al-test-harness/tests/` (test through LSP protocol)

## Key Rules
- ALL logic goes in `al_core::queries::*` — `al_core::server` only does transport conversion
- Query function signatures stay free of `lsp_types::*`
- Use `al_core::syntax::LanguageData` for any AL language knowledge — never hardcode
- Convert UTF-16 positions to byte offsets before string operations
- Use iterative tree-sitter traversal (explicit stack), not recursion
