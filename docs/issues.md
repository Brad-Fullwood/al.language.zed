# Infrastructure Issues Log

Agents, subagents, and the supervisor report issues here. The main agent or infra-fixer reviews periodically.

## How to Report

Append an entry with this format:
```
### ISSUE-NNN: Brief title
- **Reporter**: agent name or "user"
- **Date**: YYYY-MM-DD
- **Category**: hook | rule | skill | agent | plan | constraint | idea
- **Severity**: bug | drift | improvement
- **Description**: What's wrong or what could be better
- **Status**: open | fixed | wontfix
```

## How to Fix

Run `/fix-infra` with the issue number, or spawn the infra-fixer agent directly. The fixer reads this file, fixes the problem, and updates the status.

---

## Open Issues

_(none yet — issues will be logged here as agents encounter them)_
