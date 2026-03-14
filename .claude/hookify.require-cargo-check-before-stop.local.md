---
name: require-cargo-check-before-stop
enabled: true
event: stop
action: block
---

**BLOCKED: Cannot stop without verifying compilation**

You must run `cargo check --workspace --exclude zed-al` before stopping or claiming task completion.

The conversation transcript does not show evidence of compilation verification.

Run the check now and verify it succeeds before stopping.
