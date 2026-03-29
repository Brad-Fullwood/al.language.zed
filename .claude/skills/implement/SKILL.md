---
name: implement
description: Implement a feature with enforced proof-of-work. Ensures proper architecture, tests with both pass and fail paths, and scoped changes.
argument-hint: "[description of the feature to implement]"
allowed-tools: Read, Grep, Glob, Edit, Write, Bash, Agent
---

# Implement Feature: Enforced Workflow

You MUST follow these steps IN ORDER. Do not skip steps.

## Step 1: Research

- Read CLAUDE.md for architecture rules
- Identify which crate(s) this feature touches
- Read existing code in those areas
- State: "This feature belongs in [crate] because [reason]"

## Step 2: Design

Before writing code:
- Which al-core query function(s) will you add/modify?
- What transport wiring is needed in al-lsp?
- Does daemon mode need changes?
- What types will you define? (must be transport-agnostic in al-core)
- List every file you plan to touch

## Step 3: Write Tests First

Before implementing:
1. Write tests that define the expected behavior
2. Include BOTH:
   - **Positive tests** — valid input produces correct output
   - **Negative tests** — invalid input returns error/None gracefully
3. Run tests — confirm they FAIL (feature doesn't exist yet)

## Step 4: Implement (al-core first)

1. Add business logic in `al-core/src/queries/` — transport-agnostic types only
2. Wire transport in `al-lsp` — thin conversion layer only
3. Add daemon dispatch if needed

Rules during implementation:
- No hardcoded AL values — use LanguageData / al-symbols
- No tower_lsp types in al-core query returns
- UTF-16 positions converted to bytes before string ops
- Iterative tree-sitter traversal (no recursion)
- No DashMap refs held across await points

## Step 5: Verify

1. Run your new tests — confirm they PASS
2. Run full crate tests: `cargo test -p <crate>`
3. Run clippy: `cargo clippy --workspace --exclude zed-al -- -D warnings`
4. Run fmt: `cargo fmt --all`
5. Run scope check: `git diff --stat` — verify only expected files changed

## Step 6: Proof of Work Summary

```
## Proof of Work

**Feature:** [one sentence]
**Architecture:** [which crate, which query module]
**Files changed:** [list]
**Positive tests:** [test names] — all PASS
**Negative tests:** [test names] — all PASS (verify error handling)
**Full test suite:** cargo test -p <crate> — PASS
**Clippy:** PASS
**Formatting:** PASS
**Scope check:** only expected files changed
```

## Feature Description

$ARGUMENTS
