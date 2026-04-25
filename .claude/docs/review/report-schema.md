# Review report schema (Phase 5 reducer output)

The reducer writes `.agentic/<run-id>/review/report/FINAL.md`. Human-
readable; the machine-readable counterpart is `findings.jsonl` next
to it.

## Required structure

```markdown
# Review Run <run-id>

> Branch: <branch>   Head: <short-sha>   Mode: <full|diff|incremental>
> Previous run: <run-id or "none">   Generated: <ISO-8601>

## Executive summary

One paragraph. What's the shape of the problems? What's the biggest risk?

- **Total findings:** N (X new, Y persistent, Z regressed)
- **Resolved since last run:** M
- **By kind:** bugs N, risks N, refactors N, gaps N, docs N
- **By severity:** critical N, high N, medium N, low N, nit N, speculative N
- **Reviewer coverage:** list of reviewers, count per reviewer
- **Mis-diagnosed (kept — explanation wrong, symptom real):** N

## Since last run

Populated only if `manifest.previous_run` is set. Subsections:

### New findings
Short numbered list with severity + one-liner + file:line link. No
full finding text; reference by id.

### Regressed findings
Previously dismissed, now verified. Escalated automatically.

### Resolved findings
Previously verified, now absent. Good news; confirms prior fixes held.

## Top 10 to fix first

Numbered list. Each entry: severity, kind, file:line, one-line description,
link to the finding detail below. Ordered by `impact × reproducibility × status`.

## Strategic recommendations

Bulleted list. Cross-cutting patterns the reviewers noticed:
- Dep-rule drift in al-syntax
- UTF-16 handling inconsistent across al-core queries
- ... etc.

Each recommendation includes ≥3 supporting finding ids.

## Missing features worth building

Bulleted list from `kind: gap` findings. Prioritized.

## Findings

Grouped by `kind` (order: bug, risk, refactor, gap, doc).
Within each kind, grouped by severity (critical → speculative).

For each finding:

````markdown
### <severity> · <kind> · <category> · <id>

**File:** `<path>:<line>` — [open](path#L<line>)
**Reviewer:** <reviewer-name>   **Confidence:** <n>/100   **Verdict:** <verdict>
**Status:** <new|persistent|regressed>   **Corroborated by:** <list or "—">

**What.** <one paragraph>

**Why.** <one paragraph>

**Fix.** <paragraph or "none proposed">

**What we know now.** <only if kind=refactor>

**Scope estimate.** <only if kind=refactor>

**Reproduction.**
- Kind: <cargo-test|manual|grep|none>
- Command: `<cmd>`
- Expected: <what we want>
- Observed: <what we get>

**Cross-critic.** <ok|suspect|—>

**Reclassified from.** <only if verdict=misdiagnosed — the original reviewer's what/why>
````

## Misdiagnosed findings

Full section after the main findings list. Same per-finding format but
with the original interpretation preserved in `reclassified_from`. The
reader needs to see both what the reviewer thought AND what the
validator found, because either could be right — the point is the
symptom is real.

## False positives (discarded, shown for audit)

Compact table: id, severity, category, file:line, one-line rebuttal.
No full text. Just so the reader can audit the validator's judgement.

## Run statistics

- Phase durations
- Subagent token costs
- Findings per reviewer (table)
- Kill rate (false_positive / total)
- Reproduction rate (reproduced / needs-reproduction)
- Cross-critic flip rate (suspect / total `verified-static`)
```

## Validation

`.claude/hooks/schema-validate.sh review-report <path>` (minimal — checks
presence of each required `##` heading).
