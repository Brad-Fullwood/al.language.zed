---
name: require-cargo-check-before-stop
enabled: true
event: stop
action: warn
conditions:
  - field: transcript
    operator: regex_match
    pattern: crates/.+\.rs
  - field: transcript
    operator: not_contains
    pattern: cargo check
---

**No compilation check detected this session**

You should run `cargo check --workspace --exclude zed-al` before stopping, to verify the project compiles.

If this was a non-implementation session (planning, docs, audit), this warning can be ignored.
