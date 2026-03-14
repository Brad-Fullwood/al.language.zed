---
name: report
description: Generate a progress report showing completed tasks, current status, and what's next
user_invocable: true
---

# Progress Report

Generate a concise progress report for the Zed AL Extension project.

## Steps

### 1. Read Current State
- Read `docs/progress.md` for task completion status
- Read `docs/plan.md` for total task count and WP structure
- Read `docs/proof_of_functionality.toml` for evidence log entries

### 2. Calculate Metrics
- Total tasks vs completed tasks
- Current WP and its completion percentage
- Next task to work on
- Number of PoF entries

### 3. Report Format

```
## Progress Report — [DATE]

### Summary
- **Completed**: X/Y tasks (Z%)
- **Current WP**: WPX — [NAME] (A/B tasks done)
- **Next task**: TXXX — [NAME]
- **PoF entries**: N

### Recently Completed
- TXXX: [NAME] — [DATE]
- TXXX: [NAME] — [DATE]

### Blocked / Deferred
- TXXX: [REASON]

### Next Steps
1. TXXX — [DESCRIPTION]
2. TXXX — [DESCRIPTION]
```

Keep it concise. Focus on actionable information.
