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
    pattern: al-(core|syntax|symbols|semantic|diag)
---

**BLOCKED: Thin adapter Cargo.toml dependency violation**

You are adding a dependency on an internal crate to a thin adapter's Cargo.toml.

Thin adapters (al-cli, al-explorer, al-mcp) must have ZERO compile-time dependency on:
- al-core
- al-syntax
- al-symbols
- al-semantic
- al-diag

They may only depend on **al-protocol** (shared JSON-RPC types).

Thin adapters communicate with al-lsp via JSON-RPC at runtime, not via direct crate imports.
