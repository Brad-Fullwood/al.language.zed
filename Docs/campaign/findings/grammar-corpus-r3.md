# Grammar corpus round 3: List, Dictionary and the constructs added since round 2

Scope: the `tree-sitter-al` repository on branch `campaign/gr-corpus-r3`, starting at a108400
(the GR2-4 merge), against the AL that the local test interpreter and the test router read now.
The inputs were `crates/al-runtime/src/interpreter/` and `crates/al-test/src/router/ast.rs` in
the superproject at 70384db2, and the list of constructs in the round brief.

Each construct has a corpus test in `test/corpus/`, with the field names in the expected tree.
Every corpus input was also compiled with the Microsoft AL compiler 17.0.34 (the `dotnet tool`
build) with no symbol packages. That reports missing tables and codeunits, which is expected,
and it reports syntax errors: none remain in the inputs.

Tools: tree-sitter CLI 0.26.9 (the pin in `.tree-sitter-cli-version`), alc 17.0.34.45391.

## Coverage

- [x] `tree-sitter generate` leaves the tree clean and `tree-sitter test` passes 103 of 103 at a108400
- [x] List and Dictionary: `test/corpus/collections.txt` (`List of [Text]`, `List of [List of [Integer]]` and `Dictionary of [Code[20], Decimal]` as locals and parameters, `AddRange` with a list and with values, `GetRange(1, 2)` and the var result form `GetRange(2, 1, Part)` with three `expression` nodes under `call:` as the router counts them, `RemoveRange`, `Reverse`, `LastIndexOf`, `foreach` over a List and over `Prices.Keys` as a `member_suffix`, `Prices.Get(ItemNo, Value)` as an `if` condition). The element types inside `[...]` are a `bracketed_block` of keyword nodes, and the interpreter and router read the `type:` text, so that shape is enough. One name in the brief is not AL: alc rejects `Key` as a variable name (`AL0519: 'Key' is not valid value in this context`), so the test uses `ItemNo`. The grammar accepts `Key: Code[20]` as a declaration with a `metadata_keyword` name, which is more permissive than alc and harmless.
- [x] glued sign operators: `test/corpus/sign_operators.txt` (`X := -1`, `X:=-1`, `A*-1`, `A - -1`, `2*-B`, `A/-2`, `A+-1`, `A--1`, `X+=-1`, `X -= -1`, `X<-1`, `X>-1`, `X<>-1`, `X=-1`, `X<=-1`, `[-5..-2]`, `SetRange(Amount, -100, 0)`, `exit(-1)`). Every sign is a `unary_operator` and every operator keeps its own token, `+=` and `-=` included. GR3-1.
- [x] variables named after keywords: `test/corpus/keyword_names.txt` (`Page`, `Report`, `Code`, `Value`, `Record`, `Codeunit`, `Enum`, `Label` and `Object` as globals, locals and parameters, assignments, `Report.Append(Page)`, `Report.Length` and `Page.ToUpper` without parentheses, `Enum` and `Record` as `for` counters, `Page[1] := 'x'`, `Code[2]`, `case Code of`). alc 17.0.34 accepts all nine names. The brief asks for a `name` or identifier node, and the tree gives one only for `Label` and `Object` and for a `for` counter (always `identifier`). In other expressions `Page`, `Report`, `Value`, `Codeunit` and `Enum` are `object_keyword` and `Code` and `Record` are `type_keyword`, and in declarations the names are `name_or_keyword` over `object_keyword`, `keyword` (`Code`, `Record`) or `metadata_keyword` (`Label`). The grammar cannot do better: `Page.RunModal(...)` on the object and `Page.ToUpper` on a Text variable are the same tokens, and only the symbol table tells them apart. That is the GR2-2 decision, and since 985d68a6 the interpreter reads `object_keyword` and `type_keyword` as names when a variable binds them. `al-explorer test-run` on a scratch test with the nine names passes, except for indexing. GR3-2.
- [x] case labels after GR2-4: `test/corpus/case_labels.txt` (`-1:`, `- 2:`, `-Level::Gold.AsInteger():`, `-5..-3:`, `-10..-6, 10..20:`, `Y := A -1;` as the statement of the arm before `else`, `'a'..'f':`, `'g', 'h':`, `Enum::Level::Bronze, Enum::Level::Silver:`, `Level::Gold:`, `else` followed by a statement and a semicolon, by `;`, by nothing, and after an arm with no semicolon). Each signed label is one `signed_case_label`, and a signed range is `(signed_case_label) (binary_operator) (unary_expression (unary_operator) ...)`. `al-explorer test-run` on a scratch test with these procedures passes 3 of 3, including `-5..-3`, `-10..-6`, both `-Level::X.AsInteger()` labels and the `else` with no semicolon, so the runtime already handles the negative range that `STATE.md` lists as needing an interpreter test. That Rust test is not written yet. The `case_statement` selector is the `value:` field, which the interpreter looks up as `subject` (see GR3-3).
- [x] table and codeunit shapes from rounds 8 and 9: `test/corpus/codeunit_shapes.txt` (a table with `OnValidate` in two fields and `[IntegrationEvent(true, false)] local procedure OnStamp`, a codeunit with `SingleInstance = true;` and `EventSubscriberInstance = Manual;` as `property_assignment` children of `body:` where `single_instance` and `manual_instance` look for them, a var section followed by `[TryFunction] procedure`, a subscriber with `var Sender: Record "Sales Header"`, `OnAfterDeleteEvent` subscribers on `Database::Customer` and on the bare ID `18`, `Cu := OtherCu`, `Clear(Cu)`, `BindSubscription(Cu)`, `Cu.TryCount(Total)` as an `if` condition). Each attribute is a named child of its `procedure_declaration`, and `kw_procedure` starts at the word `procedure` (the GR2-1 shape). alc reports only the tables and codeunits that are not in the test input. The brief's `Database::18` is not AL: alc 17.0.34 gives `AL0104: Syntax error, ',' expected` at the `18`, as for `Codeunit::50170` in GR2-5, and the grammar gives the same `(ERROR (integer))` confined to that argument that `test/corpus/recovery.txt` pins.
- [x] node kinds and field names the interpreter and router match: a scan of every string literal compared with a `kind()`, used in a `match` arm on a kind, or passed to `child_by_field_name` in `crates/al-runtime/src/interpreter/` (tests excluded) and `crates/al-test/src/router/ast.rs`, checked against `src/node-types.json` and the corpus trees. 73 matched kinds exist in the grammar. Before this round two had no corpus tree, `empty_if_statement` (eval_stmt.rs:93) and `scope_call_suffix` (eval_stmt.rs:735, :787, :1109), and one field had none, `alternative` (eval_stmt.rs:199), because the older `if then else` test in `statements.txt` has no field names. `test/corpus/interpreter_kinds.txt` now covers them: `if A then;`, `if A then X else;`, an `if` with `consequence:` and `alternative:`, `true` and `false` as names, and `Codeunit::"Scope Other"::DoIt()` as a `scope_suffix` then a `scope_call_suffix` (alc 17.0.34 parses that call and rejects it with `AL0151`, so no working AL reaches the interpreter's `scope_call_suffix` branches). Every matched kind and every matched grammar field now appears in a corpus tree. Fourteen matched kinds are not in the grammar at all, and three field lookups never find a node (`subject`, `variable`, and `target` on the missing `assignment_statement`): GR3-3.

## Findings

### [GR3-1] a sign glued to the operator before it lexes as part of that operator
- where: tree-sitter-al/grammar.js:1223 (`operator`, the first `token(...)` of the choice) and generator/tools/al-gen/templates/grammar.js.template:870, at a108400. The consumers are crates/al-runtime/src/interpreter/eval_expr.rs:974 (`assignment_kind` matches `:=`, `+=`, `-=`, `*=`, `/=` only) and :990 (`binary_precedence`).
- severity: medium
- scenario:
  ```al
  X:=-1;
  Y := A*-1;
  if X<-1 then
      X := 0;
  ```
  The regex `[!$%&*+\-:<=>?@^|~]+` takes any run of operator characters, so the tree has one `(binary_operator (operator))` for `:=-`, `*-` and `<-` and a bare `(integer)` after it. `X>-1`, `2*-B` and `A--1` do the same. alc 17.0.34 compiles all of them as the operator followed by a unary minus. `al-explorer test-run` on a scratch test codeunit fails three tests with `unsupported binary operator in expression: ':=-'`, `'*-'` and `'<-'`. The Microsoft corpus does not show it: its formatter puts a space before every sign, and no operator run in its 46,389 files lexes differently under the fixed regex.
- tree alc implies: `(binary_operator (operator))` for `:=`, then `(unary_expression (unary_operator) (unary_expression ... (integer)))`, the same tree as `X := -1`.
- fix: a `+` or `-` only starts an operator token, so the regex is `[!$%&*+\-:<=>?@^|~][!$%&*:<=>?@^|~]*` followed by the unchanged dot part. `+=` and `-=` stay one token because `=` may still follow the sign. Pinned by the two tests in `test/corpus/sign_operators.txt`, which fail on a108400. In the first test the trees of `X := -1` and `X:=-1` are the same, and the interpreter already runs the spaced form, so no runtime change is needed once the superproject pointer moves.
- status: fixed 23b6f92 (grammar)

### [GR3-2] indexing a variable named after an object or type keyword fails in the interpreter
- where: crates/al-runtime/src/interpreter/indexing.rs:40 (`indexed_variable` takes the primary only when it is a `name` node). Its callers are crates/al-runtime/src/interpreter/eval_expr.rs:579 (a read) and :837 (an assignment target).
- severity: low
- scenario:
  ```al
  Page := 'abc';
  Page[1] := 'x';
  Code := 'AB';
  Letter := Code[2];
  ```
  alc 17.0.34 compiles this with `Page: Text` and `Code: Code[10]`. The tree is `(postfix_expression (primary_expression (object_keyword)) (index_suffix index: (bracketed_block ...)))`, with `type_keyword` for `Code` (pinned by `test/corpus/keyword_names.txt` "parameters, loop counters, indexes and calls without parentheses on keyword names"). `indexed_variable` returns `None` for it, so `al-explorer test-run` fails the write with `assignment to unbound identifier 'page[1]'` and the read with `'Code[2]' is not an expression the local runtime can evaluate`. The same test with a variable named `Txt` passes. Every other use of the nine names in the scratch test runs.
- tree alc implies: the same tree. The grammar has no symbol table, so it cannot make `Page` a `name` here and keep `Page.RunModal(...)` an object reference.
- fix: in `indexed_variable`, accept `object_keyword` and `type_keyword` as well as `name`, as `chain.rs:77` already does.
- status: open

### [GR3-3] the interpreter and router match node kinds and field names the grammar does not produce
- where: crates/al-runtime/src/interpreter/eval_expr.rs:84, :91, :97, :102, :134 and :1240 (`integer_literal`, `decimal_literal`, `boolean_literal`, `string_literal`, `variable_reference`, and all of `eval_literal` at :231, reached only through `boolean_literal`). crates/al-runtime/src/interpreter/eval_stmt.rs:99 (`assignment_statement`, so `eval_assignment` at :609 and its `target` lookup at :616 do not run), :388 (field `variable`), :504 (field `subject`), :526 (`case_arm`), :735 (`member_access_expression`, `method_call_expression`, `call_expression`), :1172, :1183 and :1207 (`call_arguments`, `procedure_call_arguments`). crates/al-runtime/src/interpreter/dispatch/frames.rs:215 (`parameter_declaration`). crates/al-test/src/router/ast.rs:113 (`attribute_list`). Superproject at 35001880.
- severity: low
- scenario: none of these names is in `src/node-types.json`, and `subject` and `variable` are not grammar fields, so each test is always false and each of those lookups returns `None`. No wrong result follows today, because every site also matches the real name or falls back to a position: `eval_case` asks for `subject` and then takes the first child that is not a keyword, which is the `value:` expression, and `eval_foreach` asks for `variable` and then takes the `iterator:` node the same way (both fields are pinned in `test/corpus/case_labels.txt` and `test/corpus/collections.txt`). The dead names make the code read as if the grammar had these shapes. A later grammar change that puts another named child first in a `case` or `foreach` statement would silently pick the wrong node, where a field lookup on the real name would not.
- tree alc implies: not applicable. The kinds the grammar produces for these constructs are `integer`, `decimal`, `string`, `name`, `case_branch`, `argument_list`, `parameter` and `attribute`. AL has no boolean literal node: `true` and `false` are `(name (identifier))` (pinned in `test/corpus/interpreter_kinds.txt`), which the `identifier` branch at eval_expr.rs:134 handles by text.
- fix: read `value` in `eval_case` and `iterator` in `eval_foreach`, and delete the arms and alternatives for names the grammar does not have, including `eval_assignment` and `eval_literal`.
- status: open

## Corpus round complete

3 findings.

- medium 1: GR3-1 (fixed in grammar 23b6f92)
- low 2: GR3-2, GR3-3 (interpreter, open)

Six coverage items ticked, six corpus files added (3,882 lines of tests). Grammar gates on
142aba6: 119 of 119 corpus tests, 46,389 of 46,389 repository files, crate tests 20, generator
tests 16, package, wasm and drift clean. The agent died before writing this block, which the
orchestrator added from the coverage list and the gate logs.
