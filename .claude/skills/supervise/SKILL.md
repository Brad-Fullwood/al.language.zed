---
name: supervise
description: "Verify progress, run infrastructure CI, triage and dispatch fixes. Triggered automatically by the Stop hook every 15 .rs edits — do not wait for manual invocation."
allowed-tools: Bash, Read, Grep, Glob, Edit, Agent
---

Run supervision. This is the enforcement loop — not just reporting, but driving resolution.

## 1. Infrastructure CI
Run `bash .claude/hooks/self-test.sh`. If any fail, run `/fix-infra` immediately.

## 2. Spawn Supervisor
Use the Agent tool to spawn the `supervisor` agent. It will verify progress, test results, architecture, and triage every finding as STOP, PARALLEL, or SCHEDULE.

## 3. React to Triage

Read the supervisor's report and act on each triage level:

### STOP items
**You must fix these before doing anything else.** Either:
- Fix directly if simple (e.g., a one-line compilation error you introduced)
- Spawn the infra-fixer agent if it's infrastructure (hooks, rules, skills)
- For code issues: fix them yourself — you are the PM, this is Priority-0

Do NOT proceed to any other work until all STOP items are resolved. Re-run `/test` after fixing to confirm.

### PARALLEL items
Spawn a background agent to fix these while you continue with task work:
```
Use Agent tool with:
  - description: "Fix: <issue description>"
  - model: sonnet
  - isolation: worktree
  - run_in_background: true
```
The background agent fixes the issue in a worktree. When it completes, review and merge its changes.

### SCHEDULE items
These are logged to `docs/issues.md` by the supervisor. No immediate action needed — they'll be addressed in future tasks or sessions.

## 4. Reset Gate
After handling STOP items (or if there are none):
```bash
echo 0 > /tmp/al-edit-count
```

## 5. Report to User
Summarize: what was found, what was fixed, what's running in background, what was scheduled. Then continue with the current task.
