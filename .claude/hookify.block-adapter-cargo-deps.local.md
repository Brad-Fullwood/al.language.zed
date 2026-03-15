---
name: block-adapter-cargo-deps
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-(cli|explorer|mcp)/Cargo\.toml$
  - field: new_text
    operator: regex_match
    pattern: al-(?!protocol)
---

**BLOCKED: Thin adapter Cargo.toml dependency violation**

Thin adapters (al-cli, al-explorer, al-mcp) may ONLY depend on `al-protocol` (shared JSON-RPC types).

All other `al-*` crate dependencies are forbidden. Adapters communicate with al-lsp via JSON-RPC at runtime.

This is a whitelist rule — any new `al-*` crate is automatically blocked.
