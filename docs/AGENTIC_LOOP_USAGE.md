# Agentic Loop — Usage

The agentic loop is a long-running, self-supervising review→fix→re-review
cycle that runs **on your local machine**. Once started, it:

1. Reviews the codebase exhaustively (Review Department).
2. Designs any cross-crate refactors (Architecture Department).
3. Implements fixes one task at a time with TDD + scoped self-review (Development Department).
4. Optionally packages a release (Release Department).
5. Loops back to Review and repeats until zero findings remain.

It survives rate limits, daemon crashes, your laptop sleeping briefly,
and your terminal closing. It does **not** survive the machine powering
off (no work loss; just stops).

---

## Quick reference

| Action | Command |
|---|---|
| **Start** | `/loop` from inside Claude Code in this repo |
| **Status (one-shot)** | `bash .agentic/<run-id>/overseer/status.sh` |
| **Live tail** | `tail -f .agentic/<run-id>/overseer/STATUS.md` |
| **STOP everything** | `touch .agentic/<run-id>/overseer/STOP` |
| **Recover (if all daemons died)** | `bash .agentic/<run-id>/overseer/recover.sh` |
| **List runs** | `ls .agentic/` |

`<run-id>` is `<UTC-ISO>-<short-sha>`, e.g. `20260425T030143Z-30e9e69`.
Find your current run-id: `cat .agentic/.current-run-id`.

---

## How to STOP

**Graceful** (preferred — daemons exit cleanly within 60s):

```bash
touch .agentic/$(cat .agentic/.current-run-id)/overseer/STOP
```

**Hard kill** (if graceful didn't work in ~2 min):

```bash
RUN_ID=$(cat .agentic/.current-run-id)
for d in meta-watchdog watchdog reporter; do
  pid=$(cat .agentic/$RUN_ID/overseer/$d.pid 2>/dev/null)
  [[ -n "$pid" ]] && kill -9 "$pid" 2>/dev/null
done
# Plus any phase subprocess
pkill -9 -f "claude.*--print"
```

**Disable the cloud heartbeat** (if you set one up):

In Claude Code, run `/schedule update agentic-loop-heartbeat enabled false`
or visit https://claude.ai/code/routines.

---

## What it produces

Everything under `.agentic/<run-id>/` (gitignored). The interesting files:

| Path | What it is |
|---|---|
| `review/report/FINAL.md` | Human-readable review report (ultrareview-shaped) |
| `review/report/handoff.json` | Machine-readable task list (input to Arch / Dev) |
| `review/report/findings.jsonl` | All verified findings, one per line |
| `arch/designs/<task-id>.md` | Per-task implementation designs |
| `arch/arch-handoff.json` | Augmented handoff (what Dev consumes) |
| `dev/handoff-progress.json` | Per-task status (done/blocked/etc.) |
| `dev/work-logs/<task-id>/` | Per-task work logs (test output, reviews) |
| `release/audit.md` | Release-readiness audit |
| `release/pr-description.md` | PR body for `gh pr create` |
| `overseer/STATUS.md` | **Live status — `tail -f` this** |
| `overseer/state.json` | Current cycle/phase machine state |
| `overseer/cycle-log.jsonl` | One line per completed cycle |
| `docs/agentic-log.md` | **Committed** persistent log (in repo) |

Commits made by Dev land on the current branch with message format:
`<type>(<scope>): <subject>` plus `Finding:`, `Run:`, `Task:` trailers.

---

## How it survives failures

| Failure mode | What happens |
|---|---|
| Subprocess crashes | Watchdog detects in ≤30s, respawns |
| Subprocess hangs >20 min with no log activity | Watchdog SIGKILLs the process group, respawns |
| Watchdog crashes | Meta-watchdog respawns it within 60s |
| Reporter crashes | Meta-watchdog respawns it within 60s |
| Meta-watchdog crashes | Run `recover.sh` manually |
| Rate limit hit (`hit your limit · resets…`) | Sleeps 60 min, retries, no failure-count cost |
| Subprocess dies in <60s 3 times in a row | Treated as transient; sleeps 60 min |
| Phase fails 100 times non-rate-limit | 30-min cooldown, counter resets, **never halts** |
| Convergence (zero tasks in handoff) | Clean stop with `halted-converged-zero-findings` |
| `STOP` file appears | Clean stop on next 60s tick |

The loop has **no cycle cap by default**. It runs until convergence or
your STOP. Adjust `max_cycles` in `overseer/state.json` if you want a cap.

---

## Tuning knobs (in `overseer/state.json`)

```jsonc
{
  "max_cycles": 99999,                 // change to cap
  "convergence_threshold": 0,          // change to "stop when fewer than N new"
  "max_phase_attempts": 100,           // before triggering 30-min cooldown
  "auto_release": false,               // true = also run /release-prep each cycle
  "stuck_threshold_seconds": 1200      // 20 min — log silence = stuck
}
```

Edit between cycles only, not mid-phase.

---

## Things to avoid

- **Don't run two `/loop`s in parallel against the same branch.** They'll
  fight over commits.
- **Don't push branches Dev is working on** until you've stopped the loop
  (otherwise commits will collide on push).
- **Don't `git reset --hard`** on the branch while Dev is running — it
  corrupts `commit-map.json`.
- **Don't delete `.agentic/<run-id>/` while running.** Wait until STOP.
- **Don't manually edit `state.json` while a phase subprocess is alive.**
  Let it advance via the watchdog.

---

## Where the work is

Look at:

1. **`docs/agentic-log.md`** (committed) — high-level history of every
   loop run.
2. **`.agentic/<run-id>/review/report/FINAL.md`** — what the review
   found this cycle.
3. **Your git log** — the actual fixes Dev committed.
4. **`.agentic/<run-id>/overseer/STATUS.md`** — live snapshot tape.

---

## "It's been running for hours and there are no commits"

That can mean one of:

- **Review is still running.** Big codebases produce hundreds of findings;
  Phase 4 validation alone can take an hour. Check `STATUS.md`.
- **Arch is in progress.** Designs come before fixes. Look at
  `.agentic/<run-id>/arch/designs/`.
- **Rate limit is in effect.** Look for a `rate-limit` GOALPOST in
  `STATUS.md`, or the watchdog log.
- **All findings need design and Arch is bottlenecked.** Check
  `arch-handoff.json` task counts.
- **Real bug.** Inspect `overseer/watchdog.log` and the latest
  `subprocess-logs/cycle*-attempt*.log`.

---

## Resuming after a STOP

The state on disk is the source of truth. To resume:

```bash
RUN_ID=$(cat .agentic/.current-run-id)
rm .agentic/$RUN_ID/overseer/STOP
bash .agentic/$RUN_ID/overseer/recover.sh
```

The watchdog reads `state.json` and continues from whatever phase it
was on. Existing artefacts are reused (resume-aware prompts).

---

## When in doubt

`bash .agentic/$(cat .agentic/.current-run-id)/overseer/status.sh`
