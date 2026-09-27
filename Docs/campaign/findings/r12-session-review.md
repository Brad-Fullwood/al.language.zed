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
- [x] 7. Merge damage: R12-MERGE-1. The twelve merges were merged again with `git merge-tree --write-tree` on their parents. Eleven (9a364d46, aaa3ca37, 85426311, b5c05341, 16514e84, 0d851d9d, dab2eb06, b2c38e6a, 6532618e, a4d45426, 7b607e40) equal the automatic merge. 19a7cae9 is the one with conflicts (LOG.md 13:10: "two conflicts taken from the fix side"), and its tree differs from the automatic merge outside the two conflict hunks: `Docs/features/native-test-runtime.md` lost the six paragraphs 0738d54c (docs re-check 3) had corrected, and `crates/al-runtime/src/interpreter/value.rs:46` lost the R10-TEXT-1 rewrite of the `lock` comment. Both losses are still in the tree at 7b607e40. No code line was lost: the `value.rs` difference outside the comment is the conflict hunk itself, which the fix side resolves correctly (`Collection<Vec<Value>>` and `Collection<DictEntries>`).
- [ ] 8. Text in the diff against the writing rules.

## Findings

### [R12-MERGE-1] merge 19a7cae9 took two whole files from the fix side, which undid the docs re-check and one R10-TEXT-1 line
- where: Docs/features/native-test-runtime.md:50-51 (the dispatch order reads "receiver-specific stubs → catalog stubs → built-in globals → real workspace procedures", the text 0738d54c replaced), :55 (no SingleInstance exception for a subscriber's instance), :69-71 (`Clear` is no longer listed or described), :137 (the DeleteAll and ModifyAll globals sentence is gone), :154 (the table globals sentence is gone), :156 (the `Sender` sentence is gone), crates/al-runtime/src/interpreter/value.rs:46 (the `lock` comment again ends "as they were; use them."), against the automatic merge `git merge-tree --write-tree aaa3ca37 7f24a581` (tree bc81ef88), whose only conflicts are the List and Dictionary bullet in the doc and the `List` and `Dict` variants in value.rs
- severity: medium
- scenario: `git diff bc81ef88 19a7cae9` shows 15 insertions and 43 deletions across the two files where a conflict resolution that keeps the fix side's two hunks would show only those hunks. The doc side of docs re-check 3 (`docs-review.md`, re-check section, the `native-test-runtime.md` items: dispatch order, `Clear`, table globals, `Sender`, the `var` write-back into a record field, subscriber instances, `ModifyAll` and `DeleteAll` globals) now describes a runtime the code does not have. A reader of the doc at 7b607e40 is told a call goes through catalog stubs before workspace procedures, which `dispatch/` stopped doing before round 9 (the reason the re-check rewrote the paragraph), and is not told that `Clear` runs locally, that a record keeps its table's globals or that `Sender` is the publisher. `git diff aaa3ca37 7b607e40 -- Docs/features/native-test-runtime.md` reproduces the loss line by line. The LOG.md entry for the merge records "two conflicts taken from the fix side" and does not mention the rest of the two files.
- fix: apply 0738d54c's six hunks onto the current `native-test-runtime.md` (they do not touch the List and Dictionary bullet, so they apply cleanly), and put the full stop back in `value.rs:46`. Note in LOG.md that the merge's resolution took whole files.
- status: open

