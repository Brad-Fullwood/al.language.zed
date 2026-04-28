---
description: Find duplicate and near-duplicate code across workspace crates
allowed-tools: Read, Grep, Glob, Bash
---

Systematically search for duplicate code across ALL crates in this workspace.

**Search method** — for each, grep the entire workspace and compare:

1. **Functions**: Similar names or signatures across crates
2. **Match patterns**: Repeated match arms, error handling blocks, tree-sitter traversal patterns
3. **Utilities**: Functions that duplicate what exists in another crate or in workspace deps (dashmap, ropey)
4. **Re-implementations**: Code that duplicates `al_core::queries::*` logic, or repeats `al_core::syntax` parsing helpers
5. **Types**: Similar struct/enum definitions across module boundaries

**Focus areas** (highest duplication risk):
- `al-core/src/queries/` vs `al-core/src/server/` — business logic leaking into transport (the boundary is now a coding rule, not compile-enforced)
- `al-core/src/syntax/` vs `al-core/src/queries/` — parsing helpers reimplemented inline
- Error types redefined inside individual query modules instead of `al_core::errors`
- Tree-sitter node traversal helpers copied between modules instead of using `al_core::syntax::traversal`
- Helper functions in `al-core/src/queries/mod.rs` vs individual query files

For each duplicate: cite both file:line locations, say which crate should own the shared impl (lower = better), suggest the refactoring.
