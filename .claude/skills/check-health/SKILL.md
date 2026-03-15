---
name: check-health
description: Full project health check — compilation, tests, architecture boundaries, hookify rules, config files. Spawns a subagent so the main agent stays light.
user_invocable: true
---

# Check Health

Spawn the health-checker agent to run all project health checks. Results come back as a report table.

## What To Do

```
Agent tool:
  subagent_type: health-checker
  model: sonnet
  description: "Health check"
  prompt: Run all health checks for the Zed AL Extension project and return the report table.
```

## After Results

- If **compilation fails**: fix it immediately (main agent)
- If **STOP issues exist**: report to user, ask whether to proceed
- If **deferred issues are actionable**: invoke `/fix-issues` to resolve them
- If **hookify rules are broken**: spawn infra-fixer agent to repair
- If **boundaries violated**: log to `.claude/data/issues.toml` via `/report-issue`
