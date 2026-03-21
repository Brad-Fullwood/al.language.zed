---
name: block-bash-file-bypass
enabled: true
event: bash
action: block
conditions:
  - field: command
    operator: regex_match
    pattern: (cat\s.*>|sed\s+-i|echo\s.*>|tee\s|dd\s.*of=).*(\.claude/(rules|skills|agents|hookify\.)|CLAUDE\.md)
---

**BLOCKED: Use Edit/Write tools instead of Bash for governance file modifications**

Bash file-writing commands (`cat >`, `sed -i`, `echo >`, `tee`) bypass hookify file-event rules when targeting governance files. Use the Edit or Write tools instead, which are subject to proper governance checks.

This rule protects: `.claude/rules/`, `.claude/skills/`, `.claude/agents/`, `.claude/hookify.*`, and `CLAUDE.md`.
Data files (`.claude/data/*`) are not affected — Bash edits to data files are permitted.
