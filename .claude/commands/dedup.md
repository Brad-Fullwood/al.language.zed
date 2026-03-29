---
description: Find duplicate and near-duplicate code across workspace crates
allowed-tools: Read, Grep, Glob, Bash
---

Systematically search for duplicate code across ALL crates in this workspace.

**Search method** — for each, grep the entire workspace and compare:

1. **Functions**: Similar names or signatures across crates
2. **Match patterns**: Repeated match arms, error handling blocks, tree-sitter traversal patterns
3. **Utilities**: Functions that duplicate what exists in another crate or in workspace deps (dashmap, ropey)
4. **Re-implementations**: Code that duplicates al-core/src/queries/ logic, or repeats al-syntax parsing
5. **Types**: Similar struct/enum definitions across crate boundaries

**Focus areas** (highest duplication risk):
- `al-core/src/queries/` vs `al-lsp/src/` — business logic leaking into transport
- `al-syntax/src/` vs `al-core/src/` — parsing logic repeated
- Error types defined in multiple crates
- Tree-sitter node traversal helpers copied between modules
- Helper functions in al-core/src/queries/mod.rs vs individual query files

For each duplicate: cite both file:line locations, say which crate should own the shared impl (lower = better), suggest the refactoring.
