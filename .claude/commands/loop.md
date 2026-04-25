---
description: Operations Overseer — runs the full agentic cycle Review→(Arch?)→Dev→Release→Review until convergence, max-cycles, or a halt condition. Single command for autonomous improvement loops.
allowed-tools: Read, Grep, Glob, Bash, Write, Agent
---

# /loop

Run the full agentic loop end-to-end. The closing of the cycle:
Review → Arch (if needed) → Dev → Release (optional) → Review again.

## Usage

```
/loop                                       # default: 3 cycles, threshold 3, no auto-release
/loop --max-cycles 1                        # one cycle only (Review→Dev, no re-review)
/loop --max-cycles 5 --convergence 2        # tighter cap, looser threshold
/loop --priority P0,P1                      # only fix top-priority tasks
/loop --auto-release                        # also run /release-prep each cycle
/loop --dry-run                             # plan only, touch nothing
```

## Phase 0

1. Verify working tree clean (`git status --porcelain` empty).
2. Compute run-id.
3. Create `.agentic/<run-id>/overseer/`.
4. Init `cycle-log.jsonl` and `convergence-report.md` template.
5. Export `AL_OVERSEER_RUN_ID=<run-id>`.

## Cycles

Follow `.claude/skills/loop/SKILL.md` verbatim.

## Stopping conditions

- `cycle >= --max-cycles` → halted-capped.
- `new_findings < --convergence` → halted-converged.
- Dev tasks_completed == 0 → halted-circuit-breaker.
- Drift (done task reappears) → halted-drift.
- `--auto-release` and audit red → halted-red-audit.

## Outputs

- `.agentic/<run-id>/overseer/cycle-log.jsonl`
- `.agentic/<run-id>/overseer/convergence-report.md`
- `docs/agentic-log.md` (appended one entry — committed-file persistent log)

## Notes

- The Overseer NEVER pushes. With `--auto-release`, the only outbound
  action is `gh pr create` (which pushes the branch under the hood).
- Each department subprocess is independent: artefacts under their
  own sub-run-id directories. The Overseer references them by id.
- The Overseer is idempotent: re-invoking `/loop` with the same
  arguments after a SIGINT picks up where it left off (state in
  cycle-log.jsonl).
