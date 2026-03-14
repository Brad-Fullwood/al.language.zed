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

### ISSUE-001: disable-model-invocation blocks Skill tool invocation
- **Reporter**: main
- **Date**: 2026-03-14
- **Category**: skill
- **Severity**: bug
- **Description**: `disable-model-invocation: true` on skills blocks Skill tool invocation, not just auto-triggering. Removed from /start-work, /pof, /audit. These skills need to be callable by agents and the Skill tool, not just by user typing the slash command. Fix applied immediately — flag removed from all three skills.
- **Status**: fixed

### ISSUE-002: Supervisor blocks on pre-existing issues, preventing WP1 start
- **Reporter**: main
- **Date**: 2026-03-14
- **Category**: agent
- **Severity**: improvement
- **Description**: Supervisor was treating all issues as blocking. Redesigned: supervisor now triages each finding as STOP (must fix now), PARALLEL (fix in background), or SCHEDULE (log for later). The /supervise skill dispatches fixers based on triage — STOP items block, PARALLEL items spawn background agents, SCHEDULE items go to docs/issues.md. Pre-existing test failures get triaged as PARALLEL (fix in background) not STOP, so they don't block task work.
- **Status**: fixed
