---
name: supervise
description: "Verify progress, run infrastructure CI, reset the supervision gate. Triggered automatically by the Stop hook every 15 .rs edits — do not wait for manual invocation."
allowed-tools: Bash, Read, Grep, Glob, Edit, Agent
---

Run a full supervision check. This is the "boss walk-in." Do these steps in order:

## 1. Infrastructure CI
Run `bash .claude/hooks/self-test.sh`. If any tests fail, run `/fix-infra` before continuing.

## 2. Spawn Supervisor Agent
Use the Agent tool to spawn the `supervisor` agent. It will:
- Verify code compiles and tests pass
- Audit progress.md claims against actual code
- Check PoF entries for completeness
- Verify architecture compliance
- Review docs/issues.md for unresolved items
- Spot-check recently edited files for rule violations

## 3. Reset Gate
After the supervisor reports, reset the edit counter so the Stop hook allows work to continue:
```bash
echo 0 > /tmp/al-edit-count
```

## 4. Act on Findings
If the supervisor reports BLOCKED or ISSUES:
- Fix all Action Required items before starting new tasks
- Use `/report` to log any new issues found
- Use `/fix-infra` for any broken infrastructure

Do NOT skip step 3 — without the reset, the Stop hook will keep blocking.
