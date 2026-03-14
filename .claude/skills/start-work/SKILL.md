---
name: start-work
description: Begin a work session — verify compilation, find next task from plan, start implementation with TDD
user_invocable: true
---

# Start Work

You are beginning a work session on the Zed AL Extension project.

## Step 1: Health Check

Run compilation check:
```bash
cargo check --workspace --exclude zed-al 2>&1 | tail -20
```

If it fails, fix compilation errors before proceeding.

## Step 2: Find Next Task

Read `docs/progress.md` and `docs/plan.md`.

In progress.md, find the first unchecked task (`- [ ]`). Cross-reference with plan.md to get:
- Task ID and name
- File ownership
- Dependencies (verify they're complete)
- Pass/fail criteria

If all tasks in the current WP are done, move to the next WP.

## Step 3: Announce

Tell the user:
- Which task you're starting (ID + name)
- What it involves
- Which files you'll touch

## Step 4: Implement

Use `superpowers:test-driven-development` to implement the task:
1. Write a failing test that validates the pass criteria
2. Implement until the test passes
3. Run `cargo test --workspace --exclude zed-al 2>&1 | tail -30` to verify
4. Run `cargo clippy --workspace --exclude zed-al 2>&1 | tail -20` for lint

## Step 5: Complete Task

When implementation passes all tests:

1. Create a PoF entry — invoke `/pof` with the task ID and WP name
2. Update `docs/progress.md` — mark the task as `[x]`
3. Spawn adversarial agent in background — invoke `/adversarial`
4. **Continue to next task** — do not stop, immediately find and start the next task
