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
- **Description**: Supervisor correctly identifies pre-existing issues (8 test failures in data_driven, 3 clippy errors in al-symbols, PoF placeholder entry) as blocking. These are application bugs predating the infrastructure work, not regressions. The supervisor/start-work flow needs guidance on distinguishing pre-existing baseline state from regressions — currently it blocks on everything, which would prevent WP1 from starting even though those failures exist in the committed baseline. Consider: (a) establishing a baseline test count at session start and only blocking on regressions, or (b) having the supervisor note known pre-existing failures separately from new regressions.
- **Status**: open
