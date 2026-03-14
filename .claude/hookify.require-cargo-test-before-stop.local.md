---
name: require-cargo-test-before-stop
enabled: true
event: stop
action: block
---

**BLOCKED: Cannot stop without running tests**

You must run `cargo test --workspace --exclude zed-al` before stopping or claiming task completion.

The conversation transcript does not show evidence of test execution.

Run the tests now and verify they pass before stopping.
