# Routine: `review-all-nightly`

This file describes the intended scheduled Routine spec. Routines are
registered server-side via the `/schedule` skill or the web UI at
claude.ai/routines — not via a local file. Use the content below when
registering the routine.

## Spec

| Field | Value |
|---|---|
| Name | `review-all-nightly` |
| Trigger | cron `0 3 * * *` (03:00 local) |
| Target | `dev` branch of this repository |
| Command | `/review-all` (full-tree sweep) |
| Description | Nightly exhaustive review of `dev`. Output lands in `.agentic/scheduled/<YYYY-MM-DD>/<run-id>/`. |

## Why full, not incremental

We deliberately run the full sweep nightly — not `--diff` or `--since`.
Old bugs in unchanged code persist forever; an incremental scheduler
would never re-find them. Token cost is paid once a night and accepted.
Efficiency comes from the orchestrator itself (worker model = haiku,
opt-in refactor + toolkit reinforcements, larger validator batches),
not from skipping coverage.

## Registration (do this once)

```
/schedule create review-all-nightly
```

Then supply:

- **cron**: `0 3 * * *`
- **command**: `/review-all`
- **working dir**: this repository root
- **env**: none (the command computes its own run-id)
- **notification**: on completion (so Brad sees summary in the morning)

## Disable

```
/schedule disable review-all-nightly
```

## Output location

The orchestrator writes to `.agentic/<run-id>/`. For scheduled runs, the
run-id prefix is `<YYYYMMDD>-scheduled-<short-sha>` so they are easy to
distinguish from interactive runs.

## Keeping history

`.agentic/scheduled/*` is gitignored. If you want a persistent trail of
scheduled-run summaries, the Overseer's `docs/agentic-log.md` already
captures that. For individual scheduled-run details, grep
`.agentic/*/review/report/FINAL.md` directly.
