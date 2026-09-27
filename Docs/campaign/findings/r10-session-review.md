# R10 review: merges since the round 9 review

Scope: `git diff a0e85e0b..9e3f26a1 -- crates plugin` (49 files, 29 commits that are not
merges). That covers the round 8 last batch (merge 8f70b75e: a table publisher's `Sender`,
keyword-named variables, signed case labels, a subscriber bound by a bare object ID, call graph
edges for every declaration of a name, `SCHEMA_VERSION` 5), the round 7 fixes (merge fac24900),
the grammar pointer move to cc31863 (4c1b0ae6), the interactive session's work merged through
404f2fb4 and b2577ceb (List and Dictionary as reference types, List range methods, Dictionary key
order, `GetRange` routed live, `clippy.toml`, a `pack-native` refusal test) and the plugin round
5 fixes (merge c6f53a79). Read-only review of the tree at 9e3f26a1 in a worktree on
`campaign/r10-review`. Round 9's findings are not repeated.

Findings are added one at a time as they are confirmed. A claim that did not reproduce is noted
in its coverage line.

## Coverage

Scratch tests for every scenario below are saved, uncommitted, as
`.campaign/r10-scratch-tests.patch` in the main checkout. Every cargo command ran one crate at a
time.

- [ ] 1. Round 8 `fixed` statuses hold.
- [ ] 2. Round 7 security fixes (R7-SEC-1 to R7-SEC-8) do what their statuses say.
- [ ] 3. List and Dictionary as references (b2577ceb), and the `clippy.toml` entry.
- [ ] 4. Keyword-named variables and the router's declared-variables check.
- [ ] 5. Signed case labels and a negative range label.
- [ ] 6. A subscriber bound by a bare object ID, `get_events`, and call graph edges for same-named declarations.
- [ ] 7. Merge damage in every merge of `git log --merges a0e85e0b..9e3f26a1`.
- [ ] 8. Plugin round 5 fixes (c6f53a79) against the binaries.
- [ ] 9. Text in the diff against `~/.claude/CLAUDE.md`.

## Findings

### [R10-REF-1] variables declared on one line, and the elements of an array, share one List, Dictionary or JSON value
- where: crates/al-runtime/src/interpreter/dispatch/frames.rs:173-179 (`bind_structured_var_decl` builds one default and binds `default.clone()` to every name of the declaration), crates/al-runtime/src/interpreter/dispatch/frames.rs:342-351 (`bind_regular_var_decl` does the same with the default from `Value::default_for`), crates/al-runtime/src/interpreter/json.rs:1209-1212 (`default_for` mints the JSON handle once, so every clone points at one node), crates/al-runtime/src/interpreter/records.rs:3137-3146 (`default_for_array` fills the array with `vec![default; length]`)
- severity: high
- scenario: b2577ceb made a List and a Dictionary a `Shared` handle, and a50b536c (the R8-JSON-1 fix) made a JSON value a handle into the arena. Cloning either now shares the contents, and the binders still clone one default for every name. `A, B: List of [Integer]; A.Add(1); exit(B.Count())` returns 1 locally, where BC gives 0: each declared variable is its own list. The same holds for `A, B: Dictionary of [Integer, Integer]` (1), for codeunit globals `GA, GB: List of [Integer]` (1), for `A, B: JsonObject; A.Add('x', 1); B.WriteTo(T)` (`{"x":1}` where BC writes `{}`) and for `A: array[2] of JsonObject; A[1].Add('x', 1); A[2].WriteTo(T)` (`{"x":1}`). Confirmed with scratch procedures `MultiNameLocalLists`, `MultiNameLocalDicts`, `MultiNameGlobalLists`, `MultiNameJson` and `ArrayOfJson` in records_tests.rs. The test runner binds a test's locals through the same functions (`bind_procedure_locals`, al-test backends/interp.rs:704), so a `[Test]` procedure with `Expected, Actual: List of [Text]` compares a list with itself and passes whatever the code under test does. The runtime tests declare one variable per line, so none of them sees it.
- fix: build the default inside the loop over names (call `default_for_structured` or `Value::default_for` once per name), and build each array element on its own in `default_for_array`. Add the five procedures above as a test.
- status: open
