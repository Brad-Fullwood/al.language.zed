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
- [ ] al-test/src/output/cobertura.rs + junit.rs
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

### [BUG] FlowField Min/Max ignore rows whose target field was never assigned
- where: crates/al-runtime/src/mock/record.rs:763 (`target_cells`) and record.rs:820 (`FlowAgg::Min | FlowAgg::Max`)
- severity: low
- scenario: `target_cells` is `matching.iter().filter_map(|row| target.and_then(|t| row.get(&t)))`, so a row inserted without ever assigning the aggregated field contributes nothing. For `Sum` that is the same answer BC gives (adding 0). For `Min` it is not: with rows `Amount = 5` and `Amount` unassigned, `CalcFields(MinAmount)` returns 5 locally and 0 on BC.
- fix: in the `Min`/`Max` arm, substitute the field's typed zero for rows in `matching` that have no cell for `target`, or pass the field default into `calc_flow`.
- status: open
