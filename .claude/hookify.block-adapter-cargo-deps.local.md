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
    pattern: al-(?!daemon-client)
---

**BLOCKED: Thin adapter Cargo.toml dependency violation**

Thin adapters (al-cli, al-explorer) have ZERO al-* compile-time dependencies. They connect to al-lsp via JSON-RPC at runtime.

al-mcp has NO al-* compile-time dependencies. It shells out to the `al` CLI binary at runtime.

All other `al-*` crate dependencies are forbidden.

This is a whitelist rule — any new `al-*` crate is automatically blocked.
