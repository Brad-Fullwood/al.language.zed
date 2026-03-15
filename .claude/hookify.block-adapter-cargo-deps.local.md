---
name: block-adapter-cargo-deps
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-(cli|mcp)/Cargo\.toml$
  - field: new_text
    operator: regex_match
    pattern: al-(?!protocol)
---

**BLOCKED: Thin adapter Cargo.toml dependency violation**

al-cli may ONLY depend on `al-protocol` (shared types + discovery). It connects to al-lsp via JSON-RPC at runtime.

al-mcp has NO al-* compile-time dependencies. It shells out to the `al` CLI binary at runtime.

al-explorer is exempt from this rule — it directly imports al-symbols and al-protocol for its TUI.

All other `al-*` crate dependencies in al-cli or al-mcp are forbidden.

This is a whitelist rule — any new `al-*` crate in al-cli or al-mcp is automatically blocked.
