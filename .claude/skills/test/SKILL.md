---
name: test
description: "Run cargo tests for the workspace"
argument-hint: "[crate-name or test filter]"
allowed-tools: Bash, Read
---

Run the test suite. If $ARGUMENTS is provided, use it as a filter.

## Steps

1. If `$ARGUMENTS` is empty, run: `cargo test --workspace --exclude zed-al 2>&1 | tail -30`
2. If `$ARGUMENTS` is a crate name (e.g., `al-test-harness`), run: `cargo test -p $ARGUMENTS 2>&1 | tail -30`
3. If `$ARGUMENTS` contains `::`, treat it as a test name filter: `cargo test --workspace --exclude zed-al "$ARGUMENTS" 2>&1 | tail -20`
4. Report: total passed / failed / ignored. For failures, show test name and first 3 lines of error.

## Token Efficiency
- Always pipe through `| tail` to limit output. Full cargo output wastes tokens.
- If all tests pass, a one-line summary is sufficient.
- Only show full failure output for the FIRST 3 failing tests.
