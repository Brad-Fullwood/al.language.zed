---
name: ci
description: "Run infrastructure self-test: validates all hooks, file structure, and tool availability"
allowed-tools: Bash
---

Run the infrastructure self-test:

```bash
bash .claude/hooks/self-test.sh
```

If any tests fail, run `/fix-infra` with the failure description. Do not proceed with implementation work until CI passes.
