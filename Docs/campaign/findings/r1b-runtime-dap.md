# R1b review: Runtime & DAP, second pass (al-runtime, al-test, al-test-harness, al-dap)

Adversarial read-only review, 2026-09-21. Continues `r1-runtime-dap.md`: this pass covers every
item that file left unticked, plus the files its checklist does not name.

## Coverage

- [x] al-runtime/src/mock/calcformula_parser.rs
- [x] al-runtime/src/stubs/library_random.rs
- [x] al-runtime/src/stubs/library_variable_storage.rs
- [x] al-runtime/src/stubs/any.rs
- [x] al-test/src/backends/live_bc.rs
- [x] al-test/src/backends/snapshot.rs
- [x] al-test/src/test_runner.rs
- [x] al-test/src/session.rs
- [x] al-test/src/persistence.rs
- [x] al-test/src/output/junit.rs
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

### [BUG] `--filter` drops a test whose name contains the trailing pattern twice, and the run still reports green
- where: crates/al-test/src/session.rs:51 (`method_name_matches`), consumed at crates/al-test/src/backends/live_bc.rs:239 and :246
- severity: high
- scenario: for a pattern with no trailing `*`, the matcher scans forward greedily with `find` and then requires `cursor == name.len()`. The first occurrence wins, so a later occurrence that would satisfy the anchor is never tried:
  ```
  method_name_matches("TestPostPost", "*Post")  // false
  method_name_matches("aaa", "*aa")             // false
  ```
  Both names do end with the pattern. In `LiveBcMode::run` a method that does not match is dropped from `expanded`, so the test is never sent to BC, never appears in any `CaseResult`, and `SessionComplete` counts it in neither `total` nor `failed`. `al test run --filter '*Post'` prints a green summary that silently omits `TestPostPost`. The unit tests at live_bc.rs:406-434 only exercise patterns whose chunk occurs once.
- fix: when the pattern has no trailing `*`, match the final chunk with `ends_with` against the remaining slice instead of the greedy `find`, and check that the match starts at or after `cursor`. Add the `("TestPostPost", "*Post")` and `("aaa", "*aa")` cases as regression tests.
- status: open

### [BUG] A group holding both a whole-codeunit and a single-method target runs the codeunit twice and double-counts it
- where: crates/al-test/src/backends/live_bc.rs:114 (`has_specific_methods`) and the loop at :117
- severity: medium
- scenario: `run` groups `TestId`s by codeunit and dedupes only exact `(codeunit_id, method_name)` pairs, so `[TestId{50100, None}, TestId{50100, Some("TestA")}]` survives as one group with `methods = [None, Some("TestA")]`. `has_specific_methods` is true, so the loop calls `run_codeunit` once per entry, and the `None` entry runs the *whole* codeunit. For a codeunit with five tests the run issues two BC calls, emits two `SuiteComplete` events, and `SessionComplete` reports `total = 6` with `TestA` counted twice. A client that sends the codeunit node and one of its method nodes in the same request (the obvious shape for "run selected" in a tree UI) hits this.
- fix: when any entry in a group is `Some`, drop the `None` entries (the whole-codeunit run is a superset), or expand the `None` entry through `list_methods` and merge before grouping.
- status: open

### [GAP] The parallel live-BC path spawns one task per codeunit with no concurrency cap
- where: crates/al-test/src/backends/live_bc.rs:287 (`JoinSet::spawn` inside the `for` loop)
- severity: medium
- scenario: `opts.parallel` with 200 test codeunits opens 200 simultaneous `POST /dev/tests/{id}/run` calls against one BC server instance, each holding a session. On-prem NST typically caps concurrent sessions well below that, so the surplus calls fail with a server error and `append_failed_case` reports them as test *failures* rather than as an infrastructure limit. The failures look like product regressions and are not reproducible on a rerun with fewer codeunits.
- fix: bound the fan-out (a `Semaphore`, or spawn in chunks) with a default around 4 to 8 and expose it in `RunOptions`, and keep a server-error result distinguishable from an assertion failure in the emitted `TestMethodResult`.
- status: open

### [BUG] A test name or failure message containing a control character makes the whole JUnit report unparseable
- where: crates/al-test/src/output/junit.rs:131 (`BytesText::new(body)`) and :121 (`push_attribute(("name", ...))`)
- severity: medium
- scenario: quick-xml escapes `& < > " '` and nothing else, so a byte such as `\u{0}`-`\u{8}`, `\u{B}`, `\u{C}` or `\u{E}`-`\u{1F}` is written raw. Those code points are not legal anywhere in an XML 1.0 document. An AL test that fails with a message built from binary or terminal data, for example
  ```al
  Error('bad payload: %1', Format(Blob.Read()));   // contains 0x00..0x1F
  ```
  produces a `<failure>` body that Jenkins, GitLab and the .NET JUnit readers reject for the entire file. The CI step then reports "no test results" rather than one failing test, which reads as a passing build in the configurations that do not fail on a missing report.
- fix: strip or replace the XML-1.0-illegal code points in both the failure body and the `name`/`classname` attributes before writing, and add a test that round-trips a message containing `\u{0}` and `\u{1b}` through `assert_well_formed_xml`.
- status: open

### [DEAD] Five entries in the SignalR invoke-timeout table name targets the client never invokes, and a test pins them
- where: crates/al-dap/src/dap/bc_debug/wire.rs:41 (`"Next" | "StepIn" | "StepOut" | "Continue" | "Break"`), test at wire.rs:241
- severity: low
- scenario: `grep -rn 'invoke("' crates/al-dap/src` lists every target the session ever sends: Attach, DebugAdapterConfigurationDone, AddBreakpoint, RemoveBreakpoint, UpdateBreakpoint, SetBreakpointResponse, StopDebugging, TerminateSession, GetStackTrace, GetVariables, ExpandGlobals, ExpandNode, GetWatchNode, GetSource and IsAlive. Stepping goes through `SetBreakpointResponse` (session.rs:789), so the 10-second arm is unreachable and the real step budget is the 30-second breakpoint bucket. `invoke_timeout_step_ops_are_short` asserts a property of strings no caller produces, and the sibling test `invoke_timeout_dead_entries_are_gone` at wire.rs:320 exists specifically to stop dead entries creeping back.
- fix: delete the arm and rewrite `invoke_timeout_step_ops_are_short` to assert the budget for `SetBreakpointResponse`, which is what a step actually waits on.
- status: open

### [SLOP] The `redact_connection_token` doc comment is attached to `validate_signalr_handshake_response`
- where: crates/al-dap/src/dap/bc_debug/wire.rs:175-179 and :219
- severity: low
- scenario: the module split left the redaction doc (lines 175 to 178) immediately above the handshake doc with no blank line, so rustdoc renders both paragraphs on `validate_signalr_handshake_response` and `redact_connection_token` at line 219 documents nothing. A reader of the handshake validator is told it replaces `connectionToken` with a placeholder.
- fix: move the four-line block back above `redact_connection_token`.
- status: open

### [BUG] A full completion channel stalls the single SignalR reader, so Break events stop arriving
- where: crates/al-dap/src/dap/bc_debug/session.rs:158 (`completion_tx.send(msg).await` inside `route_signalr_message`), called from the one reader task at session.rs:394
- severity: medium
- scenario: the comment at session.rs:37 promises that Break has "a *separate* dedicated channel" so a dropped push cannot leave the debugger stuck. That holds only while the reader keeps running. `route_signalr_message` awaits the bounded 32-slot `completion_tx` on the reader task, so once 32 type-3 messages are buffered with no `invoke` consuming them the reader blocks inside that await and routes nothing further, including Break, OnDetachedFromConnection and IsAlive. A BC hub that answers an invocation twice, or that replies to invocations the client already timed out on, reaches 32 unconsumed completions and the session goes silent with no error surfaced: the DAP client simply never receives another `stopped` event.
- fix: bound the wait, for example `try_send` with a warn and a drop for a completion whose invocation id is not the one currently in flight, or move the completion routing off the reader task so a full channel cannot block Break routing.
- status: open

### [DEAD] Snapshot capture's shutdown-failure handling can never run, because `stop()` swallows every error
- where: crates/al-test/src/backends/snapshot.rs:104-112 (the four-arm match) and the `CaptureAndShutdown` variant at snapshot.rs:88, against crates/al-dap/src/native_debug.rs:397 (`stop`) and crates/al-dap/src/dap/bc_debug/session.rs:810 (`stop_debugging`)
- severity: low
- scenario: `NativeDebugSession::stop` logs a warning and returns `Ok(())` unconditionally, and `stop_debugging` one layer down does the same, so `shutdown_result` is always `Ok`. Two of the four match arms and the whole `CaptureAndShutdown` error variant are unreachable, and the code reads as if a failed teardown discards a good snapshot when in fact a failed teardown is invisible to the caller. A BC session that refuses `StopDebugging` leaves an attached debugger on the server and the snapshot path reports complete success.
- fix: decide which behaviour is wanted. Either have `stop()` return the error, in which case keep the match but return the snapshot rather than dropping it on a teardown failure, or delete the two dead arms and `CaptureAndShutdown`.
- status: open

### [GAP] Snapshot capture rejects two breakpoints on the same line number when BC does not report the object
- where: crates/al-test/src/backends/snapshot.rs:191-210 (`candidates` filter and the `_ =>` arm)
- severity: low
- scenario: the candidate filter matches on `breakpoint.line == location.line` and only narrows by object when `current_object()` is `Some`. With breakpoints on line 42 of two different files and a stop whose object is unknown, `candidates` has two entries and the `_` arm returns `UnexpectedStop`, aborting a capture where both breakpoints were configured exactly as asked. The same filter would also mis-attribute the sample if it happened to pick one.
- fix: narrow by file as well when the object is unknown, using the stop's source path if the session exposes it, and fail with a message that says the object was unknown rather than that the stop was unconfigured.
- status: open
