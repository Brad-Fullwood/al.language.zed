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
