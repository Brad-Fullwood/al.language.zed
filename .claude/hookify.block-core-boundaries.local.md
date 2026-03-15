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
    pattern: use\s+al_(?!(syntax|symbols|semantic|dap_client)\b)
---

**BLOCKED: al-core dependency violation**

al-core may ONLY import these `al_*` crates:
- `al_syntax`, `al_symbols`, `al_semantic`, `al_dap_client`

It must NOT import server or adapter crates (al_lsp, al_cli, al_explorer, al_mcp) or any future `al_*` crate not listed above.

This is a whitelist rule — any new `al_*` crate is automatically blocked unless added here.
