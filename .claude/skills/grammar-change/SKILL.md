---
name: grammar-change
description: Guide for modifying the tree-sitter-al grammar. Enforces the correct submodule workflow to prevent common agent mistakes (editing generated files, forgetting submodule push).
argument-hint: "[description of the grammar change needed]"
allowed-tools: Read, Grep, Glob, Edit, Write, Bash, Agent
---

# Grammar Change: tree-sitter-al Submodule Workflow

tree-sitter-al is a **git submodule** with its own repository. Changes require a specific workflow. Follow these steps IN ORDER.

## Step 1: Identify What to Edit

**You MAY edit:**
- `tree-sitter-al/grammar.js` — the grammar definition
- `tree-sitter-al/generator/tools/al-gen/` — grammar rule generators
- `tree-sitter-al/generator/tools/al-extract/` — AL syntax extraction tools
- `tree-sitter-al/queries/` — highlight, indent, fold, text-object queries
- `tree-sitter-al/data/` — JSON data files loaded by `al-syntax::LanguageData`
- `tree-sitter-al/tests/` — test corpus and reference data

**You MUST NOT edit:**
- `tree-sitter-al/src/` — GENERATED (parser.c, etc.) — regenerate instead
- `tree-sitter-al/bindings/` — GENERATED — regenerate instead
- `tree-sitter-al/node_modules/` — dependencies

If your change touches `src/` or `bindings/`, STOP. You're editing generated output.

## Step 2: Make the Change

Edit the appropriate source file(s) identified in Step 1.

For grammar changes (`grammar.js`):
- Understand the existing rule structure before modifying
- Keep rules composable — prefer small, named rules over inline complexity
- Add test cases in `tree-sitter-al/tests/corpus/` for new or changed rules

For query changes (`queries/*.scm`):
- Test against real AL files, not just synthetic snippets
- Verify highlights render correctly in Zed

For data changes (`data/*.json`):
- These are loaded by `al-syntax::LanguageData` at runtime
- Validate JSON is well-formed after editing

## Step 3: Regenerate (if grammar.js changed)

```bash
cd tree-sitter-al && npx tree-sitter generate
```

This regenerates `src/parser.c` and related files. If generation fails, fix the grammar — do NOT hand-edit `src/`.

## Step 4: Run Tests

```bash
cd tree-sitter-al && npx tree-sitter test
```

Fix any test failures before proceeding. If you added new grammar rules, ensure you also added corpus tests.

## Step 5: Verify Rust Crates Still Build

```bash
cargo check --workspace --exclude zed-al
cargo test -p al-syntax
```

Grammar changes can break the Rust parser bindings. Fix any compilation or test failures.

## Step 6: Commit INSIDE the Submodule

```bash
cd tree-sitter-al
git add -A
git commit -m "feat: <description of grammar change>"
git push
```

The submodule has its own repository. Changes must be committed and pushed there FIRST.

## Step 7: Update the Parent Submodule Reference

```bash
cd ..  # back to project root
git add tree-sitter-al
git commit -m "chore: update tree-sitter-al submodule"
```

This records the new submodule commit hash in the parent repository.

## Common Mistakes to Avoid

1. **Editing `src/parser.c` directly** — it gets overwritten on next `tree-sitter generate`
2. **Forgetting `git push` inside the submodule** — the parent ref points to a commit that doesn't exist on the remote
3. **Committing only in the parent** — the submodule changes won't be available to other clones
4. **Not running `tree-sitter test`** — grammar regressions are hard to debug later
5. **Not checking Rust builds** — `al-syntax` binds to the generated parser; grammar changes can break it
