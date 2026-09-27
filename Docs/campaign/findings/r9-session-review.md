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

### [R9-CU-1] a subscriber in a codeunit whose instance is running joins that instance instead of getting a fresh one
- where: crates/al-runtime/src/interpreter/dispatch/workspace_procedure.rs:424-436 (`globals_for_call` with no instance: `None if stack.has_object_globals(object_name) => Globals::OnStack`), crates/al-runtime/src/interpreter/dispatch/events.rs:212-218 (`raise` dispatches a subscriber with no instance, so it takes that arm)
- severity: low
- scenario: the EventSubscriberInstance page on Learn says that when the codeunit is not single instance, "each event subscriber will be run in its own codeunit instance. When an event gets raised, a codeunit instance is created for each event subscriber method that subscribes to the event. The individual codeunit instances are immediately disposed after its event subscriber is run." A codeunit `Self Sub` with `var Calls: Integer`, `procedure Do(): Integer` that calls `T.Tick()` twice on a publisher and exits `Calls`, and a subscriber to `OnTick` in the same codeunit that runs `Calls += 1`. `S.Do()` returns 0 on BC. Locally it returns 2 (scratch `SelfSubscriber`): the subscriber finds the running instance's globals frame on the stack and writes into it. The pinned test `subscriber_codeunits_with_globals_run_on_a_fresh_instance` raises the event from a third codeunit, so the subscriber's codeunit is never on the stack when the event fires. A codeunit that both publishes work through a helper and subscribes to that helper's events to collect results is the shape that hits this.
- fix: have `raise` ask for a fresh frame (a flag beside `pending_instance`, or a `Globals::Fresh` request) so `globals_for_call` gives a subscriber its own frame even when an instance of its codeunit is active, and keep `OnStack` for bare calls within the running instance.
- status: open

### [R9-PLUGIN-1] al-fetch-release.sh verifies regular files only, so a symlink or directory named al-lsp installs unverified
- where: plugin/scripts/al-fetch-release.sh:202 (`find . -type f` lists regular files only), :212-228 (only listed files are hashed), :230 (`have_pair` tests `-x`, which follows a symlink and is true for a directory), :235-241 (the install)
- severity: medium
- scenario: an archive whose `al-lsp` member is a symlink to `/bin/sh`, with a checksum listing that names only `al-explorer`, installs and reports "installed al-lsp and al-explorer v0.2.2 into .../bin, verified against binary-checksums.txt". The installed `al-lsp` is the symlink. An archive whose `al-lsp` member is a directory installs with the same report. Both reproduced against a local http server with `AL_RELEASE_BASE_URL`, `AL_ALLOW_INSECURE_RELEASE_URL=1` and an empty `CLAUDE_PLUGIN_DATA`. Under the threat model where the archive asset is replaced and the checksum asset is not, a regular file that is missing from the listing or does not match is refused, and a symlink is not checked at all: it can point `al-lsp` at any executable path on the host, and the session context says it was verified. It cannot bring attacker code (a symlink to a file inside the archive points at a regular file, which is hashed), so the gain is redirection to a host binary plus a false verification claim. Two smaller points in the same code: tar keeps the archive's mode bits, so the members are already executable in the stage directory when the digests are checked, and `have_pair` at :230 needs `-x` before the `chmod +x` at :235, so the chmod changes nothing (a member without the bit is refused as "missing al-explorer or al-lsp at its root"). The header's "nothing is made executable before its digest matches" holds for the install directory only.
- fix: after extraction refuse the archive unless every member is a regular file or a directory (`find . ! -type f ! -type d` prints nothing) and both binaries are regular files (`[ -f ] && ! [ -L ]`), and say so in the report. Word the header as "nothing reaches the install directory before its digest matches".
- status: open

### [R9-PLUGIN-2] al-fetch-release.sh reports "installed" when the install steps fail
- where: plugin/scripts/al-fetch-release.sh:237-243 (`mkdir -p`, `mv`, `rm -rf`, `mv` and the report run in sequence with no status check; `set -e` is off by design at :32)
- severity: low
- scenario: `CLAUDE_PLUGIN_DATA=/proc/version/nope` (a path that cannot be created) with a valid archive whose two members match the listing: the script prints "installed al-lsp and al-explorer v0.2.2 into /proc/version/nope/bin, verified against binary-checksums.txt", exits 0, and nothing is installed. Reproduced against the local server. That line reaches the session's context through al-session-context.sh, so the agent is told the binaries are in place. `rm -rf "${bin_dir:?}"` at :240 also runs when the `mv` into `$bin_dir.new` failed, so a directory holding one binary from an earlier attempt is removed and nothing replaces it.
- fix: chain the install steps with `&&` and report a refusal that names the failed step, and remove the old directory only after the new one is in place.
- status: open
