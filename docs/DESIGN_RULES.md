# Design Rules

These rules govern all code changes in this project. They are enforced by hooks and code review.

---

## Rule 1: No Hardcoded AL Language Values

AL is updated every Business Central release. Hardcoded lists break silently.

**Use instead:**
- `al-syntax::LanguageData` — keywords, builtins, types (from `tree-sitter-al/data/`)
- `al-symbols` — object types, procedures, fields, events (from `.app` packages)
- `al-semantic` — .NET CLR semantic info

**If data isn't available:** Update `tree-sitter-al/generator/tools/al-extract/` to extract it.

---

## Rule 2: Business Logic in al-core Only

`al-lsp` is a **transport layer**. It converts between LSP/JSON-RPC wire format and al-core types.

**Correct:**
```
al-lsp handler → call al-core query → convert result to LSP type → return
```

**Wrong:**
```
al-lsp handler → read tree-sitter nodes → compute result → return
```

---

## Rule 3: Dependency Direction is Downward Only

```
al-lsp → al-core → {al-syntax, al-symbols, al-semantic, al-dap-client, al-daemon-client}
```

Leaf crates (al-syntax, al-symbols, al-semantic) must never import each other or al-core.

---

## Rule 4: UTF-16 Position Handling

LSP positions are UTF-16 code units. Rust strings are UTF-8 bytes.

**Always convert** using the rope's `utf16_cu_to_byte()` before indexing into strings. Never treat `position.character` as a byte offset.

---

## Rule 5: Iterative Tree-sitter Traversal

Use explicit `Vec<Node>` stack, not recursion. Deeply nested AL files can blow the call stack.

---

## Rule 6: No DashMap Refs Across Await Points

Clone data out of DashMap guards immediately, drop the guard, then `.await`.

---

## Rule 7: Scope Discipline

Only change files directly related to the task. No opportunistic cleanup, no drive-by refactoring, no "while I'm here" improvements.

---

## Rule 8: Commit Standards

- Every commit must compile and pass tests
- Use conventional commits: `feat(crate):`, `fix(crate):`, `refactor(crate):`, `chore:`, `docs:`
- One logical change per commit
- No `WIP` commits on shared branches

---

## Rule 9: tree-sitter-al Generated Files are Read-Only

`tree-sitter-al/` is a git submodule. You MAY edit `grammar.js`, `generator/`, `queries/`, `data/`, and `tests/`. You MUST NOT edit `src/` or `bindings/` — these are generated.

After making changes in the submodule:
1. Commit and push inside `tree-sitter-al/`
2. Update the submodule reference: `git add tree-sitter-al` in the parent repo

---

## Rule 10: zed-al is Isolated

The WASM extension (`zed-al`, root `src/`) has no compile-time dependency on native crates. Don't add one. Build it separately with `--target wasm32-wasip1`.
