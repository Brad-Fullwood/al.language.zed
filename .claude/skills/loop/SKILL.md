---
name: agentic-loop
description: The phase-by-phase recipe the /loop command body follows. Runs Review → Arch (conditional) → Dev → Release (optional) cycles until convergence, max-cycles, or a halt condition. Use this when invoking /loop.
---

# Operations Overseer Orchestration

## Preconditions

- The Review/Arch/Dev/Release commands all exist and are usable
  independently.
- Working tree is clean (no uncommitted changes). The Overseer aborts
  if dirty — would interfere with Dev's commit-per-task protocol.

## Phase 0 — Orchestrator setup

1. Parse flags: `--max-cycles N` (default 3), `--convergence-threshold N`
   (default 3), `--priority P0,P1` (default all), `--auto-release`
   (default false), `--dry-run`.
2. Compute `run-id = <UTC-ISO>-<git-short-sha>`.
3. Create `.agentic/<run-id>/overseer/` directory.
4. Initialise `cycle-log.jsonl` (empty) and `convergence-report.md`
   (template).
5. Initialise `manifest.json` with overseer flags + cycle count 0.
6. Export `AL_OVERSEER_RUN_ID=<run-id>` so the log-append hook fires.

If `--dry-run`: print the planned cycles and exit. Do NOT invoke any
department.

## Main loop

```
cycle = 0
while True:
    cycle += 1
    record_cycle_start(cycle)

    # PHASE A — Review
    review_run_id = invoke_review(cycle)
    handoff = read .agentic/<review_run_id>/review/report/handoff.json

    # PHASE B — Arch (conditional)
    if any task in handoff has needs_design == true:
        arch_run_id = invoke_arch(handoff)
        next_handoff = .agentic/<arch_run_id>/arch/arch-handoff.json
    else:
        arch_run_id = None
        next_handoff = handoff

    # PHASE C — Dev
    dev_run_id = invoke_dev(next_handoff, --priority=...)

    # PHASE D — Release (optional)
    if --auto-release:
        release_run_id = invoke_release(dev_run_id)
        if release.audit.overall == "red":
            record_cycle_end(cycle, "halted-red-audit")
            break

    # PHASE E — Plan
    decision = invoke_overseer-planner(cycle)
    record_cycle_end(cycle, decision.outcome)
    if decision.next_action == "stop":
        break

write_convergence_report()
append_to docs/agentic-log.md
```

## Invocation details

### Review (Phase A)

The Overseer invokes `/review-all` as a subagent in this session
(via the Agent tool with subagent_type=`general-purpose` and a
prompt that describes the work, OR by spawning a fresh Claude Code
process — the simpler choice in v1).

For v1 simplicity: the Overseer's command body invokes `/review-all`
by spawning `claude -p '/review-all'` as a Bash subprocess. The
subprocess produces its own `.agentic/<sub-run-id>/review/`
artefacts. The Overseer captures the sub-run-id from the subprocess's
output (it always prints `Run id: <id>` in Phase 0).

Same approach for /arch-plan, /dev-implement, /release-prep —
spawned as `claude -p '<command> <args>'` subprocesses, each with
its own run-id, all artefacts under `.agentic/`.

### Spawning command (template)

```bash
claude -p "/review-all" --output-format json | jq -r .sub_run_id
```

If subprocess invocation isn't available, fallback: dispatch via
the Agent tool with a general-purpose subagent that runs the slash
command via the Skill tool. Less robust but functional.

### Drift detection

After Review (before Dev), the Overseer-planner can also be
consulted with the new handoff to flag drift early — a finding id
in the new handoff that matches a `done` task in any prior cycle.
If drift detected, halt before invoking Dev.

## Phase E — Planner invocation

Dispatch `overseer-planner` subagent. Prompt:

> Cycle <N> for run <run-id> just completed. Review/Arch/Dev/Release
> sub-run-ids are: <list>. Decide next_action and append to
> cycle-log.jsonl per your system prompt.

Read the planner's reply (next_action). Loop or break accordingly.

## End-of-loop summary

Write `.agentic/<run-id>/overseer/convergence-report.md`:

```markdown
# Convergence report — <run-id>

> Branch: <branch>   Started: <ISO>   Finished: <ISO>
> Cycles: <N>   Final stop reason: <reason>

## Cycle-by-cycle

| Cycle | review_id | arch_id | dev_id | release_id | findings_after | tasks_done | delta | outcome |
|---|---|---|---|---|---|---|---|---|
| 1   | ... | ... | ... | ... | ... | ... | -7 | completed |
| 2   | ... | -   | ... | -   | ... | ... | -3 | halted-converged |

## Findings shape

(Same structure as a Review FINAL.md executive summary — but for
the LATEST cycle only, not the first.)

## Tasks completed across all cycles

Numbered list with task ids, kinds, scopes.

## Recommendation for next session

If the loop halted-capped or halted-circuit-breaker, suggest next
human action. If halted-converged, confirm "branch is in good shape
for review."
```

Then append a one-paragraph summary to `docs/agentic-log.md` (committed
file). Use the log-append hook.

## Failure handling

- A Department subprocess returns non-zero → record cycle outcome
  as `aborted-error`, write the captured error to convergence
  report, halt loop.
- The Overseer-planner subagent fails → fallback to "stop with
  reason: planner-failure", do not loop further.
- SIGINT mid-cycle → state is in cycle-log.jsonl; restart of /loop
  resumes per resumability protocol.

## Dispatch discipline

- Each cycle is sequential (Review → Arch → Dev → Release → plan).
- Only ONE `overseer-planner` subagent runs per cycle.
- The Overseer NEVER reads finding/task/audit bodies into its own
  context — only path pointers and counts.
