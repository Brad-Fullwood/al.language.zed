# Schema: Finding

`$schema_version: "1"`

A single line of `findings/*.jsonl`, `verified/*.jsonl`, or
`report/findings.jsonl`. Finding identity is stable across a run and
across runs — the same structural issue in the same file gets the same
`id` every time it's surfaced, so run-to-run diffing is precise.

## JSON Schema

```jsonc
{
  "$schema_version": "1",
  "id": "string",                  // sha1(reviewer + file + line + category + kind)
  "reviewer": "string",            // the agent that produced this finding
  "kind": "bug | risk | refactor | gap | doc",
  "severity": "critical | high | medium | low | nit | speculative",
  "category": "bug | architecture | code-quality | rust | testing | security | performance | grammar | al-extract | missing-feature | docs | tooling | observability | refactor | misc",
  "file": "string",                // repo-relative POSIX path
  "line": number,                  // 1-indexed
  "line_end": number | null,       // 1-indexed, optional
  "what": "string",                // one paragraph, wraps at 80 cols
  "why": "string",                 // one paragraph — why it matters
  "fix": "string | null",          // suggested approach
  "what_we_know_now": "string | null",   // REQUIRED for kind=refactor
  "scope_estimate": "string | null",     // REQUIRED for kind=refactor; "S|M|L|XL" or free text
  "reproduction": {
    "kind": "cargo-test | manual | grep | none",
    "command": "string | null",
    "expected": "string | null",
    "observed": "string | null"
  } | null,
  "evidence": "static | grep | test | manual",
  "confidence": number,            // 0-100, filled by validator in Phase 4
  "verdict": "verified-static | reproduced | misdiagnosed | false_positive | null",
  "rebuttal": "string | null",     // only for false_positive
  "reclassified_from": {           // only for misdiagnosed
    "what": "string",
    "why": "string",
    "fix": "string | null"
  } | null,
  "cross_critic": "ok | suspect | null",
  "corroborated_by": ["reviewer-name", ...],   // filled by reducer during dedup
  "status": "new | persistent | regressed | resolved | null",   // filled by reducer vs previous run
  "needs_design": boolean,
  "timestamp": "ISO-8601 string"
}
```

## Required-field rules by phase

### Phase 2 / Phase 3 (candidate writers)
Required: `$schema_version`, `id`, `reviewer`, `kind`, `severity`,
`category`, `file`, `line`, `what`, `why`, `evidence`, `timestamp`.

Forbidden (will be filled later): `confidence`, `verdict`, `rebuttal`,
`reclassified_from`, `cross_critic`, `corroborated_by`, `status`,
`needs_design`.

### Phase 4 (validator writers)
Validator updates in place or copies to `verified/`:
- MUST set `verdict`.
- MUST set `confidence` on `verdict: verified-static`.
- MUST set `reproduction` on `verdict: reproduced`.
- MUST set `rebuttal` on `verdict: false_positive`.
- MUST set `reclassified_from` on `verdict: misdiagnosed`.
- MAY set `cross_critic`.

### Phase 5 (reducer writer)
Reducer writes `report/findings.jsonl`:
- MUST set `corroborated_by` (may be empty array).
- MUST set `status` (`new | persistent | regressed` for current-run findings).
- MUST set `needs_design` per the rules in
  [../arch/needs-design-criteria.md](../../arch/needs-design-criteria.md).

### Refactor-kind special rule
If `kind == "refactor"`, both `what_we_know_now` and `scope_estimate`
MUST be non-null and non-empty. The reducer REJECTS any refactor
finding missing either field and writes an entry into
`report/rejected.jsonl` with a one-line reason.

## `id` computation

```
id = sha1(
  reviewer + "\n" +
  file + "\n" +
  line + "\n" +
  category + "\n" +
  kind
).hexdigest()[:16]
```

First 16 hex chars. Stable across runs as long as the finding's
structural identity (same file, same line, same kind+category,
same reviewer identity) is the same.

## Validation

`jq -c . < file.jsonl > /dev/null` must pass. The full validator lives at
`.claude/hooks/schema-validate.sh finding <path>`.
