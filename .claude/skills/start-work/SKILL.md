---
name: start-work
description: Begin a work session — health check, fix actionable issues, find next task, implement with TDD
user_invocable: true
---

# Start Work

You are beginning a work session on the Zed AL Extension project.

## Step 1: Health Check

Invoke `/check-health` which spawns the health-checker subagent (sonnet, read-only). React to results:
- Compilation fails → fix before proceeding
- STOP issues → report to user, ask whether to proceed
- Broken hookify → spawn infra-fixer
- Boundary violations → log via `/report-issue`

## Step 2: Fix Actionable Issues

Invoke `/fix-issues` which spawns the issue-fixer subagent (opus). It will:
- Check `.claude/data/issues.toml` for actionable issues
- Fix what it can, report what needs design decisions
- If nothing actionable, reports that and moves on

## Step 3: Find Next Task

Read `.claude/data/task-index.toml` (84 lines, NOT the full tasks.toml). Find the first task where `done = false` and all `deps` are `done = true`.

Then get full details for that specific task:
```bash
python3 -c "
import tomllib
with open('.claude/data/tasks.toml','rb') as f: d=tomllib.load(f)
t = next(t for t in d['tasks'] if t['id']=='TASK_ID')
for k,v in t.items(): print(f'{k}: {v}')
"
```
Replace TASK_ID with the actual ID.

Read `.claude/data/features.toml` to verify the task is in scope (status = "supported" or "planned").

**If all tasks in the current WP are done — review before moving on:**

Invoke `/review-milestone` for the completed WP. If it fails, fix failures before starting next WP.

## Step 4: Announce

Tell the user: task ID + name, what it involves, which files.

## Step 5: Implement

Use `superpowers:test-driven-development`:
1. Write a failing test that validates the pass criteria
2. Implement until the test passes
3. `cargo test --workspace --exclude zed-al 2>&1 | tail -30`
4. `cargo clippy --workspace --exclude zed-al 2>&1 | tail -20`

## Step 6: Complete Task

Invoke `/complete-task <task_id> <wp_name>`. It handles tests, proof, task-index update, and spawning `/find-bugs` in background.

After completion, **loop back to Step 3**. Do not stop.

## Handling Bug-Finder Results

When `/find-bugs` agent completes (background notification):
1. If it **fixed bugs**: commit with "Adversarial fix: [description]"
2. If it **deferred bugs**: logged to issues.toml
3. If **tests broke**: stop current work, fix regression, resume
