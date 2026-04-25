# Schema: Overseer Cycle Log

`$schema_version: "1"`

JSONL at `.agentic/<run-id>/overseer/cycle-log.jsonl`. One line per
completed (or aborted) cycle. Used for resumability — if the Overseer
is killed mid-cycle and restarted, it reads this file, determines the
highest cycle number, and starts the next cycle.

## Entry schema

```jsonc
{
  "$schema_version": "1",
  "cycle": number,                          // 1-indexed
  "started":  "ISO-8601",
  "finished": "ISO-8601 | null",            // null if aborted mid-cycle
  "outcome":  "completed | halted-converged | halted-capped | halted-circuit-breaker | halted-drift | halted-red-audit | aborted-error",
  "review_run_id":  "string",                // nested run-id of the /review-all invocation
  "arch_run_id":    "string | null",         // null if skipped
  "dev_run_id":     "string | null",
  "release_run_id": "string | null",
  "findings_before": {
    "critical": number, "high": number, "medium": number, "low": number, "nit": number, "speculative": number
  },
  "findings_after": {
    "critical": number, "high": number, "medium": number, "low": number, "nit": number, "speculative": number
  },
  "new_findings":    number,
  "resolved_findings": number,
  "tasks_completed": number,
  "tasks_blocked":   number,
  "tasks_bounced":   number,
  "regression_detected": boolean,
  "regression_task_ids": ["string", ...],
  "convergence_delta": number,               // new_findings - resolved_findings; negative = improving
  "next_action": "continue | stop",
  "stop_reason": "string | null"             // populated when next_action == stop
}
```

## Outcome values

- `completed` — the cycle ran all planned phases successfully; the
  Overseer decides based on deltas whether to run another cycle.
- `halted-converged` — `new_findings < convergence_threshold`.
- `halted-capped` — `cycle >= max_cycles`.
- `halted-circuit-breaker` — Dev completed zero tasks this cycle.
- `halted-drift` — a task marked `done` in a prior cycle reappeared in
  the current cycle's findings with the same id.
- `halted-red-audit` — Release audit failed.
- `aborted-error` — an unhandled error during a phase. `stop_reason`
  carries the error.

## Resumability protocol

On `/loop` restart:
1. Read `cycle-log.jsonl` if it exists for this run-id.
2. Find the last entry.
3. If `finished == null` → that cycle was interrupted; re-run it
   idempotently (each department is safe to re-invoke on the same
   run-id; they'll no-op if their phase is already `complete` in
   `manifest.json`).
4. If `finished != null` and `next_action == continue` → start cycle
   `last.cycle + 1`.
5. If `next_action == stop` → the loop is already done; print the
   convergence-report and exit.

## Validation

`jq -c . < cycle-log.jsonl > /dev/null` must pass. Full validator at
`.claude/hooks/schema-validate.sh cycle-log <path>`.
