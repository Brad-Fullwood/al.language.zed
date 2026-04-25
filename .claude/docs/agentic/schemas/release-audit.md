# Schema: Release Audit (Release → Overseer)

`$schema_version: "1"`

Written at `.agentic/<run-id>/release/audit.md`. Human-readable markdown
with a structured frontmatter block so the Overseer can decide `green`
vs `red` programmatically.

## Frontmatter

```yaml
---
schema_version: 1
run_id: <run-id>
branch: <branch>
base_branch: <base>
head_sha_before: <sha>
head_sha_after: <sha>
tasks_in_batch: <int>
commits_in_batch: <int>
checks:
  compile: pass | fail
  clippy: pass | fail
  fmt: pass | fail
  tests: pass | fail
  scope_check: pass | fail
  dep_check: pass | fail
  commit_map_reachable: pass | fail
overall: green | red
generated_at: <ISO-8601>
---
```

## Required body sections

```markdown
# Release Audit

## Batch

- Branch: `<branch>` → `<base_branch>`
- Head SHA before: `<short>`
- Head SHA after:  `<short>`
- Commits in batch: <N>
- Tasks: <links to handoff-progress entries>

## Check results

### Compile
`cargo check --workspace --exclude zed-al` → pass | fail
<details — stderr on fail>

### Clippy
`cargo clippy --workspace --exclude zed-al -- -D warnings` → pass | fail

### Format
`cargo fmt --all -- --check` → pass | fail

### Tests
`cargo test --workspace --exclude zed-al` → pass | fail
<summary — N passed, M failed>

### Scope
Per-commit scope check (/scope-check equivalent) → pass | fail
<table: commit | files touched | expected scope | match>

### Dependency direction
/dep-check → pass | fail

### Commit map
Every `status: done` task in `dev/handoff-progress.json` has its
`commit_sha` present and reachable from `HEAD`.

## Overall

**GREEN** — proceed to PR
or
**RED** — abort; see failing checks above. Loop must halt.
```

## Validation

Overseer parses the frontmatter's `overall` field. `red` → halt the loop
and escalate. `green` → proceed.

## Validation tool

`.claude/hooks/schema-validate.sh release-audit <path>` parses the
frontmatter and ensures every `checks.*` field is `pass` or `fail` (no
nulls) and `overall` matches the computed AND of all checks.
