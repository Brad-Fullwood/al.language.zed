---
name: supervise
description: "Verify progress, run infrastructure CI, triage and dispatch fixes. Triggered automatically by the Stop hook every 15 .rs edits — do not wait for manual invocation."
allowed-tools: Bash, Read, Grep, Glob, Edit, Agent
---

Run supervision. This is the enforcement loop — not just reporting, but driving resolution.

## 1. Infrastructure CI
Run `bash .claude/hooks/self-test.sh`. If any fail, run `/fix-infra` immediately.

## 2. Spawn Supervisor
Use the Agent tool to spawn the `supervisor` agent (sonnet, maxTurns 15). It will verify progress, test results, architecture, and triage every finding as STOP, PARALLEL, or SCHEDULE.

## 3. React to Triage

Read the supervisor's report and act on each triage level:

### STOP items
**Fix before doing anything else.** Either:
- Fix directly if simple (e.g., a one-line compilation error)
- Spawn the infra-fixer agent if it's infrastructure
- For code issues: fix them yourself — Priority-0

Do NOT proceed until all STOP items are resolved. Re-run `/test` after fixing.

### PARALLEL items
Spawn a **sonnet** background agent to fix these in a worktree while you continue.

### SCHEDULE items
Logged to `docs/issues.md` by the supervisor. No immediate action.

## 4. Spawn Adversarial (WPX mandatory)
After supervision, always spawn the adversarial agent in background to test recent changes:
```
Agent tool: subagent_type=adversarial, run_in_background=true, model=sonnet
```
Then record the adversarial run:
```bash
cat /tmp/al-edit-count > /tmp/al-adversarial-last-run
```

## 5. Write Supervision Proof & Reset Gate
```bash
# Tamper-resistant marker: commit hash + timestamp + edit count
echo "$(date +%s) $(git rev-parse HEAD 2>/dev/null) edits=$(cat /tmp/al-edit-count 2>/dev/null)" > /tmp/al-supervision-proof
echo 0 > /tmp/al-edit-count
```

## 6. Report to User
One-line summary: findings count, what was fixed, what's running in background. Then continue with current task.
