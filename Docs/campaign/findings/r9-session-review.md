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

### [R9-RT-1] a table's globals start fresh on every trigger call, and the hand merge 6b4be394 sends such tables to the local runtime
- where: crates/al-runtime/src/interpreter/dispatch/table_code.rs:91-94 (`run_table_code` installs a fresh globals frame for every trigger and procedure call of a table), crates/al-test/src/router/mod.rs:601-621 (`classify_table_code` builds a `ProcedureLocation` with `single_instance_state: false`, so a table with globals routes `InterpRecord`), crates/al-test/src/router/tests.rs:1551-1660 (`label_globals_are_not_object_state` now asserts `InsertsCounted` routes `InterpRecord`; at 391bbaa2 it asserted `LiveBc` with "reachable table 'R8 Counted' has object-level state")
- severity: high
- scenario: Business Central keeps a table's global variables with the record variable. The Record.Reset page on Learn says Reset "clears any AL variables defined on its table definition", and the Record.ModifyAll page says that "by design, the global variables of the record instance being modified will be initialized to their default value during the ModifyAll method execution, independently of the value that was previously set", which is the one bulk method that resets them. The base application relies on this in `SetHideValidationDialog`, `SetInsertFromContact` and similar setters that a trigger reads later. A workspace table `Member` with `var HideDialog: Boolean`, `procedure SetHideDialog(Hide: Boolean)` and a field `Name` whose `OnValidate` runs `if not HideDialog then Error('dialog shown')`, and a test that runs `Member.SetHideDialog(true); Member.Validate(Name, 'x');`: BC passes, the local run fails with `dialog shown`. The other direction: `var Strict: Boolean`, `procedure SetStrict()`, `trigger OnInsert() begin if Strict then Error('strict insert'); end;` and a test `Member.SetStrict(); asserterror Member.Insert(true);`: BC passes, locally `Insert(true)` succeeds (`inserted: Strict was lost` in the scratch probe) and the asserterror fails. Both confirmed with scratch procedures in records_tests.rs (`GlobalsLost`, `HideDialogLost`). The router sends both tests to the local runtime: 391bbaa2 sent a table with a real global to live BC with the reason "reachable table 'R8 Counted' has object-level state", and the hand merge 6b4be394 dropped that rule and the `object_kind` field with it, and rewrote the test to expect `InterpRecord`, so tests that were red or green for the right reason on live BC now get the wrong answer locally. The `ModifyAll` case is right locally (scratch `ModifyAllResetsGlobals` gives `yS`: the trigger ran with reset globals and its own field write was saved).
- fix: give each record variable its own globals frame, keyed the way `TableRef` keys the record's view, bound on the variable's first trigger or procedure call and kept between calls (a copy by value or `Rec := Other` gives the copy its own frame, `Reset` and `Clear` drop it, `ModifyAll` and `DeleteAll` run each row with a fresh frame). Until that exists, restore the router rule: a table with a `regular_variable_declaration` global that a reachable trigger or procedure reads routes to live BC, with the object kind in the reason. Pin the `SetHideDialog` shape as a runtime test and the routing as a router test.
- status: open
