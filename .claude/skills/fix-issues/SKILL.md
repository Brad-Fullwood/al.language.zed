---
name: fix-issues
description: Work on open issues — spawns a subagent to pick the highest priority fixable issue and resolve it
user_invocable: true
args: "[issue-id]"
---

# Fix Issues

Spawn the issue-fixer agent to resolve open issues from `.claude/data/issues.toml`.

## Arguments

- `issue-id` (optional): Specific issue to fix (e.g., "ISSUE-013"). If omitted, picks the highest priority actionable issue.

## What To Do

```
Agent tool:
  subagent_type: issue-fixer
  model: opus
  description: "Fix [ISSUE-ID or 'highest priority issue']"
  prompt: |
    Fix open issues in the Zed AL Extension project.
    [If specific ID]: Fix ISSUE-NNN specifically.
    [If no ID]: Pick the highest priority actionable open issue.
    Read .claude/data/issues.toml for the issue list.
    Read .claude/data/tasks.toml to check if deferred issues' blocking tasks are complete.
```

## After Results

1. If the agent **fixed issues**: review the changes, commit
2. If the agent **needs a design decision**: present the options to the user
3. If the agent **created new issues**: acknowledge them
4. If **no actionable issues exist**: report that and move on
