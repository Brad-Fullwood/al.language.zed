---
name: ci
description: Check infrastructure health — verify hookify rules, compilation, tests, and project config are valid
user_invocable: true
---

# CI — Infrastructure Health Check

Verify the project's agent infrastructure and code health.

## Checks to Perform

### 1. Hookify Rules Valid
Read all `.claude/hookify.*.local.md` files. For each:
- Verify YAML frontmatter parses (name, enabled, event fields present)
- Verify event type is valid (bash, file, stop, prompt, all)
- Verify action is valid (warn, block) or absent (defaults to warn)
- Report any malformed rules

### 2. Compilation
```bash
cargo check --workspace --exclude zed-al 2>&1 | tail -20
```

### 3. Tests
```bash
cargo test --workspace --exclude zed-al 2>&1 | tail -30
```

### 4. Clippy
```bash
cargo clippy --workspace --exclude zed-al 2>&1 | tail -20
```

### 5. Config Files Valid
- Read `.claude/constraints.toml` — verify it parses
- Read `.claude/deferred-issues.toml` — verify it parses
- Read `docs/proof_of_functionality.toml` — verify it parses

## Output

Report results as a table:

| Check | Status | Details |
|-------|--------|---------|
| Hookify rules | OK/FAIL | N rules, M malformed |
| Compilation | OK/FAIL | error count |
| Tests | OK/FAIL | passed/failed/total |
| Clippy | OK/FAIL | warning count |
| Config files | OK/FAIL | which files are broken |

If any check fails, suggest running `/fix-infra` for infrastructure issues or fixing code directly for compilation/test failures.
