---
name: test
description: "Run cargo tests for the workspace"
argument-hint: "[crate-name or test filter]"
allowed-tools: Bash, Read
---

Run the test suite. If $ARGUMENTS is provided, use it as a filter.

## Steps

1. If `$ARGUMENTS` is empty, run: `cargo test --workspace --exclude zed-al 2>&1`
2. If `$ARGUMENTS` is a crate name (e.g., `al-test-harness`), run: `cargo test -p $ARGUMENTS 2>&1`
3. If `$ARGUMENTS` contains `::`, treat it as a test name filter: `cargo test --workspace --exclude zed-al "$ARGUMENTS" 2>&1`
4. Report: total passed / failed / ignored. For failures, show the test name and first 5 lines of error output.
