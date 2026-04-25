---
name: release-prep
description: The phase-by-phase recipe the /release-prep command body follows. Four phases — audit (gates the rest), changelog, PR description, optional PR creation via gh. Use this when invoking /release-prep.
---

# Release Department Orchestration

## Preconditions

- A Dev batch has completed (a `handoff-progress.json` exists with at
  least one `status: done` entry).
- Working tree is clean.
- Current branch contains the batch's commits.
- `gh` is authenticated IF `--pr` will be used (check
  `gh auth status`).

## Phase 0 — Orchestrator setup

1. Resolve input path: `--from-progress <path>`, or default to most
   recent `.agentic/*/dev/handoff-progress.json` by mtime.
2. Validate input with
   `.claude/hooks/schema-validate.sh handoff-progress`.
3. Determine run-id from the path.
4. Ensure `.agentic/<run-id>/release/` exists.
5. Parse flags: `--base <branch>` (default `dev`), `--pr`,
   `--squash`.

## Phase 1 — Audit

Dispatch `release-auditor`. Prompt:

> Run the audit for run `<run-id>`. Input:
> `.agentic/<run-id>/dev/handoff-progress.json`. Write audit.md per
> your system prompt.

Reply parsing:
- `overall: green` → proceed.
- `overall: red` → halt. Print the audit summary. Do NOT proceed to
  changelog / PR. Exit with a clear pointer at the failing checks.

## Phase 2 — Changelog

Dispatch `release-changelog-writer`. Prompt:

> Produce changelog entries for run `<run-id>`. Read handoff-progress,
> commit-map, and the source review's findings.jsonl if present.

Verify the entries file was written.

## Phase 3 — PR description

Dispatch `release-pr-writer`. Prompt:

> Produce PR description for run `<run-id>`.

Verify the pr-description.md was written.

## Phase 4 — Execution

If `--pr` and `gh auth status` is authenticated:

```
gh pr create \
  --base <base> \
  --title "<title derived from changelog>" \
  --body-file .agentic/<run-id>/release/pr-description.md
```

Title generation: use the top (highest severity) change plus a
trailing `(+N more)` if the batch has > 1 entry.

If not `--pr`:

Print:

```
Release prep complete.
  Audit: green.
  Branch: <branch> (<N> commits)
  Changelog: .agentic/<run-id>/release/changelog-entries.md
  PR desc:   .agentic/<run-id>/release/pr-description.md
  Next:      gh pr create --base <base> --body-file <pr-desc-path>
```

## Squash option

If `--squash` is passed, before Phase 4:

```
git reset --soft <head_sha_before>
git commit -m "<generated squash message>"
```

Squash commit message body = task-ids and titles list.

The audit was done on the pre-squash state; re-run clippy + tests
post-squash sanity check before proceeding to PR.

## Dispatch discipline

- Audit BLOCKS the rest. Never run changelog/PR on a red audit.
- Each phase returns ≤500 tokens. Orchestrator reads artefacts by
  path, not from replies.
- No subagent runs `gh` — the orchestrator runs it at Phase 4, and
  only on explicit `--pr`.

## Failure handling

- Audit red → print audit.md's `## Overall` section, exit.
- Changelog or PR writer fail → print the error, keep the audit
  artefact so the user can retry without re-auditing.
- `gh pr create` fails → print the error and the local paths so the
  user can open the PR manually.
