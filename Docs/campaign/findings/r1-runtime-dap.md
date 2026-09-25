# R1 review: Runtime & DAP (al-runtime, al-test, al-test-harness, al-dap)

Adversarial read-only review, 2026-09-21. Baseline: AUDIT-BACKLOG.md section "Runtime & DAP"
(2026-07-31). Findings already fixed in current code are not repeated. Items from that audit
that are still open are tagged [STILL-OPEN].

## Coverage

- [x] AUDIT-BACKLOG.md "Runtime & DAP" baseline
- [x] al-runtime/src/interpreter/value.rs
- [x] al-runtime/src/interpreter/scope.rs
- [x] al-runtime/src/interpreter/eval_expr.rs
- [x] al-runtime/src/interpreter/eval_stmt.rs
- [x] al-runtime/src/interpreter/dispatch.rs
- [x] al-runtime/src/interpreter/records.rs
- [x] al-runtime/src/interpreter/coverage.rs
- [x] al-runtime/src/mock/record.rs
- [x] al-runtime/src/mock/filter.rs
- [x] al-runtime/src/stubs/mod.rs + library_assert.rs
- [x] al-test/src/router.rs
- [x] al-test/src/mutate.rs (score accounting, variant validation)
- [x] al-test/src/backends/interp.rs (pass/fail decision, lifecycle)
- [x] al-test/src/output/cobertura.rs
- [x] al-dap/src/dap/native_dap.rs (stack frames, paging, pending breakpoints)
- [x] al-dap/src/dap/bc_debug/events.rs (stop reasons)
- [x] al-dap/src/dap/framing.rs
- [ ] al-runtime/src/mock/calcformula_parser.rs — not read
- [ ] al-runtime/src/stubs/library_random.rs, library_variable_storage.rs, any.rs — not read
- [ ] al-test/src/backends/live_bc.rs + snapshot.rs — not read
- [ ] al-test/src/test_runner.rs + session.rs + persistence.rs — not read
- [ ] al-test/src/output/junit.rs — not read
- [ ] al-dap/src/dap/bc_debug/session.rs + session_config.rs + wire.rs + rest.rs — not read
- [ ] al-dap/src/native_debug.rs — not read
- [ ] al-test-harness/src/lib.rs + protocol.rs — not read

### Audit-backlog items confirmed fixed (not re-reported)

String-literal unescaping, compound record-field assignment, DateTime/Duration arithmetic,
`coerce_into_slot` i32 narrowing, CASE label error propagation, assignment to an unbound
identifier, the missing global builtin catalog, `asserterror` error capture, `Format`'s length
and format arguments, `render_value` Date rendering, `substitute_placeholders` re-scanning,
`CopyStr` past the end, per-record-variable views, statement-position `Get`/`Find*`,
`SetFilter` placeholder ordering, unset-field typed zeros, caseless `Code` primary keys,
FlowField Boolean constants, filter case-sensitivity, quoted empty filter tokens, set-literal
parse memoization, `DeleteAll` complexity, the router's bare-global safe-list, the DAP
`initialized`/pending-breakpoint queue, stack-frame line off-by-one and `StatementSpan.From`,
`startFrame`/`levels` paging, Break stop reasons, and the `configurationDone` retry.

## Findings

### [BUG] Option and Enum table fields have no typed zero, so an unset enum field is `Empty` everywhere
- where: crates/al-runtime/src/interpreter/value.rs:353 (`default_for`), consumed at crates/al-runtime/src/interpreter/records.rs:297 and crates/al-runtime/src/mock/record.rs:127 (`zero_like`)
- severity: high
- scenario: `Value::default_for` maps integer/decimal/boolean/text/code/date/time/datetime/duration/guid/char and nothing else, so a field declared `field(2; Status; Enum "My State")` (or `Option`) never lands in `RecordStore::field_defaults`. `read_buffer_field` then returns `Value::Empty` for a row that never assigned `Status`, and `zero_like` returns `None` for an `Option` bound so `field_cmp(Empty, Option{Open,0})` is `None`.
  Local pass / BC fail:
  ```al
  Rec.Init(); Rec."No." := 'A'; Rec.Insert();   // Status never assigned
  Rec.Reset();
  Rec.SetRange(Status, Rec.Status::Open);       // Open has ordinal 0
  Assert.IsTrue(Rec.IsEmpty(), 'no open rows'); // local: passes. BC: fails, the row IS Open
  ```
  The mirror case is a false failure: `Assert.AreEqual(Rec.Status::Open, Rec.Status, '')` on that row compares `Option` against `Empty` in `values_equal` (variant index 13 vs 1) and reports not-equal, where BC reports equal.
- fix: record the declared option/enum type per field when parsing the table (`parse_field_def` already captures `type_text`) and store `Value::Option { type_name, member: <ordinal-0 member>, ordinal: 0 }` as the field default; additionally give `zero_like` an `Option` arm returning ordinal 0 so an `Empty` cell still matches an ordinal-0 bound when no default was recoverable.
- status: fixed 84144137

### [BUG] SetFilter rejects Date, Time, DateTime and Option placeholder values
- where: crates/al-runtime/src/interpreter/records.rs:1746 (`render_filter_value`), called from records.rs:879
- severity: high
- scenario: `render_filter_value` handles only Integer/BigInteger/Decimal/Text/Code/Boolean and returns `Err` for everything else, so two of the most common AL filter idioms die at runtime:
  ```al
  Rec.SetFilter("Posting Date", '%1..%2', StartDate, EndDate);
  Rec.SetFilter(Status, '%1|%2', Rec.Status::Open, Rec.Status::Released);
  ```
  Both raise `SetFilter: placeholder value type Date is not supported by the local record runtime`. The test router has no way to see the argument's runtime type, so these tests are routed to `Interp` and fail locally although they pass on BC.
- fix: extend `render_filter_value` with `Date`/`Time`/`DateTime` (rendered as the day/ms carrier, matching `value_to_filter_string` in filter.rs so the parsed bound compares through the `cmp_value` Date arm) and `Option` (rendered as the ordinal, since BC filters options by ordinal). Add a round-trip test asserting `SetFilter(F, '%1..%2', D1, D2)` selects the rows `SetRange(F, D1, D2)` selects.
- status: fixed ef9c11e5. `filter::cmp_value` gained the `Option` arm, and `pattern_matches` now
  answers an option cell to both its member name and its ordinal.

### [BUG] Insert after Init with no primary-key assignment errors instead of inserting a blank key
- where: crates/al-runtime/src/mock/record.rs:312 (`current_primary_key`) via record.rs:383 (`insert_in`)
- severity: medium
- scenario: `current_primary_key` maps each PK field through `view.current.get(&f)` and returns `RecordError::MissingKeyField(f)` when the buffer has no entry. `init_in` clears the buffer, so:
  ```al
  Rec.Init();
  Rec.Description := 'blank key row';
  Rec.Insert();      // local: "Insert: primary key field 1 has no value in current row"
  ```
  BC inserts a row whose `Code`/`Text` key is `''` (or `0` for an Integer key) and only rejects the *second* such insert as a duplicate. The local runtime turns a passing BC test into a failure.
- fix: have `current_primary_key` fall back to the field's declared zero value rather than erroring. The field defaults already exist one layer up in `RecordStore::field_defaults`; either pass them into `MockRecord` at construction or resolve the key in `records::run_record_method` before calling `insert_in`.
- status: fixed 3abe4a43 (passed into `MockRecord` through `with_field_defaults`)

### [BUG] Round uses banker's rounding where BC rounds midpoints away from zero
- where: crates/al-runtime/src/interpreter/dispatch.rs:1554, pinned by the test at dispatch.rs:2637
- severity: high
- scenario: `builtin_round` applies `RoundingStrategy::MidpointNearestEven`. Microsoft's `System.Round` reference states the `'='` direction as "Values of 5 or greater are rounded up. Values less than 5 are rounded down", which is round-half-away-from-zero, not round-half-to-even.
  ```al
  Assert.AreEqual(2, Round(2.5, 1), '');       // local: passes (banker's -> 2). BC: Round gives 3
  Assert.AreEqual(0.12, Round(0.125, 0.01), ''); // local: passes. BC: 0.13
  ```
  This is the most dangerous shape in the repo: a money-rounding test that goes green locally and red on the build agent. The unit test `round_uses_bankers_rounding_and_directions` currently locks the wrong behaviour in.
- fix: switch to `RoundingStrategy::MidpointAwayFromZero` and rewrite the unit test to the documented cases (`Round(2.5, 1) = 3`, `Round(1.5, 1) = 2`, `Round(-2.5, 1) = -3`). Keep `'<'` as floor and `'>'` as ceil.
- correction while fixing: `'<'` and `'>'` are not floor and ceil. The reference page rounds
  -1234.56789 to -1234.567 with `'<'` and to -1234.568 with `'>'`, so they move the magnitude.
  They are now `RoundingStrategy::ToZero` and `RoundingStrategy::AwayFromZero`.
- status: fixed 182c5c93

### [BUG] FlowField Min/Max ignore rows whose target field was never assigned
- where: crates/al-runtime/src/mock/record.rs:763 (`target_cells`) and record.rs:820 (`FlowAgg::Min | FlowAgg::Max`)
- severity: low
- scenario: `target_cells` is `matching.iter().filter_map(|row| target.and_then(|t| row.get(&t)))`, so a row inserted without ever assigning the aggregated field contributes nothing. For `Sum` that is the same answer BC gives (adding 0). For `Min` it is not: with rows `Amount = 5` and `Amount` unassigned, `CalcFields(MinAmount)` returns 5 locally and 0 on BC.
- fix: in the `Min`/`Max` arm, substitute the field's typed zero for rows in `matching` that have no cell for `target`, or pass the field default into `calc_flow`.
- status: fixed 3abe4a43

### [BUG] Static Cobertura double-counts a procedure covered by more than one test
- where: crates/al-test/src/output/cobertura.rs:52 (`group_by_object`), used at cobertura.rs:165
- severity: medium
- scenario: `group_by_object` iterates `report.coverage` (one entry per test) and pushes a `ProcLine` for every `cov` it sees, with no dedupe. The top-level `<coverage line-rate>` is computed from a deduped `HashSet` at cobertura.rs:96, but the per-class rate at cobertura.rs:165 is `class.lines.iter().filter(hits > 0).count() / class.lines.len()`. A codeunit with one procedure reached by three tests and one untested procedure emits four `<line>` elements, three of them the same line number, and reports `line-rate="0.7500"` where the true figure is 0.5. The `<class>` and `<coverage>` rates disagree in the same document, and dashboards that aggregate per class get the inflated number.
- fix: dedupe by `(object, file, line)` in `group_by_object` the same way `write_cobertura` already dedupes for the overall rate, keeping `hits` as the number of covering tests rather than repeating the line.
- status: fixed ffb0807e

### [GAP] Dynamic Cobertura always reports line-rate 1.0, so a coverage gate on it is inert
- where: crates/al-test/src/output/cobertura.rs:225 and :285 (`lines-valid` set equal to `lines-covered`)
- severity: medium
- scenario: the interpreter records hits only, so the writer sets `line-rate="1.0"` and `lines-valid == lines-covered` whenever anything ran. A CI step that fails the build below, say, 80 percent line coverage passes unconditionally on this file. The doc comment discloses the limitation, but the emitted document does not: a consumer sees a valid Cobertura file claiming 100 percent.
- fix: the denominator is available — `al-analysis` already knows every statement line per file. Feed the executable-line set into `write_cobertura_dynamic` and emit real misses as `hits="0"`, or (cheaper) drop `line-rate`/`lines-valid` from the dynamic document and keep only `branch-rate` and the MC/DC attributes, which do have honest denominators.
- correction while fixing: `al-analysis` has no statement-line query, and the numerator is every
  node `eval_stmt` descends into, so reproducing the denominator outside the interpreter would
  mean mirroring that traversal. The second option was taken: `line-rate` and `lines-valid` are
  gone from the dynamic document, `lines-covered` stays, and `line-coverage="unavailable"` plus
  the leading comment say why and point a gate at `branch-rate` or `mcdc-rate`.
- still open: a real line-rate. It needs an executable-line set per file, which is a new query
  in `al-analysis` plus a parameter on `write_cobertura_dynamic` and its `al-lsp` caller.
- status: fixed ffb0807e

### [BUG] `Record "X" temporary` is routed to the local record runtime but its table name keeps the `temporary` keyword
- where: crates/al-runtime/src/interpreter/records.rs:1711 (`subtype_after_keyword`) via records.rs:1682, against crates/al-test/src/router.rs:1193 (`split_type_reference`, which strips `" temporary"`)
- severity: high
- scenario: the AL grammar puts `kw_temporary` inside `type_reference` (tree-sitter-al/grammar.js:681, pinned by crates/al-syntax/src/symbols.rs:1339 which asserts the declared type string is `Record "Sales Header" temporary`). `subtype_after_keyword` slices off the `Record` keyword and then calls `trim_matches('"')`, which strips the leading quote but not the trailing keyword, so the record's `table_name` becomes `Sales Header" temporary`.
  ```al
  var
      TempSalesLine: Record "Sales Line" temporary;
  begin
      TempSalesLine.Init();
      TempSalesLine."Line No." := 10000;
      TempSalesLine.Insert();
  end;
  ```
  The router strips `" temporary"` in `split_type_reference`, finds table `Sales Line` in the workspace and routes the test to `InterpRecord`. The runtime then calls `ensure_store("Sales Line\" temporary")`, `find_by_object_name` misses, and the test dies with `record table 'Sales Line" temporary' not found in workspace`. Every interp-routed test that declares a temporary record, which is most BC test code, fails locally with a message naming a table that does not exist.
- fix: strip a trailing `temporary` keyword in `subtype_after_keyword` (or read the subtype from the grammar's own child nodes rather than slicing text), and trim quotes from each end independently instead of `trim_matches('"')`. Beyond the name, a temporary record needs its own store: BC gives each temporary record variable a private in-memory table isolated from the physical one and from other temporary variables, while `ensure_store` keys only on the table name. Once the name is fixed, two `Record "X" temporary` variables would silently share rows with each other and with the persistent `Record "X"` — the classic expected-buffer-versus-actual-buffer test would then pass unconditionally. Key the store on `(table_name, temporary-instance-id)` and have the router keep routing to `LiveBc` until that lands.
- status: fixed abbb8e07. Stores are keyed by a `TableRef` (table name, plus the owning
  variable's handle when the declaration says `temporary`), so the router needs no change.
  Passing a temporary record by value copies the whole store, because a record passed without
  `var` is a copy of the variable and a temporary record variable holds the rows. A FlowField
  on a temporary record still aggregates the referenced table's persistent store.

### [BUG] A procedure that falls off the end returns its last statement's value instead of the return type's default
- where: crates/al-runtime/src/interpreter/dispatch.rs:748 (`Eval::Exit(v) => Eval::Normal(v)`, with the fall-through `other => other` carrying `eval_block`'s last value) and crates/al-runtime/src/interpreter/eval_stmt.rs:135 (`eval_block` returns `last`)
- severity: high
- scenario: the declared return type is never parsed (`collect_params` reads parameters only; nothing in dispatch.rs looks at the return type), so when a body ends without `exit(...)` the caller receives whatever the last statement evaluated to. Record methods return `Boolean`, so:
  ```al
  procedure TryCreate(): Boolean
  begin
      Rec.Init();
      Rec."No." := 'A';
      Rec.Insert();          // last statement, evaluates to Boolean(true)
  end;                       // no exit
  ...
  Assert.IsTrue(TryCreate(), 'creation succeeded');
  ```
  Local: `TryCreate` returns `true` and the assertion passes. BC: a function that falls off the end returns the return type's default, `false`, and the assertion fails. The inverse shape is a false failure: a body whose last statement is an assignment yields `Value::Empty`, so `if MyIntFunc() = 0 then` compares `Empty` against `Integer(0)` in `values_equal` and is false where BC says true, and `if MyBoolFunc() then` errors with "if condition must be Boolean, got Empty".
- fix: parse the procedure declaration's return type in `collect_params`' sibling (or a new `collect_return_type`), and in `dispatch_workspace_procedure` map a fall-through `Eval::Normal(_)` to `Value::default_for(<return type>)` (or `Value::Empty` for a procedure with no return type) rather than passing the body's last value out. Add a regression test for the `Rec.Insert()`-as-last-statement shape above.
- status: fixed c1ec0077

### [SLOP] `CallFrame::return_slot` is declared and initialised but never read or written
- where: crates/al-runtime/src/interpreter/scope.rs:28 and scope.rs:41
- severity: low
- scenario: the field is documented as "Slot for the procedure return value, populated on `exit(value)` or by assigning to the procedure name". `grep -rn return_slot crates/al-runtime/src` finds only the declaration and the `None` initialiser. `exit(value)` is carried by `Eval::Exit` instead, and assigning to the procedure name is not implemented at all — `eval_assignment` would reject it as an unbound identifier. The field misleads a reader into thinking a return path exists that does not.
- fix: delete the field and its doc comment, or implement the return-by-name path it describes as part of the return-type fix above.
- status: fixed c1ec0077 (field deleted, the named return value uses the frame's ordinary binding)

### [GAP] Assert.ExpectedError is absent, so the asserterror round trip still cannot run locally
- where: crates/al-runtime/src/stubs/library_assert.rs:147 (`resolve`), against crates/al-runtime/src/interpreter/eval_stmt.rs:705 (`ctx.last_error`) and dispatch.rs:519 (`getlasterrortext`)
- severity: medium
- scenario: `asserterror` now stores the caught `ErrorInfo` in `ctx.last_error` and `GetLastErrorText`/`ClearLastError` are implemented, specifically so the standard pattern works. But the `Library Assert` stub catalog resolves only `IsTrue`, `IsFalse`, `AreEqual`, `AreNotEqual`, `AreNearlyEqual` and `Fail`, so
  ```al
  asserterror CreateInvalidOrder();
  Assert.ExpectedError('The order must have a customer');
  ```
  still routes the whole test to `LiveBc` (router.rs:1116 promotes on `stubs::resolve(subtype, method) == None`). The error-path plumbing is finished but unreachable. The same applies to the other common members: `RecordIsEmpty`, `RecordIsNotEmpty`, `TableIsEmpty`, `IsSubstring`, `KnownFailure`.
- fix: add `expectederror` to `library_assert::resolve` as a stub that compares its argument against `ctx.last_error`. That needs the ctx, so either give `StubFn` a context parameter or special-case `ExpectedError` in `dispatch_call_scoped` alongside `getlasterrortext`.
- status: fixed 036b8227. `StubCatalog` gained a `context_members` list and the dispatcher runs
  those members itself; `stubs::is_supported` is what the router now asks. The behaviour is
  ported from the Library Assert codeunit in BCApps (substring match, the same two messages).
  `RecordIsEmpty`, `RecordIsNotEmpty` and `TableIsEmpty` need a RecordRef over the record store
  and stay routed to `LiveBc`.

### [BUG] Assertion failure messages print Date/Time/DateTime as raw day and millisecond carriers
- where: crates/al-runtime/src/stubs/library_assert.rs:136
- severity: low
- scenario: `render_value` in the assert stub has `Date(d) | Time(d) | DateTime(d) => d.to_string()`, so `Assert.AreEqual(20240701D, Rec."Posting Date", 'date')` fails with `expected 739068 but got 739069` instead of a date. `dispatch::render_value` (dispatch.rs:1987) was fixed to render these properly; this second copy was not.
- fix: call `crate::interpreter::dispatch::render_value` from the assert stub rather than keeping a second renderer, or at minimum apply the same `ymd_from_al_days` formatting.
- status: fixed 036b8227 (the date family and Option delegate; quoted strings stay)

### [GAP] Recursion is capped at 48 AL call frames, below what real AL algorithms use
- where: crates/al-runtime/src/interpreter/dispatch.rs:29 (`MAX_RECURSION_DEPTH`), enforced at dispatch.rs:564
- severity: low
- scenario: the cap exists to stay inside a 2 MiB thread stack, which is a sound reason, but 48 is shallow for AL code that recurses over data. A BOM explosion, a recursive chart-of-accounts total, or a `ProcessNode` walk over a 60-row hierarchy fails locally with `recursion depth exceeded` and passes on BC. The failure looks like a product bug rather than a runner limit.
- fix: raise the cap and buy the headroom explicitly — run the interpreter body on a thread created with `std::thread::Builder::stack_size` (the unit test at dispatch.rs:2522 already does exactly this with 16 MiB to test the guard), or set `thread_stack_size` on the runtime whose blocking pool `al-test/src/backends/interp.rs:219` spawns into. Also make the error text say it is a local runner limit and suggest re-running that test on live BC.
- status: fixed cd80eaa5. The backend spawns the body on a 64 MiB thread (`INTERP_STACK_BYTES`)
  and the cap is 512. `MAX_AST_DEPTH` and `MAX_EXPR_DEPTH` had to rise to 2560 with it: both
  counters are cumulative across calls, so at 256 a deep recursion reported a nesting error
  instead of the call limit.

### [BUG] Text[N] and Code[N] length limits are never enforced on assignment
- where: crates/al-runtime/src/interpreter/value.rs:332 (`coerce_into_slot`), crates/al-runtime/src/interpreter/records.rs:290-300 (field type parsing strips the `[N]` suffix), crates/al-runtime/src/interpreter/dispatch.rs:1083 (`declared_text_length`, which feeds `MaxStrLen` only)
- severity: medium
- scenario: `parse_table_meta` splits the declared field type on `[` and keeps only the base name, so a `field(2; "No."; Code[20])` records the default `Value::Code("")` and nothing else. `coerce_into_slot` uppercases a `Code` assignment but never checks length. The same holds for locals: `bind_declared_text_length` stores the capacity but only `MaxStrLen` reads it.
  ```al
  Rec."No." := 'THIS-CODE-IS-WAY-LONGER-THAN-TWENTY';   // Code[20]
  Rec.Insert();
  Rec.Get('THIS-CODE-IS-WAY-LONGER-THAN-TWENTY');
  Assert.AreEqual('THIS-CODE-IS-WAY-LONGER-THAN-TWENTY', Rec."No.", '');  // local: passes
  ```
  BC raises "The length of the string is 35, but it must be less than or equal to 20 characters" at the assignment. The mirror case is a false failure: `asserterror Rec.Validate("No.", TooLongCode)` finds no error locally and reports "asserterror: expected an error to be raised, but none was".
- fix: keep the parsed `[N]` capacity in `RecordStore::field_defaults` (as a parallel `field_lengths: HashMap<FieldNo, usize>`) and in `CallFrame::declared_text_lengths` for locals, and have `coerce_into_slot` take the capacity so an over-long `Text`/`Code` assignment returns `Err` the way the out-of-range `Integer` narrowing already does.
- also found while fixing: `parse_field_def` read only the first child of the type segment, so
  `Code[20]` arrived as `Code` and no field had a capacity to keep. A `Code` assignment is now
  trimmed as well, since a Code value's length excludes leading and trailing spaces.
- status: fixed d32104f8

## Review complete

Round 1 found 13 open items; the 2026-07-31 audit's Runtime and DAP list is otherwise fixed, and
the whole al-dap surface it flagged now has tests pinning the corrected behaviour.

1. `Round` uses banker's rounding; BC rounds midpoints away from zero, so `Round(2.5, 1)` is 2
   locally and 3 on BC, and a unit test pins the wrong answer. Money tests go green locally, red on BC.
2. A procedure that falls off the end returns its last statement's value, so `Rec.Insert()` as the
   last statement makes a `Boolean` function return true where BC returns the default false.
3. `Record "X" temporary` keeps the `temporary` keyword in the table name, so the router sends the
   test to the local record runtime and the runtime then cannot find table `X" temporary`.
4. Option and Enum fields have no typed zero: an unset enum reads as `Empty`, never matches an
   ordinal-0 filter, and never compares equal to its own ordinal-0 member.
5. `SetFilter` rejects Date and Option placeholder values outright, and `Text[N]`/`Code[N]` length
   limits are never enforced, both of which split local and BC results in opposite directions.

## Round 1 fixes

All 13 are fixed on branch `campaign/fix-r1-runtime-dap`, each with a test that fails before its
commit. Every BC rule the fixes rely on is cited in its commit body from learn.microsoft.com,
except `Assert.ExpectedError`, which is ported from the Library Assert codeunit in
microsoft/BCApps because the test libraries are not on Learn.

Two BC behaviours the fixes did not confirm from documentation, left routed to live BC:
`Assert.RecordIsEmpty`, `Assert.RecordIsNotEmpty` and `Assert.TableIsEmpty` (they need a
RecordRef over the record store), and a real `line-rate` for the dynamic Cobertura document.

Not caused by these fixes: `al-test-harness --test cancellation` fails its two tests
(`cancel_non_existent_id_is_silently_ignored`, `cancel_before_request_does_not_panic_server`)
with an LSP shutdown timeout. Both fail the same way at 433dd5f7, before any of this work, and
extending `AL_TEST_REQUEST_TIMEOUT_MS` to 60 s does not help.
