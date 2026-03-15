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
    pattern: use\s+al_(?!protocol\b)
---

**BLOCKED: Thin adapter Rust import violation**

Thin adapters (al-cli, al-explorer, al-mcp) may ONLY import `al_protocol` (shared JSON-RPC types).

All business logic lives in al-core, accessed via JSON-RPC through the al-lsp daemon.

This is a whitelist rule — any new `al_*` crate is automatically blocked.
