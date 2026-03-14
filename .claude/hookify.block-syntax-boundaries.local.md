---
name: block-syntax-boundaries
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-syntax/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_(core|lsp|symbols|semantic|diag)
---

**BLOCKED: al-syntax dependency violation**

al-syntax is a standalone analysis library. It must NOT import:
- `al_core`, `al_lsp`, `al_symbols`, `al_semantic`, `al_diag`

al-syntax depends only on tree-sitter and std. No upward or lateral dependencies.
