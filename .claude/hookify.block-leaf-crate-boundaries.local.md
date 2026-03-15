---
name: block-leaf-crate-boundaries
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-diag/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_
---

**BLOCKED: al-diag dependency violation**

al-diag is a standalone tracing/logging layer. It must NOT import ANY `al_*` crate.

External crate dependencies are fine — this rule only blocks internal `al_*` imports.

Whitelist rule — any new `al_*` crate is automatically blocked.
