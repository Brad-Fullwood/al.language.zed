# Schema: Handoff (Review → downstream)

`$schema_version: "1"`

Produced at `.agentic/<run-id>/review/report/handoff.json`. Consumed by
`/arch-plan` and `/dev-implement` and (for cycle control) by `/loop`.

## Structure

```jsonc
{
  "$schema_version": "1",
  "run_id": "string",
  "branch": "string",
  "base_branch": "string",
  "head_sha": "string",
  "generated_at": "ISO-8601",
  "previous_run": "string | null",
  "summary": {
    "total": number,
    "by_kind":     {"bug": n, "risk": n, "refactor": n, "gap": n, "doc": n},
    "by_severity": {"critical": n, "high": n, "medium": n, "low": n, "nit": n, "speculative": n},
    "by_status":   {"new": n, "persistent": n, "regressed": n, "resolved": n}
  },
  "tasks": [
    {
      "task_id": "string",                 // MUST equal the finding's id
      "finding_ref": "string",             // same as task_id, provided for clarity
      "priority": "P0 | P1 | P2 | P3",
      "type": "bugfix | refactor | docs | test | arch | gap",
      "kind": "bug | risk | refactor | gap | doc",
      "owner_crate": ["al-core", ...],     // list even if single
      "title": "string",                   // short, verb-first
      "acceptance_criteria": ["string", ...],
      "blocked_by": ["task-id", ...],
      "estimated_effort": "S | M | L | XL",
      "what_we_know_now": "string | null", // refactor tasks only
      "reproduction": {
        "kind": "cargo-test | manual | grep | none",
        "command": "string | null",
        "expected": "string | null",
        "observed": "string | null"
      },
      "needs_design": boolean,
      "status": "new | persistent | regressed"
    }
  ],
  "strategic": ["string", ...],            // cross-cutting recommendations
  "missing_features": ["string", ...]      // kind:gap findings summarised
}
```

## Priority mapping

Reducer rules:
- `severity: critical` → `priority: P0`
- `severity: high` → `priority: P1`
- `severity: medium` → `priority: P2`
- `severity: low | nit | speculative` → `priority: P3`

A `status: regressed` finding is promoted one priority tier (never above P0).

## Type mapping

- `kind: bug` → `type: bugfix`
- `kind: risk` → `type: bugfix` (latent bugs; dev treats them the same)
- `kind: refactor` → `type: refactor`
- `kind: gap` → `type: gap`
- `kind: doc` → `type: docs`
- `category: architecture` regardless of kind → `type: arch`
- `category: testing` and `kind != bug` → `type: test`

## `needs_design` rules

Populated by reducer. See
[../arch/needs-design-criteria.md](../../arch/needs-design-criteria.md).

## Validation

`jq 'type == "object" and .tasks | type == "array"' handoff.json` must
return `true`. Full validator at `.claude/hooks/schema-validate.sh handoff`.

## Stability guarantee

Review Department commits to:
- `task_id` stability (matches finding `id`; stable across runs).
- Populated `acceptance_criteria` even if empty array (never omitted).
- Populated `reproduction` object (never null; `kind: "none"` if no repro).
- Refactor tasks always carry `what_we_know_now` non-null.

If Review changes this schema, it bumps `$schema_version` and Downstream
departments will refuse old handoffs.
