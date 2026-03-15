---
name: block-dap-client-boundaries
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-dap-client/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_
---

**BLOCKED: al-dap-client dependency violation**

al-dap-client is a standalone analysis library. It must NOT import ANY `al_*` crate.

External crate dependencies are fine — this rule only blocks internal `al_*` imports.

Whitelist rule — any new `al_*` crate is automatically blocked. Current al-protocol usage (ISSUE-021) is a known violation being refactored out.
