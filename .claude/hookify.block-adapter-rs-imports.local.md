---
name: block-adapter-rs-imports
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-cli/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_(?!protocol\b)
---

**BLOCKED: al-cli Rust import violation**

al-cli may ONLY import `al_protocol` (shared types + discovery). It connects to al-lsp via JSON-RPC at runtime.

All business logic lives in al-core, accessed via JSON-RPC through the al-lsp daemon.

Note: al-explorer may import al_protocol and al_symbols. al-mcp has no al-* imports (shells out to `al` binary).

This is a whitelist rule — any new `al_*` crate in al-cli is automatically blocked.
