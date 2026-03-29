---
name: fix-issue
description: Fix a bug or implement a feature with enforced proof-of-work. Ensures tests cover both success and failure paths, code compiles, and changes are scoped correctly.
argument-hint: "[description of the issue to fix]"
allowed-tools: Read, Grep, Glob, Edit, Write, Bash, Agent
---

# Fix Issue: Enforced Workflow

You MUST follow these steps IN ORDER. Do not skip steps. Do not mark a step complete until you have actually done it.

## Step 1: Understand the Problem

- Read the relevant code files
- Identify the root cause (not just the symptom)
- State clearly: "The root cause is X because Y"

## Step 2: Plan the Fix

Before writing any code:
- List the specific files you will modify
- Explain what change you will make in each file
- Verify none of the files are out of scope
- Verify the fix doesn't violate any design rules (see CLAUDE.md)

## Step 3: Write a Failing Test FIRST

Before fixing the code:
1. Write a test that **reproduces the bug** (it should FAIL with the current code)
2. Run it: `cargo test -p <crate> -- <test_name>` — confirm it FAILS
3. If it passes, your test doesn't actually test the bug — rewrite it

For new features: write a test that exercises the new behavior — it should fail because the feature doesn't exist yet.

## Step 4: Implement the Fix

- Make the minimal change to fix the issue
- Do NOT touch unrelated files
- Do NOT refactor surrounding code
- Do NOT add docstrings to code you didn't change

## Step 5: Verify the Fix

1. Run the specific test: `cargo test -p <crate> -- <test_name>` — confirm it PASSES now
2. Run all tests for the affected crate: `cargo test -p <crate>`
3. Run clippy: `cargo clippy --workspace --exclude zed-al -- -D warnings`
4. Run fmt: `cargo fmt --all`

## Step 6: Add Negative Tests

Your test file MUST contain both:
- **Positive tests** — verify correct behavior with valid input
- **Negative tests** — verify correct behavior with invalid/edge-case input

At minimum, add one test that checks error handling:
- What happens with empty input?
- What happens with malformed input?
- What happens when a required resource is missing?

Name negative tests clearly: `test_*_invalid_*`, `test_*_missing_*`, `test_*_error_*`

## Step 7: Proof of Work Summary

Before finishing, provide this summary:

```
## Proof of Work

**Root cause:** [one sentence]
**Files changed:** [list]
**Test that reproduces the bug:** [test name] — confirmed FAIL before fix, PASS after
**Negative tests added:** [test names]
**Full test suite:** cargo test -p <crate> — [PASS/FAIL]
**Clippy:** PASS
**Formatting:** PASS
```

The Stop hook will verify compilation, clippy, and formatting. The review gate will check for scope creep, missing tests, and rule violations. If either blocks you, fix the issues — don't try to work around them.

## Issue Description

$ARGUMENTS
