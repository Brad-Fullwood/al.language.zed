---
name: block-syntax-boundaries
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: crates/al-syntax/.*\.rs$
  - field: new_text
    operator: regex_match
    pattern: use\s+al_
---

**BLOCKED: al-syntax dependency violation**

al-syntax is a standalone analysis library. It must NOT import ANY `al_*` crate.

External crate dependencies are fine — this rule only blocks internal `al_*` imports.

Whitelist rule — any new `al_*` crate is automatically blocked. See ISSUE-013 for existing tower-lsp violation.
