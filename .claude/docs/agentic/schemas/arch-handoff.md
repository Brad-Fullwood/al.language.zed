# Schema: Arch Handoff (Architecture → Development)

`$schema_version: "1"`

Produced at `.agentic/<run-id>/arch/arch-handoff.json`. Superset of
`handoff.json`: every task that had `needs_design: true` now has a
`design_path` or a `blocked` status.

## Structure

Same as [handoff.md](handoff.md), plus per-task:

```jsonc
{
  "task_id": "...",
  // ... all handoff.json task fields ...
  "design_path": "string | null",          // relative to repo root
  "design_status": "approved | blocked | not-needed",
  "design_iterations": number,             // how many designer↔critic cycles
  "blocked_reason": "string | null"        // only if design_status == "blocked"
}
```

## Transformation rules (`/arch-plan` MUST enforce)

For every task in the input handoff:
- `needs_design: false` + no design work needed → copy through with
  `design_status: "not-needed"`, `design_path: null`.
- `needs_design: true` + critic approved → `design_status: "approved"`,
  `design_path` populated, `needs_design` is flipped to `false`.
- `needs_design: true` + critic blocked → `design_status: "blocked"`,
  `blocked_reason` populated, task EXCLUDED from Dev's input (Dev reads
  only `design_status != "blocked"` tasks).

## Side file: `blocked-for-review.jsonl`

For every blocked task, arch-plan writes a finding to
`.agentic/<run-id>/arch/blocked-for-review.jsonl` so the next-cycle Review
picks it up as a regressed-from-arch finding. Schema is
[finding.md](finding.md) with:
- `reviewer: "arch-critic"`
- `kind: "bug"` (because the original review finding was misdiagnosed)
- `reclassified_from` populated with the original finding's what/why/fix
- `status: "regressed"`

## Validation

`jq '.tasks | map(select(.design_status == null)) | length == 0' arch-handoff.json`
must return `true`. Every task MUST have a `design_status`.
