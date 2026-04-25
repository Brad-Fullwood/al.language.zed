# Review Department's slice of `.agentic/<run-id>/`

See [`.claude/docs/agentic/state-dir.md`](../agentic/state-dir.md) for the
full run-directory contract. Review writes only into
`.agentic/<run-id>/review/`. Nothing else.

## Tree

```
.agentic/<run-id>/review/
├── briefs/
│   ├── domain-core-queries.md
│   ├── domain-core-infra.md
│   ├── domain-server.md
│   ├── domain-client.md
│   ├── domain-syntax.md
│   ├── domain-symbols.md
│   ├── domain-tests.md
│   ├── spec-arch.md
│   ├── spec-security.md
│   ├── spec-perf.md
│   ├── spec-concurrency.md
│   ├── spec-grammar.md         # may say "skipped: submodule bare"
│   └── spec-refactor.md
├── findings/
│   ├── domain-*.jsonl          # 7 files
│   └── spec-*.jsonl            # 6 files
├── verified/
│   ├── verified.jsonl
│   ├── misdiagnosed.jsonl
│   ├── false-positive.jsonl
│   └── stats.json
├── scratch/
│   └── <finding-id>/
│       ├── repro.rs
│       └── cargo-test.log
└── report/
    ├── FINAL.md
    ├── findings.jsonl          # every verified/misdiagnosed finding
    ├── resolved.jsonl          # only if previous_run is set
    ├── rejected.jsonl          # refactor findings missing required fields
    └── handoff.json
```

## Naming conventions

- `briefs/<prefix>-<short-name>.md` — reviewer names start with
  `domain-` or `spec-`. No hyphen ambiguity.
- `findings/<same>.jsonl` — one per brief.
- `scratch/<finding-id>/` — finding-id, not reviewer-id. One
  reproduction attempt per finding, so one dir per finding.
- `report/` files never include the run-id in the filename — the dir
  already encodes it.

## Write order (enforced by `review-phase-gate.sh`)

1. `manifest.json` (written by orchestrator, Phase 0).
2. `briefs/*.md` (written by `review-coordinator`, Phase 1).
3. `findings/*.jsonl` (written by 7 domain + 6 specialist workers,
   Phases 2 and 3, in parallel). Each worker writes only its own file;
   no shared writes.
4. `verified/*.jsonl` (written by validators in Phase 4).
5. `scratch/<id>/` (written by test-runner in Phase 4 as needed).
6. `report/FINAL.md`, `report/findings.jsonl`, `report/resolved.jsonl`,
   `report/handoff.json` (written by `review-reducer` in Phase 5).

The Stop hook rejects `/review-all` completion if `report/FINAL.md` or
`report/handoff.json` are absent when the orchestrator claims done.
