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
- [x] 12. a298861f (R13-RT-5, R13-RT-6): no finding. The six tests in `records/r13_tests.rs` read what their names say and pass. With the `Enum` pair removed from `subtype_matches`, `an_overload_is_chosen_by_the_enum_type_of_its_argument` fails at r13_tests.rs:64, and with `Value::typed_list` building a list with no element type, the five R13-RT-6 tests fail (r13_tests.rs:213, :223, :230, :239, :262). Scratch shapes that hold (`r14_scratch_enum_overload_sources` in records_tests.rs): an enum read from a record field after `Get`, the default of an enum field on a new record, an enum returned by a procedure and a variable assigned from a field each run the `Enum "R14 Size"` overload. `"R14 Size".FromInteger(1)` is `procedure not found` locally, which is older than the range. Every list the runtime builds outside tests now carries a type (`GetRange` keeps the receiver's), apart from the set literal in `literals.rs:72`, which feeds `in` and is not an AL List.
- [ ] 13. edb1b375 (round 7 security review text)
- [ ] 14. d3d281f3 (round 13 review text)
- [ ] 15. 3851a033 (R13-LSP-1)
- [ ] 16. 8bcf8a22 (R13-DAEMON-1, SEC7-7)
- [x] 17. 41ab5260 (R13-RT-1 to R13-RT-4, SEC7-3, SEC7-5): R14-RT-1. The tests read what their names say and pass. Each fix disabled by hand fails its tests: with the orphan loop in `Shared::drop` replaced by dropping the work list, `chains_of_collections_300000_deep_drop` aborts on a stack overflow, and with `compare` calling itself for each pair, `chains_of_collections_100000_deep_compare` and `comparing_lists_nested_100000_deep_in_al_returns` abort while the drop test passes. With `holds` taken out of `child_for`, the two R13-RT-2 tests and the three SEC7-5 shapes abort, and with the depth checks in `write` and `deep_copy_at` disabled, `a_value_as_deep_as_the_limit_is_written_and_copied` fails at json.rs:1369. With `hold_bytes` and `check_held_bytes` refusing nothing, `copies_of_a_large_text_stop_at_the_test_budget` fails, and with `release_held_bytes` taking nothing off, `removed_and_dropped_elements_leave_the_test_budget` fails, each passing with the other change. With `thread_cancelled` returning false, `a_cancel_request_stops_list_contains_on_a_list_of_lists` fails at records_tests.rs:7838. Scratch shapes that hold (`r14_scratch_json_cycles_by_other_methods`): `J.Set(0, J)`, `O.Replace('a', O)` and `J.Insert(0, J)` each place a copy (`[[1]]`, `{"a":{"a":1}}`, `[[1],1]`), and a chain of 10,005 arrays built in AL gives the depth error on `WriteTo` and `Clone` with no abort. The `holds` walk runs over the whole value added, so building that chain one level per iteration takes about 50 s in the debug build where it took one step per level before, which the deadline bounds. The budget leaves out texts on the call stack and record fields, the second of which SEC7-3 named: R14-RT-1.
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

### [R14-RT-1] the byte budget counts what collections hold, so a text passed down a recursion or copied into records takes memory past it
- where: crates/al-runtime/src/interpreter/value.rs:676-693 (`MAX_HELD_BYTES` counts List and Dictionary elements, array elements, TextBuilders and JSON nodes, and nothing else), against crates/al-runtime/src/interpreter/dispatch/mod.rs:85 (`MAX_RECURSION_DEPTH`, 512, each level binding its own copy of a Text parameter) and crates/al-runtime/src/interpreter/records/crud.rs:783 (`Insert` stores the record's field values, which no budget counts), and the SEC7-3 scenario, which names "a record inserted in a loop" among the shapes
- severity: low
- scenario: SEC7-3 with the copies kept where the fix does not count them. `T := 'x'; for I := 1 to 25 do T := T + T; exit(Deep(T, 16))` with `local procedure Deep(T: Text; N: Integer): Integer` calling itself with `N - 1` returns 33554432 in 1.4 s with the test process's peak resident size at 1,162 MiB, past the 256 MiB budget with no error. At the recursion cap and the 64 MiB text cap the same shape asks for about 64 GiB, and the measured rate reaches the 30 s deadline at about 24 GiB. Record fields check their declared length, so the record route is slower: a table with ten `Text[2048]` fields and 20,000 rows inserted in a loop holds 419 MiB after 11 s in the debug build with no error. Confirmed with `r14_scratch_budget_stack` and `r14_scratch_budget_wide_records` in records_tests.rs.
- fix: charge a Text, Code or Blob parameter and local variable against the budget when it is bound and give it back when its frame ends, or bound the bytes a call frame's variables hold. Charge the text bytes of a record's fields on `Insert` and `Modify` and give them back on `Delete` and `DeleteAll`. Pin the recursion shape against the budget error.
- status: open

