---
name: find-bugs
description: Stress-test recently completed work — find edge cases and bugs, fix what's fixable, defer the rest
user_invocable: true
---

# Find Bugs

Spawn the adversarial agent to stress-test recently completed work. The agent finds bugs, fixes what it can, and defers what it can't.

## What To Do

Launch the adversarial agent in the background:

```
Agent tool:
  subagent_type: adversarial
  model: sonnet
  run_in_background: true
  description: "Find bugs in [TASK_ID]"
  prompt: |
    You are an adversarial tester for the Zed AL Extension project.

    Task just completed: [TASK_ID] - [TASK_NAME]
    Files changed: [LIST_FILES]

    Phase 1: Find bugs in the changed files (edge cases, panics, logic errors).
    Phase 2: Fix bugs you can fix now. Defer bugs that need other WPs.

    For fixes: edit the code, run cargo test + clippy to verify.
    For deferrals: append to .claude/data/issues.toml.

    Run: cargo test --workspace --exclude zed-al 2>&1 | tail -30
```

Replace `[TASK_ID]`, `[TASK_NAME]`, and `[LIST_FILES]` with actual values from the completed task.

## When To Use

- After completing any task from the plan
- After fixing a bug (to verify the fix doesn't break adjacent code)
- When the user asks for stress testing

The agent runs in background — do not wait for it. Continue to the next task.

## When Results Come Back

When you receive the adversarial agent's completion notification:
1. Read the report summary
2. If it fixed bugs: commit the fixes with message "Adversarial fix: [brief description]"
3. If it deferred bugs: they go to `.claude/data/issues.toml` — picked up when their blocking task completes
4. If tests broke: stop current work and fix the regression before continuing
