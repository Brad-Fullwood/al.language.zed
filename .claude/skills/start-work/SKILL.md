---
name: start-work
description: "Initialize a work session: run supervision, find the next task, begin implementation. Use at the start of every session."
allowed-tools: Bash, Read, Grep, Glob, Edit, Write, Agent
---

You are starting a work session. Follow these steps in order.

## Step 0: Supervision

Run `/supervise`. This runs the infrastructure self-test, verifies all previously claimed progress is real, and resets the edit counter. If it reports BLOCKED, fix those issues first.

## Step 1: Find Your Task

Read `docs/plan.md` and `docs/progress.md`. Your task is the next unchecked item whose dependencies are all checked. Lowest task ID wins if multiple are available. Tell the user which task you're starting and why.

## Step 2: Read Context

Read every file listed in the task's "Files" field. Read the pass/fail criteria. Read referenced docs. Do not write code until you understand what you're building.

## Step 3: Implement

Write the code. Hooks enforce constraints automatically. If blocked, read the error.

## Step 4: Verify

1. `/test` — verify all tests pass.
2. `/adversarial` — write breaking tests. Fix real issues.
3. `/pof <WP> <task>` — create evidence entry with real test output.

## Step 5: Record

Check off the task in `docs/progress.md` with date and summary.

## Step 6: Continue or Stop

Go back to Step 1 for the next task. The Stop hook tracks your edits — after 15 .rs edits it will block and force `/supervise`. You don't need to remember to call it.

At end of a Work Package, run `/audit <WP>`.

## If Something Goes Wrong

- Infrastructure broken: `/fix-infra <description>`
- Problem or idea to log: `/report <description>`
- Architecture question: check `.claude/constraints.toml` or ask the user
