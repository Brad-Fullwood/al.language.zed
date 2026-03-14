---
name: audit
description: "Run milestone audit at end of Work Package"
argument-hint: "<WP number and name>"
disable-model-invocation: true
context: fork
agent: auditor
allowed-tools: Bash, Read, Grep, Glob, Edit
---

Run the full milestone audit checklist for Work Package: $ARGUMENTS

Check: tests pass, clippy clean, thin-adapter compliance, no dead code, PoF entries complete, progress.md updated. Report pass/fail for each category.
