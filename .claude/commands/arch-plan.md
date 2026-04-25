---
description: Architecture Department entry point. Takes a handoff.json (from /review-all) and produces designs for every task marked needs_design:true, then emits arch-handoff.json for /dev-implement.
allowed-tools: Read, Grep, Glob, Bash, Write, Agent
---

# /arch-plan

Dispatch the Architecture Department against a review handoff.

## Usage

```
/arch-plan <path/to/handoff.json>
/arch-plan .agentic/<run-id>/review/report/handoff.json
```

If no argument: use the most recent
`.agentic/*/review/report/handoff.json`.

## Phase 0 — Orchestrator setup

1. Resolve the handoff file path. If none specified, glob
   `.agentic/*/review/report/handoff.json`, sort by mtime, pick most
   recent.
2. Validate with `.claude/hooks/schema-validate.sh handoff <path>`.
3. Compute / inherit run-id. If the handoff is at
   `.agentic/<run-id>/review/report/handoff.json`, reuse that run-id.
   Otherwise compute a fresh one.
4. Export `AL_ARCH_RUN_ID=<run-id>` so the arch-phase-gate hook
   activates.
5. Create `.agentic/<run-id>/arch/{designs,scratch}` if absent.

## Phases 1–4

Follow `.claude/skills/arch-plan/SKILL.md` verbatim.

## Post-run

Terse summary. Do NOT paste design bodies into context.

```
Arch run <run-id> complete.
  Designs produced: <n> approved, <n> blocked.
  arch-handoff: .agentic/<run-id>/arch/arch-handoff.json
  Next step: /dev-implement .agentic/<run-id>/arch/arch-handoff.json
```

## If the input has no needs_design tasks

Print "no tasks needed design; copying handoff as-is" and produce an
arch-handoff.json that's a straight copy with `design_status:
"not-needed"` on every task. Still validates as arch-handoff so Dev
can consume either file type uniformly.
