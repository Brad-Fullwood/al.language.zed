# Schema: Architecture Design Document

`$schema_version: "1"`

One Markdown file per designed task at
`.agentic/<run-id>/arch/designs/<task-id>.md`.

## Frontmatter

```yaml
---
schema_version: 1
task_id: <finding-id>
run_id: <run-id>
status: approved | needs-revision | blocked
iterations: <int>
reviewed_by: arch-critic
owner_crate: ["al-core", "al_core::syntax"]
recommended_option: A | B | C | D
---
```

## Required body sections (in order)

Every design MUST have these headings. The arch-critic rejects designs
missing any of them.

```markdown
# <verb-first title>

## Problem

One paragraph. What's the finding, in the designer's own words (not
a copy-paste from the finding's `what` field).

## Current state

What the code does today. Mental model + relevant types + call graph
sketch. Cite files by path:line.

## What we know now (refactor only)

Verbatim from the finding's `what_we_know_now`. Only if kind=refactor.

## Options

### Option A: <name>
**Summary.** One paragraph.
**Mental model.** How a reader should think about this option.
**Files added.** `path/to/file.rs` — short purpose.
**Files modified.** `path:func` — short diff sketch.
**Files deleted.** `path` — short reason.
**Boundary impact.** Which crates' public API changes. None | al-core | …
**Test strategy.** What tests cover this option. Which existing tests move.
**Cost.** Effort estimate + expected review cost.
**Benefit.** What improves (and by how much, if measurable).
**Risks.**
- risk 1
- risk 2

### Option B: <name>
... same fields ...

(at least 2 options, up to 4)

## Recommended option

`Option X` — one-paragraph justification.

## Execution plan

Ordered numbered list. Each step is small enough to be a single commit.

1. <step> — files touched, test that must stay green.
2. <step>
   ...

Explicit checkpoints (e.g. "run `cargo test -p al-core` after step 3").

## Roll-back plan

How to undo this if it goes wrong after merge. One paragraph.
```

## arch-critic verdicts

The arch-critic agent reads this doc and produces one of:
- `approved` — design stands, proceed to Dev.
- `needs-revision` — specific objections inline in the doc under a new
  `## Critic feedback` section; designer gets at most one iteration.
- `blocked` — the underlying finding is misdiagnosed; bounce back to
  Review via `blocked-for-review.jsonl`.

## Validation

```bash
grep -q "^## Problem$" design.md
grep -q "^## Current state$" design.md
grep -q "^### Option A" design.md
grep -q "^### Option B" design.md
grep -q "^## Recommended option$" design.md
grep -q "^## Execution plan$" design.md
grep -q "^## Roll-back plan$" design.md
```

All must succeed. Full validator at
`.claude/hooks/schema-validate.sh arch-design <path>`.
