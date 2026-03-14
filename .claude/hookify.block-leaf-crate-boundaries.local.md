---
name: block-leaf-crate-boundaries
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-diag/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_(core|lsp|symbols|semantic)
---

**BLOCKED: al-diag dependency violation**

al-diag must NOT import:
- `al_core`, `al_lsp`, `al_symbols`, `al_semantic`

al-diag MAY import `al_syntax` (for parse-tree-based analysis).
No other internal crate dependencies are allowed.
