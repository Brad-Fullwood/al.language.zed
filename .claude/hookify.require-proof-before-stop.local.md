---
name: require-proof-before-stop
enabled: false
event: stop
action: warn
conditions:
  - field: transcript
    operator: regex_match
    pattern: crates/.+\.rs
  - field: transcript
    operator: not_contains
    pattern: proof.toml
---

**Rust files were edited but no proof was recorded**

You edited `.rs` files this session but didn't write to `.claude/data/proof.toml`. If you completed a task, use `/complete-task` to record proof before stopping.

If you were only fixing bugs, refactoring, or doing exploratory work (not completing a plan task), this warning can be ignored.
