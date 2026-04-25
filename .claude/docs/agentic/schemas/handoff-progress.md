# Schema: Development Progress (Dev → Release & next-cycle Review)

`$schema_version: "1"`

Written at `.agentic/<run-id>/dev/handoff-progress.json`. Appended to
after every task; the Dev session may be killed and resumed and the file
is always the source of truth for what's been done.

## Structure

```jsonc
{
  "$schema_version": "1",
  "run_id": "string",
  "branch": "string",
  "base_branch": "string",
  "head_sha_at_start": "string",
  "head_sha_now":      "string",
  "started_at": "ISO-8601",
  "updated_at": "ISO-8601",
  "filters_applied": {
    "max_tasks": number | null,
    "priority": ["P0", "P1"] | null,
    "kinds": ["bug", "risk"] | null,
    "dry_run": boolean
  },
  "entries": [
    {
      "task_id": "string",
      "attempted_at": "ISO-8601",
      "finished_at": "ISO-8601 | null",
      "status": "done | blocked | skipped | deferred | bounced-to-review",
      "commit_sha": "string | null",       // present iff status == "done"
      "attempts": number,                    // implementer iteration count
      "reviewers": {
        "spec": "approved | rejected | null",
        "quality": "approved | rejected | null"
      },
      "work_log_dir": "string",              // path relative to repo root
      "notes": "string | null",
      "bounce_reason": "string | null"       // only if bounced-to-review
    }
  ],
  "totals": {
    "attempted": number,
    "done": number,
    "blocked": number,
    "skipped": number,
    "deferred": number,
    "bounced_to_review": number
  }
}
```

## Status semantics

- **done** — task complete, tests green, both reviewers approved,
  commit exists.
- **blocked** — implementer exceeded attempt cap (default 3) without
  achieving green tests; `notes` explains what's stuck.
- **skipped** — filters excluded this task; does not consume an attempt.
- **deferred** — task depends on a blocked task; will be retried next
  loop cycle.
- **bounced-to-review** — the written failing test unexpectedly passed,
  OR the quality reviewer determined the finding is misdiagnosed. The
  task id goes back as a regressed-from-dev finding in the next Review
  cycle.

## Contract with next-cycle Review

When Review reads this file at the start of a new cycle, it:
- Marks the corresponding finding as `status: resolved` for every
  `status: done` entry, even if the underlying code pattern still
  superficially matches.
- Ignores `status: bounced-to-review` entries as already-handled
  signals (they'll come back through normal finding channels).
- Does not re-promote `status: blocked` tasks for Dev in the new cycle
  without human intervention (to avoid loop-of-blockage).

## Validation

`jq '.entries | type == "array" and (length | . >= 0)' handoff-progress.json`
must return `true`. Full validator at
`.claude/hooks/schema-validate.sh handoff-progress`.
