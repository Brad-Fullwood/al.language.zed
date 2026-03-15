---
name: review-milestone
description: Review a completed milestone — verify all tasks done, evidence recorded, tests pass
user_invocable: false
args: "<wp_name>"
---

# Review Work Package

Verify a completed work package has all tasks done, evidence recorded, and tests passing.

## Arguments

- `wp_name`: The work package to review (e.g., "WP1" or "WP1: al-core Skeleton & Discovery Migration")

## What To Do

Launch the supervisor agent:

```
Agent tool:
  subagent_type: supervisor
  model: sonnet
  description: "Review [WP_NAME]"
  prompt: |
    Review the completed work package [WP_NAME] in the Zed AL Extension project.

    Check the following:

    1. **Task completion**: Read .claude/data/tasks.toml — are all tasks in this WP marked [x]?

    2. **Evidence**: Read .claude/data/proof.toml — does every task in this WP have a PoF entry with both adversarial_pass and fidelity_pass?

    3. **Tests pass**: Run `cargo test --workspace --exclude zed-al 2>&1 | tail -30`

    4. **Architecture holds**: Run `cargo tree -p al-core` and verify no forbidden dependencies.
       Check that thin adapters (al-cli, al-explorer, al-mcp) have no path to al-core.

    Report findings as:
    - PASS items (verified)
    - FAIL items (with specific details)
    - WARN items (concerns that aren't blocking)
```

Replace `[WP_NAME]` with the actual work package name.

## When To Use

This review runs **automatically** via `/start-work` when a WP boundary is crossed. Use this skill directly only when:
- You want to manually verify a WP outside the normal workflow
- `/start-work` was interrupted before the review ran
- You want to re-review a WP after fixing issues
