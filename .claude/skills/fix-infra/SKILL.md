---
name: fix-infra
description: Spawn infra-fixer agent to repair broken hookify rules, agent definitions, skills, or config files
user_invocable: true
---

# Fix Infrastructure

Spawn the infra-fixer agent to repair broken agent infrastructure.

## When To Use

- After `/ci` reports infrastructure failures
- When hookify rules are malformed or not triggering
- When skill files have incorrect format
- When config files (constraints.toml, deferred-issues.toml) don't parse

## What To Do

Launch the infra-fixer agent:

```
Agent tool:
  subagent_type: infra-fixer
  model: sonnet
  description: "Fix infrastructure: [ISSUE]"
  prompt: |
    Fix the following infrastructure issue in the Zed AL Extension project:

    Issue: [DESCRIBE_ISSUE]

    The .claude/ directory contains:
    - hookify.*.local.md — enforcement rules (YAML frontmatter + markdown)
    - agents/ — agent definitions (markdown)
    - skills/ — skill definitions (SKILL.md with YAML frontmatter)
    - rules/ — project context rules (markdown)
    - constraints.toml — constraint index
    - deferred-issues.toml — known acceptable failures
    - settings.json — permissions

    Fix the broken files. Do NOT modify working files.
    Do NOT create scripts or hooks — this project uses zero custom executable code.

    After fixing, verify by reading the files back.
```

Replace `[DESCRIBE_ISSUE]` with the specific problem from `/ci` output.

## Scope

The infra-fixer agent handles ONLY `.claude/` and `docs/` infrastructure files. It does NOT fix application code — that's your job.
