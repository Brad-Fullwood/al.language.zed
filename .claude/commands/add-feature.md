Structured guide for implementing a new feature in the AL language server.

Before writing any code:

## 1. Understand the Architecture
- Read CLAUDE.md thoroughly
- Identify which al-core module(s) the feature touches (`syntax`, `symbols`, `semantic`, `queries`, `server`, `dap`)
- Verify the feature doesn't violate the dependency rules (`al-explorer → al-protocol`, `al-core → al-protocol`)

## 2. Plan the Implementation
- Which `al_core::queries::*` function(s) need to be added/modified?
- What `al_core::server` handler changes are needed (transport conversion only)?
- Does the daemon mode need a new dispatch handler in `al_core::server::daemon`?
- What tests will you write?

## 3. Implementation Order
1. **Query function first**: Implement the business logic in `crates/al-core/src/queries/`
2. **Tests second**: Add unit tests for the new query function (positive AND negative paths)
3. **Transport last**: Wire `al_core::server` (LSP handler / daemon dispatch) — keep this thin
4. **E2E tests**: Add end-to-end tests in `al-test-harness`

## 4. Pre-Submit Checklist
- [ ] All logic is in `al_core::queries::*` (not in `al_core::server`)
- [ ] Query function signatures stay free of `lsp_types::*` (use `al_core::syntax_lsp` or transport-agnostic types)
- [ ] No hardcoded AL values (use `al_core::syntax::LanguageData` / `al_core::symbols`)
- [ ] UTF-16 positions correctly converted before indexing strings
- [ ] Iterative tree-sitter traversal (not recursive)
- [ ] No DashMap refs held across await points
- [ ] Tests pass: `cargo test --workspace --exclude zed-al`
- [ ] Clippy clean: `cargo clippy --workspace --exclude zed-al -- -D warnings`
- [ ] Formatted: `cargo fmt --all`
- [ ] Only files related to this feature are modified
