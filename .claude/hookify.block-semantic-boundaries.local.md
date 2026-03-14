---
name: block-semantic-boundaries
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-semantic/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_(core|lsp|syntax|symbols|diag)
---

**BLOCKED: al-semantic dependency violation**

al-semantic is a standalone analysis library. It must NOT import:
- `al_core`, `al_lsp`, `al_syntax`, `al_symbols`, `al_diag`

al-semantic handles .NET CLR bridge independently via netcorehost.
