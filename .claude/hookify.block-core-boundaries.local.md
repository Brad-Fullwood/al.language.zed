---
name: block-core-boundaries
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-core/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_(lsp|cli|explorer|mcp)
---

**BLOCKED: al-core upward dependency violation**

You are importing a server or adapter crate from al-core.

al-core must NOT import:
- `al_lsp` (server — al-core is a library used BY al-lsp)
- `al_cli` (thin adapter)
- `al_explorer` (thin adapter)
- `al_mcp` (thin adapter)

al-core may import: `al_syntax`, `al_symbols`, `al_semantic`, `al_diag`.

Dependencies flow downward: al-lsp → al-core → analysis libs.
