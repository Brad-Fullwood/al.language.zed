Review the current uncommitted changes for quality and correctness.

Check for:
1. **Scope creep** — Are there changes to files unrelated to the current task?
2. **Hardcoded AL values** — Any const arrays of keywords, builtins, types, or object kinds?
3. **Dependency violations** — Do the changes respect the dependency hierarchy? (al-syntax/al-symbols/al-semantic must not depend on each other or al-core)
4. **Business logic in al-lsp** — Any query logic that should be in al-core instead?
5. **Unwrap in non-test code** — Any `.unwrap()` or `.expect()` in library/handler code?
6. **UTF-16 handling** — Are LSP positions correctly converted to byte offsets?
7. **Recursive tree-sitter traversal** — Should be iterative with explicit stack
8. **DashMap refs held across await** — Will cause deadlocks
9. **Missing tests** — Does the change add or modify behavior without test coverage?

Run `git diff` and `git diff --cached` to see all changes, then provide a detailed review with specific file:line citations.

Rate severity: BLOCKER (must fix), WARNING (should fix), NOTE (consider).
