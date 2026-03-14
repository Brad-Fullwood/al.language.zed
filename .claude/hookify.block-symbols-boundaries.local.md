---
name: block-symbols-boundaries
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-symbols/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_(core|lsp|syntax|semantic|diag)
---

**BLOCKED: al-symbols dependency violation**

al-symbols is a standalone analysis library. It must NOT import:
- `al_core`, `al_lsp`, `al_syntax`, `al_semantic`, `al_diag`

al-symbols handles .app package symbol indexing independently.
