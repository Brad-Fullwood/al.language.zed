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

- [x] 1. Round 8 `fixed` statuses hold, except the codeunit half of R8-RT-3 (R10-EV-1) and the JSON declarations that R8-JSON-1's handle model now shares (R10-REF-1). Round 9 ran a scratch scenario for every fix up to a0e85e0b. Since then only records.rs (List and Dictionary methods, a `None` sender for the table and validate events) and json.rs (`Value::list` for `Keys` and `Values`) changed among the files those fixes touched, and the suites pass: al-runtime 634, al-test 174 (with the scratch router tests), al-insight 141. R8-RT-3, table publisher (95f1905b): a subscriber that takes `Sender` by value and writes `Note` leaves the caller's record as it was (`original`, scratch `ByValueSender`), and a publisher raised from `OnInsert` with `Insert(true)` gives the subscriber the record being inserted, whose write is saved with it (`from insert P2`, scratch `SenderFromTrigger`). R8-RT-3, codeunit publisher: the sender is a new instance (R10-EV-1). R8-CG-1 (4143be7a): `Validate(City)` on a table whose `Name` trigger runs `Rec.Modify()` and whose Modify subscriber opens a page routes `LiveBc`, since one node holds the edges of both triggers. The local runtime could run that test, so routing it live costs time and gives no wrong answer.
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
- scenario: b2577ceb made a List and a Dictionary a `Shared` handle, and a50b536c (the R8-JSON-1 fix) made a JSON value a handle into the arena. Cloning either now shares the contents, and the binders still clone one default for every name. `A, B: List of [Integer]; A.Add(1); exit(B.Count())` returns 1 locally, where BC gives 0: each declared variable is its own list. The same holds for `A, B: Dictionary of [Integer, Integer]` (1), for codeunit globals `GA, GB: List of [Integer]` (1), for `A, B: JsonObject; A.Add('x', 1); B.WriteTo(T)` (`{"x":1}` where BC writes `{}`) and for `A: array[2] of JsonObject; A[1].Add('x', 1); A[2].WriteTo(T)` (`{"x":1}`). Confirmed with scratch procedures `MultiNameLocalLists`, `MultiNameLocalDicts`, `MultiNameGlobalLists`, `MultiNameJson` and `ArrayOfJson` in records_tests.rs. The test runner binds a test's locals through the same functions (`bind_procedure_locals`, al-test backends/interp.rs:704), so a `[Test]` procedure that declares `Expected, Actual: List of [Text]` and compares their counts compares one list with itself. The runtime tests declare one variable per line, so none of them sees it.
- fix: build the default inside the loop over names (call `default_for_structured` or `Value::default_for` once per name), and build each array element on its own in `default_for_array`. Add the five procedures above as a test.
- status: open

### [R10-EV-1] a codeunit publisher's `sender` is a new instance, so a subscriber cannot read or change the publisher's state
- where: crates/al-runtime/src/interpreter/dispatch/workspace_procedure.rs:510-523 (`raise_published_event` builds the codeunit sender as `Value::Codeunit { instance: None }`), crates/al-runtime/src/interpreter/eval_stmt.rs:958-969 (a method call on a codeunit value with no instance gives it a fresh one)
- severity: medium
- scenario: the round 8 scenario for R8-RT-3 calls a `sender: Codeunit "X"` parameter "the documented way to reach the publisher's instance". A codeunit `R10 Cu Pub` with `var Counter: Integer`, `procedure Post()` that sets `Counter := 7` and calls `[IntegrationEvent(true, false)] local procedure OnPost()`, and `procedure GetCounter(): Integer`. A subscriber `OnPostSub(sender: Codeunit "R10 Cu Pub")` errors unless `sender.GetCounter()` is 7. A test that runs `Pub.Post()` fails locally with `sender counter 0` (scratch `CodeunitSender` in records_tests.rs), and the router routes it `Interp` with no reasons (scratch `r10_scratch_codeunit_sender_routing`). On BC the sender is the running publisher, so the test passes [UNVERIFIED on a live server; the EventSubscriberInstance and IncludeSender pages say the sender is the publisher object]. A subscriber that calls a setter on the sender is lost the same way. The sender went through `instance: None` before the codeunit instance work too (aa01cb97), but before 9a9ce662 an instance-less call joined the globals on the stack. Since 9a9ce662 the first call on the value mints a new instance. 95f1905b rewrote this code for the table publisher and kept `instance: None`. The pinned test `AddFromSender` reads `sender.Bonus()`, which returns the constant 100, so it cannot tell the instances apart.
- fix: give `raise_published_event` the running instance (the `instance` `dispatch_call_scoped` took from `pending_instance`, or the SingleInstance id) and build the sender with it, so `globals_for_call` answers `OnStack`. Pin a subscriber that reads and writes the publisher's global through `sender`.
- status: open

### [R10-REF-2] a TextBuilder is copied on assignment and when passed without `var`
- where: crates/al-runtime/src/interpreter/value.rs:145-146 (`Value::TextBuilder(String)` holds the text in the value), crates/al-runtime/src/interpreter/records.rs:2842-2850 (`dispatch_textbuilder_method` changes the string in the receiver's own slot)
- severity: medium
- scenario: the TextBuilder data type page on Learn says "The TextBuilder data type is a reference type, which holds a pointer elsewhere in memory", and the JSON type pages add that "all HTTP, JSON, TextBuilder, and XML types are reference types". `A.Append('x'); B := A; B.Append('y'); exit(A.ToText())` gives `x` locally and `xy` on BC. A helper that takes the builder without `var`, `local procedure AppendY(B: TextBuilder) begin B.Append('y'); end;`, called as `AppendY(A)`, leaves `A.ToText()` at `x` where BC gives `xy`. Confirmed with scratch procedures `TextBuilderAssigned` and `TextBuilderByValue` in records_tests.rs, and the router routes a test of the second shape `Interp` with no reasons (scratch `r10_scratch_reference_types_routing`). b2577ceb moved List and Dictionary to shared handles and left TextBuilder as it was, and the feature doc now says only List and Dictionary are references.
- fix: hold a TextBuilder's text in a `Shared<String>` as List does, create it per declared name (see R10-REF-1), and have `dispatch_textbuilder_method` lock it. Add the two procedures above as a test.
- status: open

### [R10-DICT-1] a Dictionary key takes the type of the argument, not the declared key type
- where: crates/al-runtime/src/interpreter/records.rs:2962-2985 (`dict_key` serialises the argument by its runtime type: a Text argument keeps its case, a Code argument is upper-cased, a Char gets a `C` prefix), crates/al-runtime/src/interpreter/records.rs:3123-3125 (`default_for_structured` keeps no key type for the dictionary), crates/al-runtime/Cargo.toml:23 and Docs/features/native-test-runtime.md:80 (the insertion order claim)
- severity: medium
- scenario: BC converts a key argument to the declared key type, as it does for any typed parameter. `D: Dictionary of [Code[20], Integer]; D.Add('abc', 1); D.ContainsKey('ABC')` is true on BC and the key reads back as `ABC`. Locally `ContainsKey` is false (scratch `CodeKeyFromText`). `C := 'xyz'` into a `Code[20]` variable, `D.Add(C, 1)`, then `D.ContainsKey('xyz')`: false locally, true on BC (scratch `CodeKeyFromCodeThenText`). The Learn example on the Dictionary data type page, `CountCharactersInCustomerName` with `counter: Dictionary of [Char, Integer]`, builds the right counts locally (`a=2;b=1;c=1`), but `counter.Get('a')` then fails with "Dictionary.Get: the key does not exist" and `counter.ContainsKey('a')` is false, since the Text literal `'a'` is keyed as `a` and the stored Char as `Ca` (scratch `LearnCharCounter`, `CharKeyFromIndex`). b2577ceb added Char and Option keys, which made the Char case reachable: before it a Char key was refused as unsupported. The router routes a test with the Code case `Interp` (scratch `r10_scratch_reference_types_routing`). Separately, the Cargo.toml comment says Dictionary keys keep insertion order "as in BC", and the feature doc says `Keys` keeps insertion order. The Dictionary page says the type "represents an unordered collection of keys and values". The pinned case in `KeysKeepTypeAndOrder` (add 10, 2, 7, remove 2, add 2, expect 10, 7, 2) is where .NET's `Dictionary<TKey, TValue>` reuses the freed slot and enumerates 10, 2, 7 [UNVERIFIED that BC's Dictionary is backed by it].
- fix: keep the declared key type with the dictionary value (parse it in `default_for_structured`) and convert every key argument to it before `dict_key`: Text to Code upper-cases, a one-character Text to Char, Integer to Decimal for a Decimal key. Keys then read back in the declared type. Word the order comments as the local runtime's choice ("the local runtime keeps insertion order, BC documents no order").
- status: open

### [R10-LIST-1] `RemoveRange` raises when its result is used, and `AddRange` flattens a list into a list of lists
- where: crates/al-runtime/src/interpreter/records.rs:2448-2454 (`removerange` returns an error for an invalid range whether or not the result is used), crates/al-runtime/src/interpreter/records.rs:2423-2427 (`addrange` with one List argument always adds the argument's elements)
- severity: low
- scenario: the List.RemoveRange page on Learn gives `[Ok := ] List.RemoveRange(Index, Count)` and says Ok is "true if the range is a valid range, otherwise false. If you omit this optional return value and the operation does not execute successfully, a runtime error will occur." `L.AddRange(1, 2); if not L.RemoveRange(5, 10) then exit('false');` fails locally with "List.RemoveRange: index 5 and count 10 are out of range for 2 elements", where BC returns `false` (scratch `RemoveRangeUsed`, routed `Interp` as `RemoveRangeResult`). The List data type page lists two overloads, `AddRange(T [, T,...])` and `AddRange(List of [T])`, and uses `List of [List of [Integer]]` in its deep copy example. With `Outer: List of [List of [Integer]]` and `Inner` holding 1, 2, 3, `Outer.AddRange(Inner)` matches the first overload (T is `List of [Integer]`) and adds one element on BC. Locally it adds three integers and `Outer.Count()` is 3 (scratch `AddRangeNested`).
- fix: read the statement marker in `dispatch_list_method` as `dispatch_json_method` does, and have `removerange` return false when its result is used. Expand a single List argument only when its elements are not themselves lists, or better, when the declared element type is not a List. Pin both.
- status: open

### [R10-KW-1] a record method called without parentheses is read as a field, and the router keeps the test local
- where: crates/al-runtime/src/interpreter/eval_expr.rs:556-562 (a `member_suffix` on a record receiver always goes to `field_get`), crates/al-runtime/src/interpreter/eval_expr.rs:563-574 (985d68a6's rule that runs a method written without parentheses, for receivers that are not records), crates/al-test/src/router/ast.rs (only call suffixes are classified, so `R.FindFirst` with no parentheses adds no reason)
- severity: low
- scenario: AL compiles a method call without parentheses. CodeCop AA0008 exists to warn about it ("Function calls should have parenthesis even if they do not have any parameters"), and its remark names `MyRecord.RecordId` as accepted property syntax. Code converted from C/AL writes `Rec.Insert;` and `if Rec.FindFirst then` this way. Locally `R.Insert;` fails with "field 'Insert' is not declared on workspace table 'R10 Keyword Table'", `if R.FindFirst then` with "field 'FindFirst' is not declared", and `exit(R.Count)` with "field 'Count' is not declared" (scratch `InsertStatementNoParens`, `FindFirstNoParens`, `DroppedParensOnRecord` in records_tests.rs). The router routes the `FindFirst` test `InterpRecord` (scratch `r10_scratch_dropped_parens_on_record_routing`), so a test green on BC is red locally. The record branch predates the reviewed commits. 985d68a6 added the rule for `Report.Length` and `Names.Count` and stopped at records.
- fix: in the record branch, when the table declares no field with that name and the name is a record method (`supports_record_method`), run it through `eval_call_parts` as the non-record branch does. A table field of the same name keeps precedence. Pin the three shapes.
- status: open
