---
description: Deep exhaustive multi-agent review of the current branch (full, diff, incremental, or since-run mode). Replaces Anthropic's cloud /ultrareview locally.
allowed-tools: Read, Grep, Glob, Bash, Write, Agent
---

# /review-all

Dispatch the Review Department against the current branch. Six phases,
13+ reviewers, empirical validation, run-to-run diffing.

## Usage

```
/review-all                      # full review of current branch
/review-all --diff               # only files changed vs dev
/review-all <base>..<head>       # only files changed in that range
/review-all --since <run-id>     # only files changed since a prior run
```

Optional flags:
- `--previous-run <run-id>` — explicit pointer for run-to-run diff.
  Default: most recent `.agentic/<id>/review/report/findings.jsonl`.
- `--skip-grammar` — force-skip the grammar reviewer even when the
  submodule is populated (useful for speed in mid-dev).

## Phase 0 — Orchestrator setup (do this first)

1. Parse the arguments → resolve `MODE ∈ {full, diff, incremental}`.
   - no args → `full`
   - `--diff` or `<base>..<head>` → `diff`
   - `--since <run-id>` → `incremental`
2. Compute `run_id = <UTC-ISO>-<git-short-sha>`.
3. Export `AL_REVIEW_RUN_ID=<run_id>` so the Stop hook activates.
4. Create `.agentic/<run_id>/review/{briefs,findings,verified,scratch,report}`.
5. If `git submodule status tree-sitter-al | head -c1` == `-` →
   `git submodule update --init --recursive` (auto-populate per
   plan). Record `manifest.submodules.tree-sitter-al`.
6. Compute the file list for `MODE != full` via
   `git diff --name-only <base>..<head>` (default base: `dev`).
7. Write `manifest.json` at `.agentic/<run_id>/manifest.json` per
   `.claude/docs/agentic/state-dir.md`. Include the previous-run
   pointer by scanning `.agentic/*/review/report/findings.jsonl` sorted
   by mtime.
8. Print `Run id: <run_id>` so the human can grep it.

## Phase 1–6

Follow `.claude/skills/review-orchestrate/SKILL.md` verbatim. Do not
invent alternative phases.

## Post-run

At end, print the TERSE summary defined in Phase 6 of the
orchestration skill. Do NOT paste the FINAL.md body into the terminal.

## Notes

- Parallel dispatch in Phase 2/3/4 uses ONE Agent-tool message per
  phase, multiple invocations. Keep orchestrator context small.
- If any subagent reply exceeds 1500 tokens, instruct it to rewrite
  shorter and fall back to "see .agentic/<run-id>/review/findings/<name>.jsonl".
- The Stop hook (`.claude/hooks/review-phase-gate.sh`) WILL reject the
  session's end if `report/FINAL.md` or `report/handoff.json` are
  absent. If it blocks, finish the reducer phase before exiting.

## If anything goes wrong

- Phase failure mid-run leaves half-state. User can inspect
  `.agentic/<run_id>/` for what was produced. For v1, the remediation
  is to delete the run and re-run. `--resume` is a future enhancement.
- If a subagent hits a rate limit, wait 60s and retry that single
  agent (do not re-parallel-dispatch).
