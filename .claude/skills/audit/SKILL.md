---
name: audit
description: Spawn supervisor agent for deep WP-level audit — verify all tasks have evidence, tests pass, architecture holds
user_invocable: true
args: "<wp_name>"
---

# Audit

Spawn the supervisor agent to perform a deep audit of a completed work package.

## Arguments

- `wp_name`: The work package to audit (e.g., "WP1" or "WP1: al-core Skeleton & Discovery Migration")

## What To Do

Launch the supervisor agent:

```
Agent tool:
  subagent_type: supervisor
  model: sonnet
  description: "Audit [WP_NAME]"
  prompt: |
    Perform a deep audit of [WP_NAME] in the Zed AL Extension project.

    Check the following:

    1. **Task completion**: Read docs/progress.md — are all tasks in this WP marked [x]?

    2. **Evidence**: Read docs/proof_of_functionality.toml — does every task in this WP have a PoF entry with both adversarial_pass and fidelity_pass?

    3. **Tests pass**: Run `cargo test --workspace --exclude zed-al 2>&1 | tail -30`

    4. **Architecture holds**: Run `cargo tree -p al-core` and verify no forbidden dependencies.
       Check that thin adapters (al-cli, al-explorer, al-mcp) have no path to al-core.

    5. **Code review**: Use superpowers:requesting-code-review to review the implementation quality of changed files in this WP.

    Report findings as:
    - PASS items (verified)
    - FAIL items (with specific details)
    - WARN items (concerns that aren't blocking)
```

Replace `[WP_NAME]` with the actual work package name.

## When To Use

- After completing all tasks in a WP
- Before starting the next WP
- When the user wants confidence that a milestone is solid
