---
name: adversarial
description: Spawn adversarial testing agent in background to find edge cases and break recently completed code
user_invocable: true
---

# Adversarial Testing

Spawn the adversarial agent to stress-test recently completed work.

## What To Do

Launch the adversarial agent in the background:

```
Agent tool:
  subagent_type: adversarial
  model: sonnet
  run_in_background: true
  description: "Adversarial test [TASK_ID]"
  prompt: |
    You are an adversarial tester for the Zed AL Extension project.

    Task just completed: [TASK_ID] - [TASK_NAME]
    Files changed: [LIST_FILES]

    Your job:
    1. Read the changed files
    2. Write tests designed to BREAK the implementation
    3. Focus on: edge cases, empty inputs, malformed data, concurrent access, off-by-one errors
    4. Run `cargo test --workspace --exclude zed-al` to see if your tests expose bugs
    5. If you find bugs, write a clear report of what broke and why

    Do NOT fix bugs — only find and report them.

    Run: cargo test --workspace --exclude zed-al 2>&1 | tail -30
```

Replace `[TASK_ID]`, `[TASK_NAME]`, and `[LIST_FILES]` with actual values from the completed task.

## When To Use

- After completing any task from the plan
- After fixing a bug (to verify the fix doesn't break adjacent code)
- When the user asks for stress testing

The agent runs in background — do not wait for it. Continue to the next task.
