# R9 review: merges since the round 8 review

Scope: `git diff 2b7bce37..a0e85e0b -- crates plugin` (72 files). That covers the round 8 fix
batches (merges a1afe8e6, bd76f89f, 12b1778a), the codeunit instances work (9a9ce662 and the hand
merge 6b4be394), the mutation test merges (7e4c02f3, 58781aa7) and the plugin leftovers
(cf0f794c). Read-only review of the tree at a0e85e0b in a worktree on `campaign/r9-review`. The
round 7 and round 8 findings are not repeated, but each round 8 `fixed` status is checked.

Findings are added one at a time as they are confirmed. A claim that did not reproduce is noted
in its coverage line.

## Coverage

- [ ] 1. Round 8 fixes hold (each `fixed` entry, a scratch scenario its test does not cover, BC semantics from Learn)
- [ ] 2. Codeunit instances (9a9ce662, 6b4be394): globals per variable, `Clear`, by value and by `var`, SingleInstance, subscriber instance, table globals per record variable, `globals_for_call` against the router
- [ ] 3. Multi-object fixes (bd76f89f): per-object effect sites, `object_at_line`
- [ ] 4. Mutation tests (7e4c02f3, 58781aa7): ten tests, five `equivalent` verdicts
- [ ] 5. `plugin/scripts/al-fetch-release.sh` and `plugin/evals/`
- [ ] 6. Merge damage in the 13 merges of `git log --merges 2b7bce37..a0e85e0b`
- [ ] 7. The three round 8 claims nobody reproduced (STATE.md, Queued)
- [ ] 8. Audit triage spot-check: ten `fixed` rows

## Findings
