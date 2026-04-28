# `.agentic/<run-id>/` — the run directory contract

`$schema_version: "1"`

Every invocation of any department command uses a single run directory
under `.agentic/`. Sibling runs never collide; a single run's artefacts
are all self-contained for offline inspection and for downstream
department handoffs.

## Run-id format

```
<UTC-ISO>-<short-sha>
```

- `UTC-ISO` = `YYYYMMDDTHHMMSSZ` (ISO 8601 compact, UTC).
- `short-sha` = first 8 chars of `git rev-parse HEAD` at run start.

Scheduled Routines use `<YYYYMMDD>-scheduled-<short-sha>`.

Human-entered ids are rejected; the command generates the id and prints it.

## Full tree

```
.agentic/
├── <run-id>/
│   ├── manifest.json                 # who ran what, when, flags
│   ├── review/
│   │   ├── briefs/
│   │   │   └── <reviewer-name>.md    # per-reviewer brief (Phase 1 output)
│   │   ├── findings/
│   │   │   └── <reviewer-name>.jsonl # candidate findings (Phase 2+3 output)
│   │   ├── verified/
│   │   │   ├── verified.jsonl
│   │   │   ├── misdiagnosed.jsonl    # kept — symptom real, explanation wrong
│   │   │   └── false-positive.jsonl  # discarded — with rebuttal
│   │   ├── scratch/
│   │   │   └── <finding-id>/
│   │   │       ├── repro.rs
│   │   │       └── cargo-test.log
│   │   └── report/
│   │       ├── FINAL.md              # human-readable ultrareview-shaped report
│   │       ├── findings.jsonl        # every verified/misdiagnosed finding
│   │       ├── resolved.jsonl        # only when a previous run exists
│   │       └── handoff.json          # primary interface to downstream
│   ├── arch/
│   │   ├── designs/
│   │   │   └── <task-id>.md
│   │   ├── scratch/
│   │   │   └── <task-id>/context.md
│   │   └── arch-handoff.json         # handoff augmented with design_path
│   ├── dev/
│   │   ├── work-logs/
│   │   │   └── <task-id>/
│   │   │       ├── understand.md
│   │   │       ├── red.txt
│   │   │       ├── green.txt
│   │   │       ├── spec-review.md
│   │   │       └── quality-review.md
│   │   ├── handoff-progress.json     # per-task status, accumulated
│   │   └── commit-map.json           # task_id → commit_sha
│   ├── release/
│   │   ├── audit.md
│   │   ├── changelog-entries.md
│   │   ├── changelog-draft.md        # only if project has CHANGELOG.md
│   │   └── pr-description.md
│   └── overseer/
│       ├── cycle-log.jsonl           # one JSON object per cycle
│       └── convergence-report.md     # written at loop end
└── scheduled/
    └── <YYYYMMDD>/                   # Routine output goes here
        └── <run-id>/                 # nested under the date
```

## Who writes where

| Directory | Owner | Readable by |
|---|---|---|
| `review/` | Review Department only | all downstream |
| `arch/` | Architecture Department only | Dev, Overseer |
| `dev/` | Development Department only | Release, Overseer, next-cycle Review |
| `release/` | Release Department only | Overseer |
| `overseer/` | Overseer only | humans, next `/loop` invocations |
| `manifest.json` | Overseer or the first department that runs | everyone |

**No department writes into another department's subdirectory.** Ever.
If Review needs a design context, Review ingests `arch-handoff.json` —
it does not modify the design doc in `arch/designs/`.

## `manifest.json`

Written at the start of the first command in a run. Later commands read
it to learn run mode, branch, shas, etc.

```jsonc
{
  "$schema_version": "1",
  "run_id": "20260424T183000Z-a8d207a6",
  "started_at": "2026-04-24T18:30:00Z",
  "started_by": "/review-all",
  "mode": "full | diff | incremental | scheduled",
  "branch": "dev",
  "base_branch": "dev",
  "head_sha": "a8d207a6...",
  "dirty_files": [],
  "changed_files": [],              // populated only in diff/incremental mode
  "crate_inventory": {
    "al-core":     { "files": 110, "lines": 55000 },
    "al-protocol": { "files":   4, "lines":   450 },
    "al-explorer": { "files":  10, "lines":  6000 }
    // ...
  },
  "submodules": {
    "tree-sitter-al": "populated | bare"
  },
  "installed_plugins": ["superpowers@5.0.7", "pr-review-toolkit@...", ...],
  "previous_run": ".agentic/20260423T022300Z-605a638a",
  "phases": {
    "review": "pending | in_progress | complete | failed",
    "arch":   "skipped | pending | in_progress | complete | failed",
    "dev":    "...",
    "release": "...",
    "overseer": "..."
  }
}
```

## Cleanup

`.agentic/` is gitignored. Nothing cleans it automatically; runs accrete.
The Overseer's persistent summary (`docs/agentic-log.md`, committed)
contains just enough context that old `.agentic/` directories can be
deleted by the user without losing history.
