---
name: report
description: "Log an issue, improvement idea, or observation about infrastructure (hooks, rules, skills, agents, plans). Any agent can invoke this when they notice something wrong or could be better."
argument-hint: "<description of the issue or idea>"
allowed-tools: Read, Edit
---

Log an issue to `docs/issues.md`.

## Steps

1. Read `docs/issues.md` to find the next issue number (ISSUE-NNN).
2. Determine category (hook/rule/skill/agent/plan/constraint/idea) and severity (bug/drift/improvement) from the description.
3. Append an entry under "## Open Issues":

```
### ISSUE-NNN: <brief title from $ARGUMENTS>
- **Reporter**: <your agent name or "main">
- **Date**: <today>
- **Category**: <category>
- **Severity**: <severity>
- **Description**: $ARGUMENTS
- **Status**: open
```

4. If this is urgent (bug severity), add a system message recommending `/fix-infra ISSUE-NNN`.
