---
name: review-orchestrate
description: The phase-by-phase recipe the /review-all command body follows. Six phases, all disk-based, subagents return summaries not transcripts. Use this when invoking /review-all or when manually running a review phase.
---

# Review Orchestration

This is the executable recipe for the Review Department. `/review-all`
delegates to this skill.

## Preconditions

Before starting:

- The run-id is allocated (see `.claude/commands/review-all.md` for the
  computation: `<UTC-ISO>-<short-sha>`).
- `AL_REVIEW_RUN_ID` is exported so the Stop hook is active.
- `.agentic/<run-id>/review/` tree exists.
- `manifest.json` is written at `.agentic/<run-id>/manifest.json` with
  mode, branch, sha, changed files (diff mode), previous-run pointer,
  and phases initialized to `pending`.
- If `tree-sitter-al` submodule is bare: `git submodule update --init
  --recursive` has been run (the command body does this automatically).

## Phase 1 — Reconnaissance

Set `phases.review-p1 = in_progress` in manifest.

Dispatch ONE subagent: `review-coordinator`. Prompt:

> Run the Review Department's Phase 1 for run `<run-id>`. Read the
> `manifest.json`, `CLAUDE.md`, `docs/ultrareview_original.md`,
> `docs/ARCHITECTURE.md`, the cross-cutting-concerns doc, and the
> category-map doc. Produce 14 briefs at
> `.agentic/<run-id>/review/briefs/` per the brief-schema (7 domain +
> 7 specialist, including spec-runtime.md). Reply with files written
> and any ambiguities. Do NOT write findings.

Wait for completion. Verify 14 brief files exist (in particular,
`spec-runtime.md` MUST be present — runtime coverage is non-optional).
Set `phases.review-p1 = complete`.

## Phase 2 — Deep domain review (7 parallel)

Set `phases.review-p2 = in_progress`.

Dispatch SEVEN subagents IN PARALLEL (single message, 7 Agent tool
invocations). Each gets the prompt template:

> You are `<reviewer-name>`. Your brief is at
> `.agentic/<run-id>/review/briefs/<brief-file>.md`. Read it, then
> proceed. Write candidate findings to
> `.agentic/<run-id>/review/findings/<brief-file>.jsonl`. Reply per your
> system prompt.

The seven reviewer-name → brief mappings:
- review-worker-core-queries → domain-core-queries.md
- review-worker-core-infra → domain-core-infra.md
- review-worker-server → domain-server.md
- review-worker-client → domain-client.md
- review-worker-syntax → domain-syntax.md
- review-worker-symbols → domain-symbols.md
- review-worker-tests → domain-tests.md

After all complete, verify 7 findings files exist (empty is OK — but a
completely missing file is an error). Validate every file with
`.claude/hooks/schema-validate.sh finding <path>`. Any validation
failure pauses progress; the orchestrator reports the bad file.

Set `phases.review-p2 = complete`.

## Phase 3 — Specialist cross-cutting pass (7 parallel + pr-review-toolkit reinforcements)

Set `phases.review-p3 = in_progress`.

Dispatch SEVEN specialist subagents in parallel:
- review-spec-arch → spec-arch.md
- review-spec-security → spec-security.md
- review-spec-perf → spec-perf.md
- review-spec-concurrency → spec-concurrency.md
- review-spec-grammar → spec-grammar.md (MAY skip if submodule bare)
- review-spec-refactor → spec-refactor.md
- review-spec-runtime → spec-runtime.md
  **MANDATORY every cycle.** This is the only agent that launches binaries
  and exercises the daemon ↔ client wire format. Skipping it risks
  re-shipping a regression that static review cannot see (e.g. a daemon
  emitting `kind: "table"` that no client can deserialize). If the brief
  is missing or the agent is unavailable, log a `kind: gap, severity:
  critical` finding for the orchestrator and continue — never silently
  drop runtime coverage.

Plus, in the same parallel dispatch, FOUR pr-review-toolkit agents
(installed at user scope) as reinforcements. Each dispatched with a
tailored task:

- `pr-review-toolkit:silent-failure-hunter` — scan entire workspace for
  swallowed errors, empty match arms, silent fallbacks. Write findings
  to `.agentic/<run-id>/review/findings/spec-silent-failures.jsonl`
  using the project's finding schema.
- `pr-review-toolkit:comment-analyzer` — scan for lying comments,
  comment rot, docstrings that restate the name. Output to
  `spec-comments.jsonl`.
- `pr-review-toolkit:type-design-analyzer` — scan `al-core` and
  `al_core::syntax` for invariant expression, encapsulation, and type
  design quality. Output to `spec-type-design.jsonl`.
- `pr-review-toolkit:pr-test-analyzer` — scan both `tests/` trees for
  behavioural coverage gaps. Output to `spec-test-behavior.jsonl`.

Each pr-review-toolkit agent is called via the Agent tool with
`subagent_type: "pr-review-toolkit:<name>"`. The prompt asks them to
write in our project's finding JSONL schema (not their native format).

Validate all ~10 findings files with `schema-validate.sh`. Set
`phases.review-p3 = complete`.

## Phase 4 — Validation

Set `phases.review-p4 = in_progress`.

1. **Read.** Read every `findings/*.jsonl` into memory (or into a temp
   merged file).

2. **Pre-validator dedup (this is a free efficiency win).** Across all
   ~10 findings files, multiple agents routinely report the same bug
   from different angles (a worker spots a panic; spec-concurrency
   spots the await-across-DashMap that causes it; spec-runtime
   reproduces it). Cluster candidate findings by `(file_path,
   line ± 5)` BEFORE dispatching validators. For each cluster:
   - Keep the most severe finding as the canonical entry.
   - Attach the other findings' `kind` + `agent` as `co_signatures`
     metadata on the canonical entry.
   - Drop the duplicates from the validator queue.
   The reducer (Phase 5) was already doing this dedup *after*
   validation — moving it earlier means the validator (opus) only pays
   for unique findings. On a typical full-tree run this drops
   validator workload 25–40% with zero loss of coverage: every co-signed
   bug still appears in FINAL.md, with stronger evidence (multiple
   agents converged on it).

3. **Pack and partition.** Partition the deduplicated queue into
   batches of 20. (Was 10 — doubled to halve opus validator
   invocations. The validator's per-finding work doesn't grow much
   with batch size since each finding ships its own ±50-line cited
   slice; what grows is shared instructions, which are exactly the part
   that benefits from caching across a larger batch.)
   Within each batch, deduplicate cited slices: if two findings cite
   the same `(file, line_range)` region, ship the slice once and have
   both findings reference it. Saves validator input tokens on hot
   files (e.g., `Workspace.rs`) where 5+ findings cluster.
4. **Dispatch validators (parallel).** For each batch, one
   `review-finding-validator` subagent. Each receives:
   - The batch JSON (with `co_signatures` preserved on canonical
     findings — validators treat multi-agent agreement as positive
     prior, not as automatic verification).
   - For each finding, the cited file's ±50 line slice around `line`.
   - Pointer to `.claude/docs/review/cross-cutting-concerns.md`.
   Validators write to
   `.agentic/<run-id>/review/verified/batch-<N>.jsonl`.
5. **Collect verdicts.** For each finding with `verdict:
   needs-reproduction`, collect them into a reproduction queue.
6. **Dispatch test-runners (parallel, one per reproduction).** Each
   gets `.agentic/<run-id>/review/scratch/<finding-id>/` pre-created.
   Each test-runner updates its finding in place with
   `verdict: reproduced | false_positive | null(inconclusive)`.
7. **Dispatch cross-critics (parallel).** For every finding with
   `verdict: verified-static` AND `severity in {critical, high}`, one
   `review-cross-critic` (Haiku) subagent. Receives: the single
   finding + cited slice + validator's verdict. Replies
   `ok: ...` or `suspect: ...`. Suspects are re-queued through step 4
   as fresh candidates for the validator (at most one re-validation
   per finding to avoid infinite loops).
8. **Consolidate.** Merge all batches:
   - `verified.jsonl` — verdict in {`verified-static` (not suspect),
     `reproduced`}. Carry forward `co_signatures` so the reducer can
     surface them in FINAL.md.
   - `misdiagnosed.jsonl` — verdict `misdiagnosed`.
   - `false-positive.jsonl` — verdict `false_positive`.
   - `stats.json` — kill rate, reproduction rate, cross-critic flip
     rate, dedup ratio (candidates → unique).
9. Set `phases.review-p4 = complete`.

## Phase 5 — Synthesis

Set `phases.review-p5 = in_progress`.

Dispatch ONE `review-reducer` subagent. Prompt:

> Run Phase 5 for run `<run-id>`. Read verified/*.jsonl,
> manifest.json, and (if present) the previous run's
> report/findings.jsonl. Write report/FINAL.md, report/findings.jsonl,
> report/handoff.json, report/resolved.jsonl, report/rejected.jsonl.
> Reply with the executive summary.

After reply:
- Validate report/findings.jsonl with `schema-validate.sh finding`.
- Validate report/handoff.json with `schema-validate.sh handoff`.
- Verify FINAL.md has all required headings.
- Set `phases.review-p5 = complete`.

## Phase 6 — Present

Print a TERSE terminal summary (do NOT dump the report body into
context):

```
Review run <run-id> complete.
  Verified: <n>  Misdiagnosed: <n>  False-positive: <n>  Rejected: <n>
  By kind:    bug <n>, risk <n>, refactor <n>, gap <n>, doc <n>
  By severity: critical <n>, high <n>, medium <n>, low <n>
  Report: .agentic/<run-id>/review/report/FINAL.md
  Handoff: .agentic/<run-id>/review/report/handoff.json
```

Then exit. The Stop hook will verify artefacts before allowing exit.

## Failure handling

- Any phase failing leaves the run half-finished. Re-invoke
  `/review-all --resume <run-id>` (future — not in v1). For now,
  clean up and re-run.
- A subagent returning an error is written to `manifest.phases.<phase>
  = failed` and the orchestrator halts with a pointer at the broken
  subagent's output.
- The Stop hook WILL block completion without FINAL.md + handoff.json,
  so incomplete runs cannot silently "succeed."

## Dispatch discipline (critical)

- Parallel dispatch uses ONE message with multiple Agent tool calls.
  Not sequential.
- Subagents return ≤800 tokens. Never read their full transcripts into
  the orchestrator context.
- Orchestrator never reads finding JSONL bodies — only paths. The
  reducer is the only agent that reads all findings at once.
