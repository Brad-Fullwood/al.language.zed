---
name: adversarial
description: "Spawn the adversarial tester to actively find ways to break recent code changes"
argument-hint: "[focus area or file path]"
context: fork
agent: adversarial
allowed-tools: Bash, Read, Grep, Glob, Edit, Write
---

Run the adversarial tester against recent changes. Focus: $ARGUMENTS

The adversarial agent works in an isolated worktree. It reads the git diff, identifies edge cases and failure modes, writes tests designed to BREAK the code, runs them, and reports findings. Tests that can't fail are deleted.
