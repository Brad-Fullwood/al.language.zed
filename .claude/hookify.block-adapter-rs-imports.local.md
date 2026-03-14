---
name: block-adapter-rs-imports
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-(cli|explorer|mcp)/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_(core|syntax|symbols|semantic|diag)
---

**BLOCKED: Thin adapter Rust import violation**

You are importing an internal crate in a thin adapter's source code.

Thin adapters (al-cli, al-explorer, al-mcp) must NOT import:
- `al_core`
- `al_syntax`
- `al_symbols`
- `al_semantic`
- `al_diag`

They may only import `al_protocol` for shared JSON-RPC types.

All business logic lives in al-core, accessed via JSON-RPC through the al-lsp daemon.
