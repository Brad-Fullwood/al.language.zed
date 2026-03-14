---
name: fix-infra
description: "Fix a broken hook, rule, skill, agent, or constraint. Reads docs/issues.md for context."
argument-hint: "[issue number or description of what's broken]"
disable-model-invocation: true
context: fork
agent: infra-fixer
allowed-tools: Bash, Read, Grep, Glob, Edit, Write
---

Fix the infrastructure issue: $ARGUMENTS

Read `docs/issues.md` for reported issues. If an issue number is given, fix that specific issue. If a description is given, find and fix the matching problem. Test hook fixes by piping JSON. Update cross-references if needed. Log the fix in `docs/issues.md`.
