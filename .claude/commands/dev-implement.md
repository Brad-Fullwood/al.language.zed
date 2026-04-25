---
description: Development Department entry point. Consumes a handoff.json (or arch-handoff.json) and implements tasks sequentially with TDD, mandatory two-stage self-review, and scope enforcement. Produces commits + handoff-progress.json.
allowed-tools: Read, Grep, Glob, Bash, Edit, Write, Agent
---

# /dev-implement

Dispatch the Development Department against a handoff file (review or
arch). Sequential, TDD-enforced, scope-locked, one commit per task.

## Usage

```
/dev-implement <path/to/handoff.json>            # full batch
/dev-implement <path> --max-tasks 3              # cap
/dev-implement <path> --priority P0,P1           # high-priority only
/dev-implement <path> --kinds bug,risk           # bugs/risks only (default)
/dev-implement <path> --kinds bug,risk,refactor  # include refactors
/dev-implement <path> --worktree                 # isolate in sibling worktree
/dev-implement <path> --dry-run                  # print plan, touch nothing
/dev-implement <path> --full-test-every 10       # workspace tests every 10th task
```

If no argument: use the most recent `arch-handoff.json` preferring
any at `.agentic/*/arch/arch-handoff.json`, falling back to
`.agentic/*/review/report/handoff.json`.

## Default behaviour choices

- **Kinds default to `bug,risk`.** Refactors require explicit opt-in
  per-run. This is intentional — mixing refactors into a fast fix
  cycle blurs the commit history.
- **Worktree default off.** Each task is a commit on the current
  branch. Use `--worktree` for risky batches.
- **Full-test-every default 5.** Every 5th task triggers workspace-
  wide tests.

## Phase 0 — Orchestrator setup

1. Resolve handoff path + validate schema.
2. Compute/inherit `run-id`.
3. Export `AL_DEV_RUN_ID=<run-id>`.
4. Create `.agentic/<run-id>/dev/{work-logs}`.
5. Initialise or read-and-continue `handoff-progress.json`.

## Phases 1–9 per task

Follow `.claude/skills/dev-implement/SKILL.md` verbatim. Each task
loops through 9 internal phases; tasks are processed sequentially.

## Post-run

Terse summary:

```
Dev run <run-id> complete.
  Attempted: N  Done: n  Blocked: n  Skipped: n  Bounced: n
  Progress: .agentic/<run-id>/dev/handoff-progress.json
  Next: /release-prep OR inspect .agentic/<run-id>/dev/work-logs/
```

## Guardrails

- `dev-path-scope.sh` PreToolUse hook (registered in settings.json)
  blocks any Edit/Write outside the current task's scope.
- The existing `stop-gate.sh` and `review-gate.sh` hooks still fire
  at session end — they check compile, clippy, fmt, and review-
  gate's own list.
- Every commit goes through CLAUDE.md's `git commit` pre-hook that
  runs `cargo fmt --all -- --check`.

## Anti-pattern hard stops

- No amend commits.
- No WIP commits.
- No cross-crate edits in a single task (must be split or use a
  multi-crate `owner_crate` set that the design explicitly called out).
- No new deps added by the implementer.
