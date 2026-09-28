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

- [x] 1. 283b8415 (SEC6-1, SEC6-2): R13-TRUST-1, R13-TRUST-2. The ten tests read what their names say and pass (al-project lib, 276). With the three SEC6-1 hunks reverted by hand (`Found::new` without the search root, `find_in_project` back to `is_inside`, `linked_package_folders` without the analyzer folders) five of its six tests fail. `a_recorded_name_found_through_a_linked_netpackages_is_hashed_where_it_resolves` passes either way, since `find_in_project` followed the root link before the fix, so it pins older behaviour. With the four `names_a_project_file` calls reverted to `stays_inside_project` all four SEC6-2 tests fail. Scratch shapes that hold (`r13_scratch_...` in trust_tests.rs): a link below `.netpackages` is skipped and the NuGet copy loads, and a link that dangles at the grant is recorded `(not present)`, the record goes `Stale` when the target appears and the path is refused.
- [x] 2. a5be8edb (SEC6-3, SEC6-4, SEC6-5): R13-RT-1 to R13-RT-4. The eight runtime tests and the three runner tests read what their names say and pass. Each fix reverted by hand fails its test: with the met pair no longer ending the walk in `compare`, `comparing_lists_in_a_cycle_ends` overflows the stack and aborts the test binary, with `search_list` back to searching under the lock, `searching_a_list_in_a_cycle_returns` hangs until `run_cyclic` gives up at 60 s, and with the `PadStr` cap removed, `growing_a_text_past_the_limit_fails_the_test` gets a 2 GB text where it expects an error. Learn confirms the 1,000,000 element array limit the collection cap cites (AL0146). Scratch shapes that hold (`r13_scratch_...` in records_tests.rs): `foreach X in L do if L.Contains(X)` walks a snapshot and gives 3, and a 30,000 deep chain of lists compares equal in 2.6 s.
- [x] 3. 204d44ee (round 6 security review): no finding. The merge is the review text alone (26 lines in r6-security.md). The claims the tree can answer hold: `dispatch_table!` lists 95 arms at fa4fcf8f, 204d44ee and 73ed8751, every where line of SEC6-6 to SEC6-8 resolves at 204d44ee to the code it names, the four plugin scripts and `plugin/evals/run.sh` use no `eval` and `al-bin.sh` ends in `exec "$exe" "$@"`, `al-runtime/src` outside tests has no `std::fs`, `std::env`, `std::process` or socket use, and `MAX_RECURSION_DEPTH` is 512. The item 4 claims about the self referencing shapes hold at 73ed8751: `L.Add(L)` and `D.Add(D, 1)` are refused with a runtime message, `TB.Append(TB)` then `TB.Insert(1, TB)` gives `abababab`, and `Format` and `StrSubstNo` print a list as `<List>` (`r13_scratch_item3_...` in records_tests.rs). The merge shipped SEC6-7 and SEC6-8 without a `status:` line, which 59c598de added, so the format holds at 73ed8751.
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

### [R13-TRUST-1] a probing path written without `./` is recorded as text alone, so files at it, or at a link's target, change under a record that still matches
- where: crates/al-project/src/trust.rs:1027-1028 (`with_project_contents` returns the value as written unless `path.is_absolute() || value.contains(['/', '\\'])`, so `tools` is never resolved or hashed while `./tools` is), against crates/al-project/src/config.rs:968 (`merge_path_array` keeps `tools` as a relative `PathBuf`) and crates/al-project/src/analyzers.rs:317 (`discover` joins it to the project root and walks it)
- severity: low
- scenario: `"al.assemblyProbingPaths": ["tools"]` with `tools/Dep.dll` in the tree. `grant` records `al.assemblyProbingPaths = tools` with no hash. Replacing `tools/Dep.dll` leaves `decide` at `Trusted`, where `./tools` is recorded as `./tools (1 files, sha256:...)` and the same replacement gives `Stale`. With `tools` a link to a directory outside the project the record adds `linked package folder = tools (resolves to /tmp/...)`, so a retarget is caught, and replacing a file at the target still leaves the record `Trusted`, which is the SEC6-2 shape with the `./` dropped. Confirmed with `r13_scratch_probing_path_without_dot_slash_is_hashed_like_dot_slash` (`Trusted` against `Stale`) and `r13_scratch_bare_probing_path_linked_out_is_hashed_where_it_resolves` (`Trusted`) in trust_tests.rs.
- fix: in `privileged_changes`, pass every `al.assemblyProbingPaths` entry to `with_project_contents` as a path whatever its spelling (the caller knows the key), or make `is_path` true for a value that names an existing entry under the project root. Pin the `tools` spelling beside `./tools`.
- status: open

### [R13-TRUST-2] an absolute probing path spelled inside the project never has its analyzer copy recorded, so the name is refused in a trusted project
- where: crates/al-project/src/analyzers.rs:390 (`project_search_roots` drops every absolute probing path), used by `find_in_project` (:406) for `project_analyzer_copies` (trust.rs:579), against `discover` (analyzers.rs:323), which marks a hit under an absolute probing path spelled inside the project as `from_project`
- severity: low
- scenario: `.vscode/settings.json` with `"al.assemblyProbingPaths": ["<root>/tools"]` and `"al.codeAnalyzers": ["TeamCop"]`, and `tools/TeamCop.dll` in the tree. `grant` records the probing path with its tree hash and `al.codeAnalyzers = TeamCop` with no copy. `resolve("TeamCop")` in the trusted project returns `UnrecordedProjectAnalyzer`, whose text tells the user to write the entry in the project's settings, where it already is. The shape is older than the range, and the SEC6-1 hunk at analyzers.rs:323 extends it to a link out of the project. Confirmed with `r13_scratch_absolute_probing_path_inside_the_project` in trust_tests.rs.
- fix: keep an absolute probing path in `project_search_roots` when `spelled_inside_project` holds for it, so `find_in_project` and `discover` search the same roots.
- status: open

### [R13-RT-1] a chain of lists a few tens of thousands deep overflows the interpreter stack in `compare` and in `Drop`, and the process aborts
- where: crates/al-runtime/src/interpreter/value.rs:343 (`compare` recurses once per nesting level, and :463 `compare_in_order` with it), :31 (`Shared<T>(Arc<Mutex<T>>)`, whose drop recurses once per level), against crates/al-runtime/src/interpreter/dispatch/routing.rs:369 (`Clear` on a List gives the variable fresh contents, so `Prev := Cur; Clear(Cur); Cur.Add(Prev)` in a loop nests one level per iteration) and crates/al-runtime/src/interpreter/dispatch/mod.rs:61 (`INTERP_STACK_BYTES`, 64 MiB, the stack the runner gives the AL body)
- severity: low
- scenario: SEC6-3 ended the walk of a cycle, and a chain with no cycle still recurses once per level. `DeepChainCompare(60000)` (two chains built in the loop above, then `Cur = Other`) builds in about 5 s and ends with `thread '<unknown>' has overflowed its stack` and `SIGABRT`, where 30,000 compares equal in 2.6 s. `DeepChainDrop(300000)` (one chain, then the procedure returns and its locals drop) builds in about 13 s and aborts the same way, where 200,000 returns in 8.7 s. Both are inside the runner's 30 s deadline, and a deadline error drops the same locals. The runner would report the abort as the daemon going away. Confirmed with `r13_scratch_deep_chain_compare` and `r13_scratch_deep_chain_drop` on a 64 MiB thread in the debug test binary. The depth a release build needs is [UNVERIFIED] (running the two scratch tests with `--release` would give it), and is larger, while each level costs less to build.
- fix: make `compare` iterative with an explicit stack of pairs, or count depth and return an `Eval::Error` past a bound that fits the stack. Give `Shared<Vec<Value>>` and the dictionary contents a `Drop` that moves nested collections out into a work list before dropping them. Pin a chain of 100,000.
- status: open

### [R13-RT-2] `JA.Add(JA)` attaches a JSON array to itself, and the next `Add` or `WriteTo` recurses through the cycle and aborts the process
- where: crates/al-runtime/src/interpreter/json.rs:142-159 (`child_for` returns the node itself when it is not yet in `attached`, so the receiver's own id is pushed into its items, and marks it attached), :113-138 (`deep_copy`, taken on the next `Add` of an attached node, recurses through the cycle), :193 (`text_of`, which `WriteTo` at :952 uses, recurses the same way)
- severity: low
- scenario: `JA.Add(1); JA.Add(JA); exit(JA.Count())` gives 2, with the array holding itself. `JA.Add(1); JA.Add(JA); JA.Add(JA)` and `JA.Add(1); JA.Add(JA); JA.WriteTo(T)` both end with `thread '<unknown>' has overflowed its stack` and `SIGABRT`, in three statements no deadline reaches. Confirmed with `r13_scratch_json_self_add_once`, `r13_scratch_json_self_add_twice` and `r13_scratch_json_self_add_then_writeto` in records_tests.rs. Learn's `JsonArray.Add(JsonArray)` page does not say what Business Central does with an array added to itself, so its answer is [UNVERIFIED] (running the three statements in a test on a sandbox would give it).
- fix: in `child_for`, deep copy the node when it is the receiver or contains the receiver, before attaching, so an array holds a copy of itself as it holds a copy of any attached node, and the arena never holds a cycle. Pin `JA.Add(JA)` twice and `WriteTo` after it.
- status: open

### [R13-RT-3] the caps bound one value, so a large text copied into a list takes memory at 1 GiB a second inside them
- where: crates/al-runtime/src/interpreter/value.rs:497 and :504 (`MAX_TEXT_BYTES`, 64 MiB, and `MAX_COLLECTION_LEN`, 1,000,000, each checked for one value), crates/al-runtime/src/interpreter/records.rs:2494-2504 (`List.Add` checks the element count and clones the argument), against crates/al-runtime/src/interpreter/eval_stmt.rs:338-343 (the deadline is checked once per loop iteration and nothing counts memory)
- severity: low
- scenario: SEC6-5's shape with the growth spread over a list. `T := PadStr('', 16000000, 'x'); for I := 1 to 64 do L.Add(T)` returns 64 in 0.76 s with the process's peak resident size at 1,046 MiB (`VmHWM`), every value inside its cap. At the caps, a 64 MiB text added 1,000 times is 64 GiB, and the 30 s deadline allows about 40 GiB at the measured rate, more than this machine holds. A `Dictionary` value or an `array[1000] of Text` takes the same route. Confirmed with `r13_scratch_big_texts_in_a_list_memory` in records_tests.rs.
- fix: count bytes as well as elements. Keep a running total of text bytes held by the collections a test made (add on `Add`, `Insert`, `Set`, `AddRange` and array element assignment, subtract on `Remove` and `Clear`), and refuse past a test budget such as 1 GiB with an AL error, or cap the bytes one collection holds. Pin the 16 MB text added 64 times against the budget.
- status: open

### [R13-RT-4] `Contains` on a list of lists is one statement of quadratic cost, so a test outlives the watchdog and its thread keeps a core
- where: crates/al-runtime/src/interpreter/records.rs:2614-2626 (`search_list` compares the needle with every element, and each comparison of two lists walks their elements, with no deadline or cancel check inside), crates/al-test/src/backends/interp.rs:487-514 (`run_watched` stops waiting at the deadline plus `DEADLINE_GRACE` and leaves the thread running), against crates/al-runtime/src/interpreter/eval_stmt.rs:225-231 (the deadline and the cancel flag are read only between loop iterations)
- severity: low
- scenario: `Inner` holds 1..N, `Other` is a copy with the last element changed, `Outer` holds `Inner` N times, then `Outer.Contains(Other)`. Measured for N = 2,000, 4,000 and 8,000: 0.64 s, 1.9 s and 7.4 s, four times per doubling. N = 100,000 builds in a few seconds and then spends about 20 minutes in the one statement, N = 1,000,000 (inside the collection cap) about 30 hours. The runner reports the test failed 35 s in, and the interpreter thread runs on at full speed until the statement ends, with each such run adding a thread. Confirmed with `r13_scratch_quadratic_contains_timing` in records_tests.rs, projections from the measured growth.
- fix: check the cancel flag inside `search_list`'s element loop and inside `compare_in_order` (a thread local or an `Arc<AtomicBool>` the context sets, since `compare` has no context), so a cancelled test ends within one element comparison, and have `run_watched` set the flag when it stops waiting. Pin a `Contains` that the cancel flag ends.
- status: open

