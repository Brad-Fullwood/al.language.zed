---
name: dev-implement
description: The phase-by-phase recipe the /dev-implement command body follows. Nine phases per task. TDD-enforced, scoped, two-stage self-reviewed. Sequential across tasks, one commit per task. Use this when invoking /dev-implement.
---

# Development Department Orchestration

## Preconditions

- User invoked `/dev-implement <handoff-or-arch-handoff.json>`.
- Handoff file validates against `handoff.md` or `arch-handoff.md`
  schema (run `.claude/hooks/schema-validate.sh handoff|arch-handoff`).
- Current branch is clean OR `--worktree` was passed.
- `AL_DEV_RUN_ID=<run-id>` exported so the path-scope hook can find
  the scope file.

## Phase 0 — Orchestrator setup

1. Parse flags: `--max-tasks N`, `--priority P0,P1`, `--kinds bug,risk`,
   `--dry-run`, `--worktree`, `--full-test-every N` (default 5).
2. Resolve run-id from handoff path (if handoff is at
   `.agentic/<id>/...` reuse that id; else compute new).
3. Create `.agentic/<run-id>/dev/{work-logs}`.
4. Initialise `handoff-progress.json` with totals all 0, filters as
   specified.
5. Initialise `commit-map.json` as empty object.
6. Read handoff.json; filter tasks per flags + status + blocked_by.
7. `AL_DEV_RUN_ID=<run-id>` in env.

## Main loop (sequential, one task at a time)

For each task in filter order (prio P0 → P3, within prio by severity,
within severity by original order in handoff):

### Task Phase 0: Task selection

- Skip if already `done` in current `handoff-progress.json` (resumable).
- Skip if `blocked_by` contains a task-id that's not `done`.
- Skip if filters exclude.
- Append an entry with `status: attempting`, `attempted_at: now`.
- Write `.agentic/<run-id>/dev/current-task-scope.txt` with one line
  per `owner_crate`: `crates/<crate>/`.
- Export `AL_DEV_TASK_ID=<task_id>`.

### Task Phase 1: Worktree (if `--worktree`)

Use `superpowers:using-git-worktrees`. Record worktree path.

### Task Phase 2: Understand

Dispatch `dev-understand` with task JSON + design path. Wait for
`understand.md` to exist. ≤ 90s timeout; on timeout mark blocked.

### Task Phase 3: Red

Dispatch `dev-test-writer`. Watch for verdicts:
- `already-resolved` → mark status `done` with notes; continue main loop.
- `finding-mismatch` → mark status `bounced-to-review`, bounce_reason.
- `needs-cross-crate-test` → mark status `deferred`; continue.
- Normal → `red.txt` exists; proceed.

### Task Phase 4: Green

Dispatch `dev-implementer`. Loop on-reply up to 3 attempts:
- Reply `done` (or similar) → proceed to Phase 5.
- Reply `blocked: ...` → mark status `blocked` with notes.
- Reply `bounced-to-review: ...` → mark status `bounced-to-review`.
- Reply `new-dep-needed: ...` → mark status `blocked` with notes.

Each attempt increments `attempts` counter in the progress entry.

### Task Phase 5: Refactor (optional)

Only if `task.kind == "refactor"` AND the design has a refactor step.
Dispatch `dev-refactorer`. Reply `done` → proceed; `blocked: ...` →
mark blocked.

### Task Phase 6a: Spec review

Dispatch `dev-spec-reviewer`. Reply:
- `approved` → proceed to 6b.
- `rejected: <feedback>` → go back to Phase 4 with the feedback; at
  most 2 Phase-4↔Phase-6a loops total. After that: blocked.

### Task Phase 6b: Quality review

Dispatch `dev-quality-reviewer`. Reply:
- `approved` → proceed to Phase 7.
- `rejected: <feedback>` → Phase 4 rework (counts toward the same
  2-loop cap as 6a).

### Task Phase 7: Verification

Run from orchestrator (NOT a subagent):
```
cargo check --workspace --exclude zed-al
cargo clippy --workspace --exclude zed-al -- -D warnings
cargo fmt --all -- --check
cargo test -p <owner_crate>
```

Every `--full-test-every` tasks, additionally:
```
cargo test --workspace --exclude zed-al
```

Any failure → mark task blocked, capture output. Unset
`AL_DEV_TASK_ID`, continue main loop.

### Task Phase 8: Commit

Still NOT a subagent — orchestrator runs git:
```
git add -A
git commit -m "<generated conventional message per commit-message-convention.md>"
```

The message is generated in-context from the task metadata. No
amend. If commit fails (e.g. pre-commit hook rejects), mark the
task blocked and DO NOT retry — investigate.

### Task Phase 9: Progress update

Update `handoff-progress.json` entry to final state:
- `status: done`, `finished_at: now`, `commit_sha`, `reviewers`,
  `work_log_dir`.

Update `commit-map.json`: `task_id → commit_sha`.

Unset `AL_DEV_TASK_ID`. Decrement max-tasks counter if set; exit
loop if hit zero.

## End of main loop

Print terse summary:

```
Dev run <run-id> complete.
  Attempted: N   Done: n   Blocked: n   Skipped: n   Deferred: n   Bounced: n
  Progress: .agentic/<run-id>/dev/handoff-progress.json
  Next step: /release-prep (if green) or inspect work-logs/
```

## Resumability

If the orchestrator is killed mid-run and restarted, re-invoking
`/dev-implement` on the SAME handoff will resume:
- Phase 0 of each task's loop iteration skips tasks already marked
  `done` in the current handoff-progress.json (which persists
  across interruptions).
- Tasks marked `blocked` or `bounced-to-review` in the same file are
  left alone (they already had their chance).
- Tasks with no entry yet are fresh.

## Dispatch discipline

- Everything is SEQUENTIAL in this department. Parallelism across
  tasks would require a distributed lock on owner_crate files;
  version 1 keeps it simple.
- Each sub-agent returns ≤600 tokens. Never read their full
  transcripts into the orchestrator.
- Phase 7 verification runs from the orchestrator directly, not a
  sub-agent, because it uses the orchestrator's own Bash tool and
  reads the output.

## Anti-pattern avoidance (reviewed every phase)

- Do NOT let the implementer run `git commit` — that's Phase 8's
  job.
- Do NOT let the implementer touch files outside owner_crate — the
  hook enforces it.
- Do NOT reuse a task's progress entry for a later run — each run-id
  gets its own `handoff-progress.json`.
- Do NOT fall back to "warn and continue" on scope-violations — hard
  block is the only way.
