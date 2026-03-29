---
paths:
  - "tree-sitter-al/**"
---

# Tree-sitter Grammar Rules

You are editing the **tree-sitter-al submodule**.

## What You Can Edit

- `grammar.js` — the grammar definition
- `generator/tools/al-gen/` — grammar rule generators
- `generator/tools/al-extract/` — AL syntax extraction from Microsoft DLLs
- `queries/` — highlight, indent, fold, text-object queries
- `data/` — JSON data files loaded at runtime by `al-syntax::LanguageData`
- `tests/` — test corpus and reference data

## What You Must NOT Edit

- `src/` — generated parser files (parser.c, etc.)
- `bindings/` — generated bindings
- `node_modules/` — dependencies

## After Making Changes

1. Commit and push **inside the submodule**: `cd tree-sitter-al && git add -A && git commit -m "..." && git push`
2. Update the submodule reference: `cd .. && git add tree-sitter-al && git commit -m "chore: update tree-sitter-al submodule"`

This ensures `git submodule update --init --recursive` fetches the correct revision.

## Regenerating

After editing `grammar.js`, regenerate with `npx tree-sitter generate` inside the submodule directory.
