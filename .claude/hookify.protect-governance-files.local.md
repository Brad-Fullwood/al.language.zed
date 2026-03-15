---
name: protect-governance-files
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: (\.claude/(rules|skills|agents|hookify\.).*|CLAUDE\.md)
---

**BLOCKED: Governance file edit requires user authorization**

You are attempting to edit a governance file (rules, skills, agents, hookify rules, or CLAUDE.md).

These files define project architecture, boundaries, and enforcement. They MUST NOT be
weakened, relaxed, or modified to accept code violations.

If you need to change a governance file:
1. Explain what you want to change and why
2. Wait for explicit user approval before proceeding

Architecture rules express the owner's intended design. If code violates a rule, fix the
CODE, not the rule.
