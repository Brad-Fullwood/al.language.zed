# Development Department — task execution flow

For every task accepted by `/dev-implement`, the department goes through
nine phases. Detailed contract for each phase:

## Phase 0 — Task selection

Orchestrator reads `handoff-or-arch-handoff.json`, filters by:
- `status != "resolved"` (already done).
- `design_status != "blocked"` (arch rejected it).
- Respect filters: `--max-tasks`, `--priority`, `--kinds`.
- No unfinished `blocked_by` dependencies.
- Skip any task whose `task_id` already has a `status: done` entry in
  `handoff-progress.json` for this run-id (resumability).

Pick one. Record in progress JSON as `attempted_at`.

## Phase 1 — Worktree isolation (optional, default off)

Default: work directly on current branch. Each task is one commit.

With `--worktree`: use `superpowers:using-git-worktrees` to create a
sibling directory. Work there; at Phase 8 merge back (default:
`git merge --squash`).

With `--dry-run`: skip everything from here on; print the plan.

## Phase 2 — Understand

Dispatch `dev-understand` subagent (sonnet, read-only).

Input:
- Task JSON.
- Design doc if present (`arch-handoff`'s `design_path`).
- The finding itself (from `findings.jsonl` pointer).

Output: `.agentic/<run-id>/dev/work-logs/<task-id>/understand.md`
with:
- Files to touch (exhaustive; implementer works from this).
- Interfaces in play.
- Blast radius (what breaks if this is wrong).
- Tests that must stay green (specific `cargo test -p X --test Y`
  commands).

Reply ≤ 400 tokens.

## Phase 3 — Red (failing test)

Dispatch `dev-test-writer` subagent (sonnet; Write scoped to `tests/`
or `#[cfg(test)]` block of owner crate; Bash scoped to cargo test).

Input:
- understand.md.
- Acceptance criteria from the task.
- Reproduction from the finding.

Output:
- The failing test (written into the appropriate tests dir or a
  `#[cfg(test)] mod tests` block).
- `.agentic/<run-id>/dev/work-logs/<task-id>/red.txt` — captured
  failure output.

**Important exit criterion:** the test MUST fail in a way consistent
with the finding's expected failure. If it passes or fails unexpectedly,
the task is marked `bounced-to-review` and control returns to Phase 0.

Reply ≤ 400 tokens.

## Phase 4 — Green (minimum implementation)

Dispatch `dev-implementer` subagent (opus; Edit/Write scoped to
`owner_crate(s)` via the `dev-path-scope.sh` PreToolUse hook).

Input:
- understand.md.
- red.txt.
- Design doc if present.
- The task.

Output:
- Source edits in the owner crate(s) to make the failing test pass.
- `.agentic/<run-id>/dev/work-logs/<task-id>/green.txt` — cargo output
  showing green.

Loop: implementer runs `cargo test -p <owner_crate>` after each
iteration. At most 3 cycles. If stuck after 3, task is marked
`blocked` with the diagnostics captured.

Forbidden:
- Touching files outside `owner_crate` (hook blocks it).
- Adding new dependencies without explicit approval.
- Public API changes beyond what the design called out.
- WIP commits.

Reply ≤ 600 tokens.

## Phase 5 — Refactor (optional)

If the design's execution plan has a refactor step, dispatch
`dev-refactorer` subagent (sonnet). Must preserve behaviour; tests
must stay green. Usually only runs for `kind: refactor` tasks with
explicit rename/extract/inline instructions in the design.

Skip if `kind != refactor` or the design has no refactor step.

## Phase 6 — Self-review (mandatory two-stage)

### Stage 6a: spec review

Dispatch `dev-spec-reviewer` subagent (sonnet, read-only).

Question: does the implementation satisfy `acceptance_criteria` AND
resolve the finding's `reproduction`? Read the code, read the tests,
re-run the test, check.

Verdicts: `approved` | `rejected: <specific feedback>`.
Output: `.agentic/<run-id>/dev/work-logs/<task-id>/spec-review.md`.

On `rejected`, control returns to Phase 4 with the feedback as input.
At most 2 Phase-4↔Phase-6a loops. After that: mark `blocked`.

### Stage 6b: quality review

Dispatch `dev-quality-reviewer` subagent (opus, read-only). Thin
wrapper around the existing `al-reviewer` agent.

Checklist:
- No new `.unwrap()` in non-test code (enforced by existing review-gate
  as well, but we check before commit).
- No hardcoded AL values (enforced, but double-check).
- Dep direction preserved.
- CLAUDE.md hard constraints preserved.
- Error handling idiomatic.
- Tests exist for both the success path and at least one failure path.

Verdicts: `approved` | `rejected: <specific feedback>`.
Output: `.agentic/<run-id>/dev/work-logs/<task-id>/quality-review.md`.

On `rejected`, control returns to Phase 4.

## Phase 7 — Verification

Orchestrator runs the scoped pre-commit pipeline:

```
cargo check --workspace --exclude zed-al
cargo clippy --workspace --exclude zed-al -- -D warnings
cargo fmt --all -- --check
cargo test -p <owner_crate>
```

Every N tasks (default N=5, configurable), also run the full test
suite: `cargo test --workspace --exclude zed-al`. This catches
cross-crate regressions earlier than end-of-batch.

Any failure → task marked `blocked`, capture output.

## Phase 8 — Commit

Commit message generated by the implementer, conforming to:

```
<type>(<scope>): <short>

<body — 1-2 sentences on WHY>

Finding: <finding-id>
Run: <run-id>
Task: <task-id>
```

Type:
- bug → `fix`
- risk → `fix`
- refactor → `refactor`
- gap → `feat`
- doc → `docs`

Scope is the owner crate (or `workspace` if multi-crate).

No amending on rework: if the task iterated through Phase 4 multiple
times, only the final state is committed. Prior attempts stay in
work-logs/ for audit.

With `--worktree`: the commit goes into the worktree; main checkout
remains clean.

## Phase 9 — Progress update

Append to `handoff-progress.json`:

```jsonc
{
  "task_id": "...",
  "attempted_at": "...",
  "finished_at": "...",
  "status": "done",
  "commit_sha": "<sha>",
  "attempts": N,
  "reviewers": {"spec": "approved", "quality": "approved"},
  "work_log_dir": ".agentic/<run-id>/dev/work-logs/<task-id>/",
  "notes": null,
  "bounce_reason": null
}
```

Also update `commit-map.json`:
```jsonc
{ "<task-id>": "<commit-sha>" }
```

Control returns to Phase 0 for next task.

## Status values (terminal)

- `done` — everything green, commit recorded.
- `blocked` — after max iterations, still red; `notes` explains what's
  stuck.
- `skipped` — filter excluded; not attempted.
- `deferred` — depends on a blocked task; retry next loop cycle.
- `bounced-to-review` — Red test unexpectedly passed, or quality
  reviewer determined misdiagnosis. Returns to Review as regressed.
