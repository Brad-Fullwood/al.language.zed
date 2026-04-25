---
name: arch-plan
description: The phase-by-phase recipe the /arch-plan command body follows. Four phases — filter needs_design tasks, gather context (parallel), design + critique (sequential per task with at most one revision), then emit arch-handoff.json. Use this when invoking /arch-plan.
---

# Architecture Department Orchestration

## Preconditions

- The user invoked `/arch-plan <path/to/handoff.json>` with a valid
  handoff file.
- The handoff validates against
  `.claude/docs/agentic/schemas/handoff.md`.
- The arch directory exists at `.agentic/<run-id>/arch/`. If missing,
  create `.agentic/<run-id>/arch/{designs,scratch}`.

If the handoff lives outside `.agentic/` (e.g. user passed a hand-
curated file), still compute a `run-id` the same way `/review-all`
does and create `.agentic/<run-id>/arch/` so all arch artefacts live
together.

## Phase 0 — Filter

Read handoff.json. Collect tasks where `needs_design == true` into a
work list. If the list is empty, write `arch-handoff.json` as a
straight copy of `handoff.json` with `design_status: "not-needed"`
applied to every task, print "no tasks needed design", and exit.

Set `AL_ARCH_RUN_ID=<run-id>` so the arch-phase-gate hook activates.

## Phase 1 — Context gathering (parallel, one per task)

For each task in the work list, dispatch ONE `arch-context-gatherer`
subagent in a single parallel Agent-tool message. Prompt template:

> Task JSON: `<task>`. Run id: `<run-id>`. Produce
> `.agentic/<run-id>/arch/scratch/<task-id>/context.md` per your
> system prompt.

Wait for all to complete. Verify each context.md exists.

## Phase 2 — Design (sequential per task, with revision loop)

For each task, at most TWO iterations:

**Iteration 1:**
1. Dispatch `arch-designer` with prompt:
   > Task: <task_id>. Context: `.agentic/<run-id>/arch/scratch/<task-id>/context.md`.
   > Produce `.agentic/<run-id>/arch/designs/<task-id>.md` per your system prompt.
2. Dispatch `arch-critic` with prompt:
   > Design: `.agentic/<run-id>/arch/designs/<task-id>.md`. Context:
   > same as above. Verdict: approved | needs-revision | blocked.
3. Parse critic verdict:
   - `approved` → task's `design_status: "approved"`, done.
   - `blocked` → task's `design_status: "blocked"`, done. (Also the
     critic has already appended to `blocked-for-review.jsonl`.)
   - `needs-revision` → go to Iteration 2.

**Iteration 2 (only on `needs-revision`):**
1. Dispatch `arch-designer` AGAIN with prompt:
   > Revise the design at `.agentic/<run-id>/arch/designs/<task-id>.md`.
   > The arch-critic's feedback is in the `## Critic feedback` section.
   > Produce a revised design in place (update frontmatter
   > `iterations` field).
2. Dispatch `arch-critic` AGAIN.
3. Parse verdict:
   - `approved` → done.
   - `needs-revision` or `blocked` → force `design_status:
     "blocked"` (we don't iterate further) and append to
     `blocked-for-review.jsonl` if not already there.

## Phase 3 — Augment handoff

Read the original `handoff.json`. For every task:
- If it wasn't in the needs-design work list: copy as-is with
  `design_status: "not-needed"`, `design_path: null`,
  `design_iterations: 0`.
- If it was: augment with `design_status`, `design_path` (if
  approved), `design_iterations`, and `blocked_reason` (if blocked).
- `needs_design` is flipped to `false` on approved tasks.

Write to `.agentic/<run-id>/arch/arch-handoff.json`. Validate with
`.claude/hooks/schema-validate.sh arch-handoff`.

## Phase 4 — Present

Terse terminal summary:

```
Arch run for <run-id> complete.
  Tasks with designs: <n> approved, <n> blocked (returned to Review).
  arch-handoff: .agentic/<run-id>/arch/arch-handoff.json
  Designs:     .agentic/<run-id>/arch/designs/
```

## Dispatch discipline

- Phase 1 is parallel (N context-gatherers at once).
- Phase 2 is sequential per task (designer→critic, then optionally
  designer→critic again). Tasks are serialized across tasks because
  they may reference each other's files and parallel designs could
  conflict on what to recommend.
- If you have > 10 tasks, consider running Phase 2 per-task in
  parallel ONLY when the tasks' `owner_crate` sets are disjoint. For
  v1, keep it serial — simpler.

## Failure handling

- Designer timing out → retry once, then mark task blocked with
  reason "designer-timeout".
- Critic timing out → treat as `approved` with a note (benefit of the
  doubt to the designer; Dev will catch the issue at implementation).
