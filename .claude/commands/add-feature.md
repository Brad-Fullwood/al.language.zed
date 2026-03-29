Structured guide for implementing a new feature in the AL language server.

Before writing any code:

## 1. Understand the Architecture
- Read CLAUDE.md thoroughly
- Identify which crate(s) the feature touches
- Verify the feature doesn't violate dependency rules

## 2. Plan the Implementation
- Which al-core query function(s) need to be added/modified?
- What al-lsp handler changes are needed (transport only)?
- Does the daemon mode need a new dispatch handler?
- What tests will you write?

## 3. Implementation Order
1. **al-core first**: Implement the business logic as query functions in `al-core/src/queries/`
2. **Tests second**: Add unit tests for the new query functions
3. **al-lsp last**: Wire the transport layer (LSP handler, daemon dispatch)
4. **E2E tests**: Add end-to-end tests in `al-test-harness`

## 4. Pre-Submit Checklist
- [ ] All logic is in al-core (not al-lsp)
- [ ] No hardcoded AL values
- [ ] UTF-16 positions correctly converted
- [ ] Iterative tree-sitter traversal (not recursive)
- [ ] No DashMap refs held across await points
- [ ] Tests pass: `cargo test --workspace --exclude zed-al`
- [ ] Clippy clean: `cargo clippy --workspace --exclude zed-al -- -D warnings`
- [ ] Formatted: `cargo fmt --all`
- [ ] Only files related to this feature are modified
