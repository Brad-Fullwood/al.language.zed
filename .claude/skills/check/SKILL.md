---
name: check
description: "Run architecture compliance checks (thin-adapter rules, clippy, dependency tree)"
context: fork
agent: guardian
allowed-tools: Bash, Read, Grep, Glob
---

Run the full architecture guardian check. Verify thin-adapter compliance, dependency direction, clippy warnings, and dead code. Report pass/fail for each category.
