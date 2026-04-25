# Agentic Loop — Organization Map

`$schema_version: "1"`

This file is the authoritative map of the agentic system. Every department
agent, every command, every hook that touches the loop MUST treat this file
as the source of truth for department boundaries and dispatch order. If this
file disagrees with an agent's system prompt, this file wins and the prompt
is wrong.

## Departments

| # | Department | Command | Role | Consumes | Produces |
|---|---|---|---|---|---|
| 1 | Review | `/review-all [args]` | Find and validate every issue worth knowing | branch state + optional previous run pointer | `review/FINAL.md` + `handoff.json` + `findings.jsonl` |
| 2 | Architecture | `/arch-plan <handoff>` | Design refactor/cross-crate tasks before Dev touches them | `handoff.json` with `needs_design: true` tasks | `arch-handoff.json` + per-task `designs/<task-id>.md` |
| 3 | Development | `/dev-implement <handoff>` | TDD-enforced, scoped, reviewed implementation | `handoff.json` or `arch-handoff.json` | commits + `handoff-progress.json` + `commit-map.json` |
| 4 | Release | `/release-prep [options]` | Bundle completed batch into shippable unit | `handoff-progress.json` + HEAD state | `release/audit.md` + `changelog-entries.md` + `pr-description.md` (+ PR if `--pr`) |
| 0 | Overseer | `/loop [options]` | Run the full cycle end-to-end | branch + caps + convergence criteria | `overseer/cycle-log.jsonl` + `convergence-report.md` + persistent `docs/agentic-log.md` entry |

## Dispatch order

The Overseer cycles in this order. Departments are each independently
invocable outside the loop; the Overseer just automates the chain.

```
/review-all
    └─> handoff.json
        └─> any task.needs_design == true?
             ├─ yes → /arch-plan → arch-handoff.json
             └─ no  → handoff.json stays the input
                      └─> /dev-implement → handoff-progress.json
                                              └─> /release-prep (optional, gated by --auto-release)
                                                      └─> /review-all (next cycle)
```

Stopping conditions (any one triggers halt):
- `cycle_count >= --max-cycles` (default 3).
- Number of new findings in the latest cycle < `--convergence-threshold`
  (default 3).
- Circuit breaker: a Dev cycle completes 0 tasks.
- Drift: a task marked `done` in a prior cycle reappears in the next
  cycle's findings with the same id.

## State layout

Every run gets one directory under `.agentic/<run-id>/`. See
[state-dir.md](state-dir.md) for the full tree. Every department writes
into its own subdirectory; nothing ever writes outside its own slice.

## Contracts

See [schemas/](schemas/). Each inter-department artefact has a pinned schema.
Schema evolution rules are in [schema-version.md](schema-version.md).

## Why departments and not one monolithic orchestrator

Four reasons, in priority order:

1. **Context discipline.** A monolithic orchestrator holds every finding,
   every design, every implementation in one context. That is the "Dumb Zone"
   by cycle 2. Each department holds only what its phase needs; cross-
   department handoff is file-based.
2. **Independent invocation.** A human mid-review should be able to run
   `/dev-implement` on a specific task without re-running Review. That only
   works if Dev doesn't depend on Review's in-memory state.
3. **Schema pressure forces sharp interfaces.** If two departments share an
   in-memory object, coupling creeps in silently. Forcing inter-department
   communication through a versioned JSON schema makes coupling visible
   in diffs.
4. **Agent specialisation.** A reviewer agent and an implementer agent
   need different tool allowlists, different models, different system
   prompts. Keeping them in separate departments makes those choices
   local instead of a big `if department == "review"` ladder in one prompt.
