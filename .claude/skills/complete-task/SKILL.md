---
name: complete-task
description: Complete a task — run tests, record proof, update progress, spawn bug finder. The single "task done" action.
user_invocable: false
args: "<task_id> <wp_name>"
---

# Complete Task — Record Proof

This is the **single action** for completing a task. It runs tests, records evidence, updates progress, and spawns the bug finder. Do not do these steps individually — always use this skill.

## Arguments

- `task_id`: The task ID (e.g., T101)
- `wp_name`: The work package name (e.g., "WP1: al-core Skeleton & Discovery Migration")

## Steps

### 1. Run Tests Fresh

```bash
cargo test --workspace --exclude zed-al 2>&1 | tail -40
```

You MUST run this now and read the real output. Do not reuse old output or fabricate results. If tests fail, fix them before continuing — do NOT record proof of broken code.

### 2. Run Compilation Check

```bash
cargo check --workspace --exclude zed-al 2>&1 | tail -20
```

Must pass cleanly.

### 3. Write the PoF Entry

Append a `[[entries]]` block to `.claude/data/proof.toml`:

```toml
[[entries]]
date = "YYYY-MM-DD"
work_package = "WPX: Name"
task_id = "TXXX: Description"

[entries.adversarial_pass]
context = "What negative scenario was tested"
expected = "Expected failure behavior"
actual_log = """
<paste REAL test output here>
"""
status = "FAILED_AS_EXPECTED"

[entries.fidelity_pass]
context = "What positive scenario was tested"
expected = "Expected success behavior"
actual_log = """
<paste REAL test output here>
"""
status = "SUCCESS"
```

If no adversarial tests exist for this task, write one before creating the entry.

### 4. Update Progress

Update both files:
1. `.claude/data/task-index.toml` — change `done = false` to `done = true` for this task ID
2. `.claude/data/tasks.toml` — set `completed = true`, add `completed_date` and `completed_note`

Use targeted edits (Edit tool), do NOT read the full tasks.toml.
### 5. Verify

Read back the PoF entry to confirm it parses as valid TOML and contains real output.

### 6. Invoke /find-bugs

Invoke `/find-bugs` to spawn the adversarial agent in the background. Pass the task details:

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

Do not wait for this — continue immediately.

## Rules

- **NEVER fabricate test output.** Every `actual_log` must come from a command you just ran.
- **NEVER skip steps.** All 6 steps are mandatory, in order.
- Use today's date.
- Both adversarial_pass and fidelity_pass are required.
