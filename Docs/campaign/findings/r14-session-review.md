# R14 review: merges since the round 13 review

Scope: `git diff 73ed8751..f4eb117a` (92 files without the submodule and `Cargo.lock`, about
16,400 added lines, most of them the two file splits and the source moves under them) and the 22
merges in `git log --merges 73ed8751..f4eb117a`. That covers the round 12 leftovers (fc04be15
R12-DAEMON-1, 34e8f8de R12-MUT-1), the `records.rs` and `eval_expr.rs` splits (c0e143b7,
bd40b490), the query drift check and the grammar pointer at baf782b (83820e8d), the snapshot
capture work (fc24b434), docs re-checks 4 and 5 (8bb95bcc, 7717215c), the round 13 fixes
(8554630d R13-TRUST-1 and R13-TRUST-2, a298861f R13-RT-5 and R13-RT-6, 41ab5260 R13-RT-1 to
R13-RT-4, 3851a033 R13-LSP-1, 8bcf8a22 R13-DAEMON-1, dfd6df9b R13-CLI-1, statuses in
`r13-session-review.md`), the security round 7 fixes (dfd6df9b SEC7-1, 8554630d SEC7-2 and
SEC7-4, 41ab5260 SEC7-3 and SEC7-5, 8bcf8a22 SEC7-7, statuses in `r7-security.md`), the
formatter case fixes (6cae13c5), the two review merges (edb1b375, d3d281f3), five merges of the
campaign branch into fix and review branches, and the merge repair d82b8d01. Read-only review of
the tree at f4eb117a in a worktree on `campaign/r14-review`.

Each `fixed` status was read again with its test, the test was checked to read what its name
says and to fail without the fix, and a scratch scenario the test does not cover was run.
Findings are added one at a time as they are confirmed. A claim that holds is noted in its
coverage line with what was tried.

Tools: cargo test one crate at a time, `git merge-tree --write-tree`.

## Coverage

Scratch tests for the scenarios below stay uncommitted in the worktree, named `r14_scratch_...`,
for the orchestrator to save as a patch.

- [ ] 1. 8bb95bcc (docs re-check 4)
- [ ] 2. c0e143b7 (`records.rs` split into a module directory)
- [ ] 3. fc04be15 (R12-DAEMON-1, package folders loaded again when they change)
- [ ] 4. bd40b490 (`eval_expr.rs` split into a module directory)
- [ ] 5. fc24b434 (snapshot capture behind debugger and runner traits)
- [ ] 6. 83820e8d (query drift fixes, grammar pointer at baf782b)
- [ ] 7. 34e8f8de (R12-MUT-1, the seven indent tests)
- [ ] 8. 7717215c (docs re-check 5)
- [ ] 9. dfd6df9b (SEC7-1, R13-CLI-1, the source tree test over the print sites)
- [ ] 10. 8554630d (R13-TRUST-1, R13-TRUST-2, SEC7-2, SEC7-4, the `refresh_trust` comment)
- [ ] 11. 6cae13c5 (formatter case fixes)
- [ ] 12. a298861f (R13-RT-5, R13-RT-6)
- [ ] 13. edb1b375 (round 7 security review text)
- [ ] 14. d3d281f3 (round 13 review text)
- [ ] 15. 3851a033 (R13-LSP-1)
- [ ] 16. 8bcf8a22 (R13-DAEMON-1, SEC7-7)
- [ ] 17. 41ab5260 (R13-RT-1 to R13-RT-4, SEC7-3, SEC7-5)
- [ ] 18. f8beee43 (the campaign branch at 7717215c merged into fix-sec7-1)
- [ ] 19. 255cc1f5 (the campaign branch at 7717215c merged into fix-r13-runtime)
- [ ] 20. 146d0592 (the campaign branch at 7717215c merged into fix-r13-trust)
- [ ] 21. 10e8cd90 (the campaign branch at 7717215c merged into sec7-review)
- [ ] 22. 2970b58a (the campaign branch at 7717215c merged into r13-review)
- [ ] 23. d82b8d01 (merge repair, `TrustDecision::from_parts`)
- [x] 24. Merge damage: each merge's parents merged again with `git merge-tree --write-tree`: no finding. Twenty one of the 22 merges have the tree of the automatic merge of their parents. fc04be15 (R12-DAEMON-1) differs in `Docs/features/daemon-protocol.md` alone, where the automatic merge stops on a conflict in the "Changes on disk" bullet: the campaign side had rewritten the `.al` walk sentence and the settings sentence, and the branch replaced the package folder sentence. The resolution keeps the campaign side's first six lines and the branch's package folder stamp, which is what each side meant to say, and the bullet at f4eb117a adds the trust side of the stamp that later work gave it. The one clash no merge tree shows is the a298861f build break (item 26).
- [ ] 25. Text in the added lines against the writing rules
- [ ] 26. The gate gap at a298861f and code no Linux gate compiles or runs

## Findings
