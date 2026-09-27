# R12 review: merges since the round 11 review

Scope: `git diff 4c429ac5..7b607e40` (56 files, about 6,500 added lines, twelve merges in
`git log --merges 4c429ac5..7b607e40`). That covers the round 10 fixes (merges 9a364d46,
19a7cae9, 85426311, statuses in `r10-session-review.md`), the docs re-check 3 (aaa3ca37,
`docs-review.md`), the test module splits (b5c05341), the round 11 review itself (16514e84),
the round 11 fixes (0d851d9d, dab2eb06, 6532618e, statuses in `r11-session-review.md`), the
plugin round 6 runs (b2c38e6a, `plugin/TESTING.md`), security round 6 (a4d45426,
`r6-security.md`) and the mutation tests on the formatting module (7b607e40, `mutants.md`).
Read-only review of the tree at 7b607e40 in a worktree on `campaign/r12-review`.

Each `fixed` status was read again with its test, and a scratch scenario the test does not cover
was run. Findings are added one at a time as they are confirmed. A claim that did not reproduce
is noted in its coverage line.

Tools: cargo test one crate at a time, `git merge-tree --write-tree`, Microsoft Learn for
Business Central behaviour.

## Coverage

Scratch tests for the scenarios below stay uncommitted in the worktree, named `r12_scratch_...`,
for the orchestrator to save as a patch.

- [ ] 1a. 9a364d46 (R10-PLUGIN-1, R10-TEXT-1).
- [ ] 1b. 19a7cae9 (R10-REF-1, R10-REF-2, R10-DICT-1, R10-LIST-1, R10-LIST-2), with the two
  conflicts taken from the fix side.
- [ ] 1c. 85426311 (R10-RT-2, R10-EV-1, R10-RT-1, R10-KW-1, GR3-2, GR3-3).
- [ ] 2a. 0d851d9d (R11-SEC-1, R11-SEC-2, R11-SEC-3).
- [ ] 2b. dab2eb06 (R11-RT-1, R11-RT-2).
- [ ] 2c. 6532618e (R11-PLUGIN-1, R11-TEXT-1).
- [ ] 3. Docs re-check 3 (aaa3ca37): the 13 corrected claims against the code.
- [ ] 4. Test module splits (b5c05341): test names and counts on both sides.
- [ ] 5. Plugin round 6 runs (b2c38e6a): recorded commands against the skills, siblings of the
  stale `app.json`.
- [ ] 6. Mutation tests (7b607e40): behaviour or implementation.
- [ ] 7. Merge damage: every merge in the range merged again with `git merge-tree --write-tree`.
- [ ] 8. Text in the diff against the writing rules.

## Findings

