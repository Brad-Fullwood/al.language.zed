---
name: start-work
description: "Initialize a work session: run supervision, find the next task, begin implementation. Use at the start of every session."
allowed-tools: Bash, Read, Grep, Glob, Edit, Write, Agent
---

You are starting a work session. Follow these steps in order.

## Step 0: Quick Health Check

Run these shell commands first (cheap, no agent needed):
```bash
cargo check --workspace --exclude zed-al 2>&1 | tail -5
cargo test --workspace --exclude zed-al 2>&1 | tail -3
bash .claude/hooks/self-test.sh 2>&1 | tail -3
```

If ALL pass: skip to Step 1 (no need for full supervision on a clean start).
If ANY fail: run `/supervise` to triage and fix.

## Step 1: Find Your Task

Read `docs/plan.md` and `docs/progress.md`. Your task is the next unchecked item whose dependencies are all checked. Lowest task ID wins. Tell the user which task you're starting.

## Step 2: Read Context

Read every file listed in the task's "Files" field. Read pass/fail criteria. Do not write code until you understand.

## Step 3: Implement

Write the code. Hooks enforce constraints automatically. If blocked, read the error.

## Step 4: Verify

1. Run `cargo test --workspace --exclude zed-al` directly (don't spawn an agent for this).
2. Spawn adversarial agent in background: `Agent(subagent_type=adversarial, run_in_background=true)`
3. Create PoF entry — pipe test output directly:
   ```bash
   cargo test --workspace --exclude zed-al 2>&1 | tail -20
   ```
   Then append the output to `docs/proof_of_functionality.toml` using Edit tool.

## Step 5: Record

1. Check off the task in `docs/progress.md` with date and summary.
2. Commit the changes.

## Step 6: Continue or Stop

Go back to Step 1. The Stop hook tracks edits — after 15 .rs edits it forces `/supervise`.

At end of a Work Package, run `/audit <WP>`.

## Token Efficiency Rules

- Run cargo commands directly via Bash — do NOT spawn agents for mechanical tasks
- Use `| tail -N` to limit output. Full cargo output wastes tokens.
- Only spawn agents for tasks that need reasoning (adversarial, supervision triage)
- For PoF entries: pipe real command output, don't have Claude rewrite it
