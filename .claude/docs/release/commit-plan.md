# Release commit plan

The Release Department inherits a series of per-task commits produced
by Dev. It packages them into a shippable unit without rewriting their
content.

## Default mode: preserve per-task history

- Branch has N commits, one per task.
- Release Department adds any docs/changelog commits as separate
  commits AT THE TIP of the branch, then opens a PR against `dev`.
- Per-task commits are NOT squashed by default — they're useful for
  git-bisect and blame. The PR description summarises them as a
  list.

## `--squash` mode

- Release collapses all per-task commits into ONE commit.
- The commit message body enumerates the task ids and titles.
- Use when the batch is a cohesive feature (e.g., "one refactor
  pass") and the individual commits don't tell a story.

## Never-touch

- Commits older than the last `/dev-implement` run. Release looks
  only at commits made during the current run (identified by
  `head_sha_at_start` in `handoff-progress.json`).
- Commits that don't appear in `commit-map.json`. If a human made a
  manual commit mid-batch, Release flags it and asks how to proceed
  (does not silently include it).

## Changelog commit

If project has `CHANGELOG.md` at root, Release stages a single
`docs(changelog): <run-id>` commit adding entries from
`.agentic/<run-id>/release/changelog-entries.md`.

If no CHANGELOG.md, no changelog commit is made; the entries live
only in the run dir and are referenced from the PR description.

## Audit commit

None. The audit artefact lives in `.agentic/` (gitignored). It's
evidence, not history.
