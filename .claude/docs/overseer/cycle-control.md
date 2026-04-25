# Overseer cycle control

## One cycle

```
1. /review-all  →  handoff.json (+ findings delta vs previous cycle)
2. if handoff.tasks contain needs_design:true:
      /arch-plan handoff.json  →  arch-handoff.json
   else:
      arch-handoff.json = handoff.json  (symbolic link or cp)
3. /dev-implement arch-handoff.json  →  handoff-progress.json
4. if --auto-release:
      /release-prep  →  audit.md
      if audit is red → halt-red-audit, stop loop
5. Record cycle outcome in cycle-log.jsonl.
6. Decide next_action.
```

## `next_action` decision

After a cycle completes, compute:

- `new_findings` = findings with `status: new` in the cycle's review run.
- `resolved_findings` = findings in previous cycle's handoff whose
  task_ids have `status: done` in THIS cycle's handoff-progress.json.

Then:

```
if cycle >= max_cycles:
    next_action = stop
    stop_reason = "halted-capped"
elif new_findings < convergence_threshold:
    next_action = stop
    stop_reason = "halted-converged"
elif cycle.tasks_completed == 0:
    next_action = stop
    stop_reason = "halted-circuit-breaker"
elif drift_detected:
    next_action = stop
    stop_reason = "halted-drift"
elif release_run_id is not None and audit == red:
    next_action = stop
    stop_reason = "halted-red-audit"
else:
    next_action = continue
```

## Drift detection

Drift = a task marked `done` in a PRIOR cycle's handoff-progress.json
appears as a finding (with its same id) in the CURRENT cycle's
handoff.

This happens when:
- The fix was incomplete (touched one spot, missed variants).
- The fix introduced a new instance of the same pattern elsewhere.
- The finding's `id` collision is spurious (unlikely but possible —
  the id hashes (reviewer|file|line|category|kind)).

All are reasons to halt and escalate to the human.

## Circuit breaker

If Dev completes **zero** tasks in a cycle (all blocked, all
bounced-to-review, or all skipped), the loop halts. Continued looping
would be pointless — nothing is progressing.

## Convergence

A healthy loop sees `new_findings` decreasing over cycles. When it
drops below `--convergence-threshold` (default 3), stop; we've got
diminishing returns.

## Max cycles

Hard cap (default 3). Even if findings are still appearing, stop and
let the human decide whether to continue.

## Idempotence

If `/loop` is killed and restarted with the same run-id, it reads
`cycle-log.jsonl`, finds the last entry:
- `finished == null` → rerun that cycle from the next incomplete
  phase (each department is safe to re-invoke on the same run-id).
- `finished != null and next_action == continue` → start cycle N+1.
- `next_action == stop` → already done; print the convergence
  report and exit.

Each department command is written to detect "this phase is already
complete" via `manifest.phases.<phase> == complete` and no-op in that
case.

## Never-push rule

The Overseer never pushes. Even with `--auto-release`, Release only
goes as far as `gh pr create` (local PR on the remote; not a push of
the branch by the Overseer itself). `gh pr create` pushes the branch
under the hood, but that's a single controlled push, not an
auto-push to `main`/`dev`.
