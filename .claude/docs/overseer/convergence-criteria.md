# Convergence criteria

The Overseer decides when to stop the loop. Stop conditions in
priority order:

## 1. Halt — converged (best outcome)

New findings in the latest review are below `--convergence-threshold`.
Default threshold: **3**. Tune with `--convergence-threshold N`.

Stop reason: `halted-converged`.

This is the target. A healthy loop decreases `new_findings` each
cycle until the tail is small.

## 2. Halt — capped (soft limit)

`cycle_count >= --max-cycles`. Default: **3**.

Stop reason: `halted-capped`.

Protect against a loop that's making slow progress. If the findings
list isn't converging in 3 cycles, there's probably a bigger
problem (design-level, not fix-level) and a human should weigh in.

## 3. Halt — circuit breaker (bad signal)

Dev completed 0 tasks. Every task blocked, bounced, or skipped.

Stop reason: `halted-circuit-breaker`.

Either the handoff has tasks Dev can't tackle (all need design and
design failed; all have unfinished `blocked_by`) or the implementer
can't make progress (stuck in Phase 4 loops). Human escalation.

## 4. Halt — drift (very bad signal)

A task id that was `done` in a previous cycle appears again in the
new handoff with the same id.

Stop reason: `halted-drift`.

Either the fix was incomplete or the finding id is coincidental. The
Overseer doesn't try to distinguish — it halts and points the human
at the specific task id so they can decide.

## 5. Halt — red audit (with --auto-release)

Release audit produced `overall: red`.

Stop reason: `halted-red-audit`.

Don't keep looping when the most recent batch produced a red audit —
the remediation is human.

## Outcome logging

Each cycle's entry in `cycle-log.jsonl` has an `outcome` field.
Possible outcomes:
- `completed` — cycle ran fully, next_action is continue.
- One of the `halted-*` values above.
- `aborted-error` — uncaught error.

## Convergence report

Written at `.agentic/<run-id>/overseer/convergence-report.md` at loop
end. Summary of all cycles, delta per cycle, final state, total
tasks done/blocked/bounced, total commits, total time. Human-readable
but short (≤ 200 lines).
