---
name: require-cargo-test-before-stop
enabled: true
event: stop
action: warn
conditions:
  - field: transcript
    operator: regex_match
    pattern: crates/.+\.rs
  - field: transcript
    operator: not_contains
    pattern: cargo test
---

**No test execution detected this session**

You should run `cargo test --workspace --exclude zed-al` before stopping, to verify nothing is broken.

If this was a non-implementation session (planning, docs, audit), this warning can be ignored.
