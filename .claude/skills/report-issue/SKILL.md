---
name: report-issue
description: Log a bug or issue — creates a properly formatted entry in the issues tracker
user_invocable: true
args: "<description>"
---

# Report Issue

Log a bug, architectural problem, or improvement request to `.claude/data/issues.toml`.

## Arguments

- `description`: What's wrong (e.g., "al-explorer bypasses al-lsp for symbol search")

## Steps

### 1. Determine Next ID

Read `.claude/data/issues.toml` and find the highest `ISSUE-NNN` number. The new issue gets the next number.

### 2. Classify

Based on the description, determine:
- **type**: `architecture` (code boundary/design), `infra` (hook/skill/agent/config), or `deferred` (bug that needs a future task)
- **category**: `hook | rule | skill | agent | plan | constraint | idea`
- **severity**: `bug | drift | improvement`
- **priority**: `CRITICAL | HIGH | MEDIUM | LOW`
- **triage**: `STOP` (blocks work), `PARALLEL` (fix in background), `SCHEDULE` (fix later)

If unsure, ask the user.

### 3. Write Entry

Append to `.claude/data/issues.toml`:

```toml
[[issues]]
id = "ISSUE-NNN"
title = "Brief title"
type = "architecture"
reporter = "user"
date = "YYYY-MM-DD"
category = "constraint"
severity = "bug"
priority = "HIGH"
status = "open"
triage = "STOP"
description = """
Full description of the issue.
"""
```

### 4. Confirm

Tell the user:
- Issue ID assigned
- Priority and triage level
- Whether it blocks current work (STOP) or not
