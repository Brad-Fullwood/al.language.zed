# R13 review: merges since the round 12 review

Scope: `git diff 7b607e40..73ed8751` (82 files, about 5,100 added lines, eleven merges in
`git log --merges 7b607e40..73ed8751`). That covers the security round 6 fixes (283b8415 SEC6-1
and SEC6-2, a5be8edb SEC6-3 to SEC6-5, 5f74977c SEC6-6, d9225520 SEC6-7 and SEC6-8 and the
dotnet advisory file name, statuses in `r6-security.md`), the round 6 security review itself
(204d44ee), the daemon `app.json` reload (18a1905f), the queued batch 3 (0f19a5a7), the round 12
review commits (3c303a94), the two CI fixes at the pause (3c831399 and the checkpoint's
`8cf97b56`, which is before this range), the round 12 fixes (77308d91 R12-LIST-1, R12-LIST-2,
R12-RT-1, R12-RT-2, and a624bdf4 R12-KW-1, R12-MERGE-1, R12-TEXT-1, statuses in
`r12-session-review.md`) and the grammar pointer move (73ed8751, `tree-sitter-al` from 142aba6 to
fc80b85, R12-GR-1). Read-only review of the tree at 73ed8751 in a worktree on
`campaign/r13-review`.

Each `fixed` status was read again with its test, the test was checked to read what its name
says and to fail without the fix, and a scratch scenario the test does not cover was run.
Findings are added one at a time as they are confirmed. A claim that holds is noted in its
coverage line with what was tried.

Tools: cargo test one crate at a time, `git merge-tree --write-tree`, Microsoft Learn for
Business Central behaviour.

## Coverage

Scratch tests for the scenarios below stay uncommitted in the worktree, named `r13_scratch_...`,
for the orchestrator to save as a patch.

- [ ] 1. 283b8415 (SEC6-1, SEC6-2).
- [ ] 2. a5be8edb (SEC6-3, SEC6-4, SEC6-5).
- [ ] 3. 204d44ee (round 6 security review).
- [ ] 4. 18a1905f (daemon `app.json` reload).
- [ ] 5. 0f19a5a7 (queued batch 3: the diagnostics clear, the pack-native notice).
- [ ] 6. 3c303a94 (round 12 review commits).
- [ ] 7. The two CI fixes at the pause (3c831399, 8cf97b56).
- [ ] 8. 77308d91 (R12-LIST-1, R12-LIST-2, R12-RT-1, R12-RT-2).
- [ ] 9. d9225520 (SEC6-7, SEC6-8, the dotnet advisory file name).
- [ ] 10. 5f74977c (SEC6-6).
- [ ] 11. a624bdf4 (R12-KW-1, R12-MERGE-1, R12-TEXT-1).
- [ ] 12. 73ed8751 (the grammar pointer at fc80b85, R12-GR-1).
- [ ] 13. 9a364d46 (R10-PLUGIN-1, R10-TEXT-1), left unticked by round 12.
- [ ] 14. Merge damage: each merge's parents merged again with `git merge-tree --write-tree`.
- [ ] 15. Readers of the `for` iterator, `argument_list` and `expression_list` against fc80b85, the node kind guard, the query files and `extension.toml`.
- [ ] 16. Text in the added lines against the writing rules.

## Findings
