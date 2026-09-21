# R1b review: Runtime & DAP, second pass (al-runtime, al-test, al-test-harness, al-dap)

Adversarial read-only review, 2026-09-21. Continues `r1-runtime-dap.md`: this pass covers every
item that file left unticked, plus the files its checklist does not name.

## Coverage

- [x] al-runtime/src/mock/calcformula_parser.rs
- [x] al-runtime/src/stubs/library_random.rs
- [x] al-runtime/src/stubs/library_variable_storage.rs
- [x] al-runtime/src/stubs/any.rs
- [ ] al-test/src/backends/live_bc.rs
- [ ] al-test/src/backends/snapshot.rs
- [ ] al-test/src/test_runner.rs
- [ ] al-test/src/session.rs
- [ ] al-test/src/persistence.rs
- [ ] al-test/src/output/junit.rs
- [ ] al-test/src/error.rs + result.rs + lib.rs
- [ ] al-dap/src/dap/bc_debug/session.rs
- [ ] al-dap/src/dap/bc_debug/session_config.rs
- [ ] al-dap/src/dap/bc_debug/wire.rs
- [ ] al-dap/src/dap/bc_debug/rest.rs
- [ ] al-dap/src/native_debug.rs
- [ ] al-dap/src/dap/client.rs + protocol.rs + types.rs + config.rs + json_util.rs
- [ ] al-test-harness/src/lib.rs + protocol.rs + bin/gen-zed-index.rs
- [ ] al-test-harness/tests/* (harness suites)
- [ ] al-runtime/src/test_support.rs

## Findings

### [BUG] An unquoted table name in a Sum/Min/Max/Average/Lookup CalcFormula fails to parse, and the error kills the whole table
- where: crates/al-runtime/src/mock/calcformula_parser.rs:143 (`read_name`) and :371 (`expect_char('.')`), surfaced at crates/al-runtime/src/interpreter/records.rs:555 and :279
- severity: medium
- scenario: `read_name` accepts `.` as part of an unquoted identifier, so it reads a dotted table-and-field reference as one name. For
  ```al
  field(50; TotalAmount; Decimal)
  {
      FieldClass = FlowField;
      CalcFormula = Sum(SalesDetail.Amount WHERE (DocNo = FIELD("No.")));
  }
  ```
  `read_name` returns `SalesDetail.Amount`, `expect_char('.')` then sees `W` and returns `UnexpectedToken`. `parse_field_calcformula` turns that into `Err`, `parse_field_def` propagates it, and `parse_table_meta` fails, so the table cannot be loaded at all. Every interp-routed test that touches that table dies with a parse message, while BC runs the test normally. Quoting the table name (`Sum("SalesDetail".Amount ...)`) hides the bug, which is why all 12 unit tests and the two lint fixtures use quoted names. `Count(SalesDetail)` happens to work because `Count` reads no field name, so the failure is silent until someone writes an aggregating FlowField.
- fix: stop treating `.` as an identifier character in `read_name`, or read the table name with a dedicated routine that stops at the first `.` when the formula type takes a field. Add a unit test parsing `Sum(SalesDetail.Amount)` and `Lookup(Item.UnitPrice)` with no quotes.
- status: open

### [BUG] `LibraryVariableStorage.DequeueText`/`PeekText` print Date, Time, DateTime and Duration as raw carriers
- where: crates/al-runtime/src/stubs/library_variable_storage.rs:398 (`format_value`, arms at :405-:408)
- severity: medium
- scenario: this is a third copy of the value renderer, alongside `dispatch::render_value` (fixed) and `library_assert::render_value` (flagged in round 1). `Value::Date(d) => d.to_string()` prints the AL day carrier, so
  ```al
  LibraryVariableStorage.Enqueue(WorkDate());
  ...
  Assert.AreEqual(Format(WorkDate()), LibraryVariableStorage.DequeueText(), 'posting date');
  ```
  compares `01/07/24` against `739068` and fails locally where BC passes. `Duration` is worse: BC formats a Duration as `2 hours 5 minutes`, this prints the millisecond count.
- fix: delete `format_value` and call `crate::interpreter::dispatch::render_value` so there is one renderer. Cover the Date and Duration round trip in a unit test.
- status: open

### [BUG] `Any.DecimalInRange` rejects the two-argument call when MaxValue is a Decimal
- where: crates/al-runtime/src/stubs/any.rs:59 (`decimal_in_range` match arms)
- severity: medium
- scenario: the AL signature is `DecimalInRange(MaxValue: Decimal; DecimalPlaces: Integer)`, so passing a Decimal is the declared case, but the two-argument arm matches only `[Value::Integer, Value::Integer]`. There is no `[Value::Decimal, Value::Integer]` arm, and the three-argument arms do not apply, so
  ```al
  MaxAmount := 100.0;
  Amount := Any.DecimalInRange(MaxAmount, 2);
  ```
  errors with `Any.DecimalInRange expects (Integer, Integer) or (Num, Num, Integer)`. The test fails locally and passes on BC. The three-argument forms already cover every Integer/Decimal mix, so the omission is in the two-argument form only.
- fix: add `[Value::Decimal(max), Value::Integer(places)] => decimal_in_range_impl(Decimal::ZERO, *max, *places)`, and accept `BigInteger` wherever `Integer` is accepted across this file and `library_random.rs`.
- status: open

### [SLOP] `Any.GetSeed` doc comment and its test both describe behaviour the code does not have
- where: crates/al-runtime/src/stubs/any.rs:255 (doc), :260 (`get_seed`), test at any.rs:580
- severity: low
- scenario: the doc says "the LCG *state* is not the same as the user-visible seed (the state is updated on every call). We return the raw state cast to i64". `set_seed` calls `set_lcg_seed(seed)` which assigns the cell directly and advances nothing, so `Any.SetSeed(999); Any.GetSeed()` returns exactly 999. The test comment repeats the wrong claim ("it won't equal 999 exactly") and then asserts only that the result is an `Integer`, which no implementation of `GetSeed` could fail.
- fix: correct the doc, and make the test assert `Value::Integer(999)` so the round trip is actually pinned.
- status: open

### [SLOP] `Any.AlphanumericText` doc claims a character set the code does not produce
- where: crates/al-runtime/src/stubs/any.rs:140-161
- severity: low
- scenario: the doc says the range is "0-9, a-f from GUIDs -> here extended to a-z for variety". The body uses `const HEX: &[u8] = b"0123456789abcdef"` only, so the output is hex. The code matches BC (the AL original strips separators out of GUIDs, giving hex); the comment invents an extension that does not exist and would be a divergence if someone implemented it.
- fix: delete the second and third paragraphs of the doc comment and say the output is lowercase hex, matching the GUID-derived original.
- status: open

### [BUG] `AssertFull`'s failure message says the queue is empty whatever it holds
- where: crates/al-runtime/src/stubs/library_variable_storage.rs:129 (`assert_full`), message at :134
- severity: low
- scenario: with 3 of 25 slots used, `LibraryVariableStorage.AssertFull()` fails with `AssertFull failed - queue has 3/25 items. Queue is empty.` The trailing sentence is a constant and contradicts the count in the same string, which sends the reader looking for a lost enqueue.
- fix: drop the constant sentence, or say "queue is not full".
- status: open

### [GAP] `Any.AlphabeticText`, `UnicodeText`, `AlphanumericText` and `Email` allocate whatever length the AL code asks for
- where: crates/al-runtime/src/stubs/any.rs:130, :158, :177, :200
- severity: low
- scenario: each builds `(0..length as usize).map(...).collect::<String>()` with no upper bound, so `Any.AlphabeticText(2000000000)` in a test tries to build a 2 GB string and either takes the runner down or swaps the machine. BC caps the result at the `Text` variable's declared length and errors instead. `library_random::rand_text` already clamps to 250 at line 131, so the guard exists one file over.
- fix: clamp the length the way `rand_text` does, and return an error above the cap rather than allocating.
- status: open
