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
- [ ] al-runtime/src/interpreter/dispatch.rs
- [ ] al-runtime/src/interpreter/records.rs
- [ ] al-runtime/src/interpreter/coverage.rs
- [ ] al-runtime/src/interpreter/mod.rs
- [x] al-runtime/src/mock/record.rs
- [x] al-runtime/src/mock/filter.rs
- [ ] al-runtime/src/mock/calcformula_parser.rs
- [ ] al-runtime/src/stubs/*.rs
- [ ] al-test/src/router.rs
- [ ] al-test/src/mutate.rs
- [ ] al-test/src/backends/interp.rs
- [ ] al-test/src/backends/live_bc.rs + snapshot.rs
- [x] al-test/src/output/cobertura.rs + junit.rs
- [ ] al-test/src/test_runner.rs + session.rs + persistence.rs
- [ ] al-dap/src/dap/native_dap.rs
- [ ] al-dap/src/dap/bc_debug/session.rs + session_config.rs
- [ ] al-dap/src/dap/bc_debug/events.rs + wire.rs + rest.rs
- [ ] al-dap/src/native_debug.rs
- [ ] al-dap/src/dap/client.rs + framing.rs + protocol.rs + types.rs
- [ ] al-test-harness/src/lib.rs + protocol.rs
- [ ] existing test coverage in the above (tests_records.rs, regression_tests.rs, tests_coverage.rs)

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
- status: open

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
- status: open

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
- status: open

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
- status: open

### [BUG] FlowField Min/Max ignore rows whose target field was never assigned
- where: crates/al-runtime/src/mock/record.rs:763 (`target_cells`) and record.rs:820 (`FlowAgg::Min | FlowAgg::Max`)
- severity: low
- scenario: `target_cells` is `matching.iter().filter_map(|row| target.and_then(|t| row.get(&t)))`, so a row inserted without ever assigning the aggregated field contributes nothing. For `Sum` that is the same answer BC gives (adding 0). For `Min` it is not: with rows `Amount = 5` and `Amount` unassigned, `CalcFields(MinAmount)` returns 5 locally and 0 on BC.
- fix: in the `Min`/`Max` arm, substitute the field's typed zero for rows in `matching` that have no cell for `target`, or pass the field default into `calc_flow`.
- status: open

### [BUG] Static Cobertura double-counts a procedure covered by more than one test
- where: crates/al-test/src/output/cobertura.rs:52 (`group_by_object`), used at cobertura.rs:165
- severity: medium
- scenario: `group_by_object` iterates `report.coverage` (one entry per test) and pushes a `ProcLine` for every `cov` it sees, with no dedupe. The top-level `<coverage line-rate>` is computed from a deduped `HashSet` at cobertura.rs:96, but the per-class rate at cobertura.rs:165 is `class.lines.iter().filter(hits > 0).count() / class.lines.len()`. A codeunit with one procedure reached by three tests and one untested procedure emits four `<line>` elements, three of them the same line number, and reports `line-rate="0.7500"` where the true figure is 0.5. The `<class>` and `<coverage>` rates disagree in the same document, and dashboards that aggregate per class get the inflated number.
- fix: dedupe by `(object, file, line)` in `group_by_object` the same way `write_cobertura` already dedupes for the overall rate, keeping `hits` as the number of covering tests rather than repeating the line.
- status: open

### [GAP] Dynamic Cobertura always reports line-rate 1.0, so a coverage gate on it is inert
- where: crates/al-test/src/output/cobertura.rs:225 and :285 (`lines-valid` set equal to `lines-covered`)
- severity: medium
- scenario: the interpreter records hits only, so the writer sets `line-rate="1.0"` and `lines-valid == lines-covered` whenever anything ran. A CI step that fails the build below, say, 80 percent line coverage passes unconditionally on this file. The doc comment discloses the limitation, but the emitted document does not: a consumer sees a valid Cobertura file claiming 100 percent.
- fix: the denominator is available — `al-analysis` already knows every statement line per file. Feed the executable-line set into `write_cobertura_dynamic` and emit real misses as `hits="0"`, or (cheaper) drop `line-rate`/`lines-valid` from the dynamic document and keep only `branch-rate` and the MC/DC attributes, which do have honest denominators.
- status: open
