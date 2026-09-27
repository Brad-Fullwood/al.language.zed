# Grammar corpus round 2: the constructs the local interpreter runs

Scope: the `tree-sitter-al` submodule on its `campaign/2026-09-21` branch, starting at f211aef,
against the AL that the local test interpreter runs. The inputs were the AL sources embedded in
`crates/al-runtime/src` and `crates/al-test/src` (250 objects, extracted from the test strings),
the AL files under `crates/al-test-harness/data` (29 files), the pinned BCApps and
ALAppExtensions corpus (46,387 files), and the list of constructs in the round brief.

Each construct has a corpus test in `tree-sitter-al/test/corpus/`, with the field names in the
expected tree, so the shapes the Rust crates read (`child_by_field_name("name" | "value" |
"body" | "keyword" | "member" | "index" | "call")`) are pinned. Every corpus input was also
compiled with the Microsoft AL compiler 17.0.34 with no symbol packages, which reports syntax
errors only: none remain.

Tools: tree-sitter CLI 0.26.9 (the pin in `.tree-sitter-cli-version`), `al-explorer test-run`
from `target/debug` on a scratch project to check what the interpreter does with a shape.

## Coverage

- [x] `tree-sitter test` before any change: 59 of 59 pass
- [x] JSON types and methods: `test/corpus/json.txt` (JsonObject, JsonArray, JsonToken, JsonValue, ReadFrom, SelectToken with a var token, Add, Get, Replace, WriteTo, Contains, `AsValue().AsText()` and `AsObject().GetDecimal()` chains, `Items.Count() - 1` as a for bound)
- [x] labels: `test/corpus/labels.txt` (placeholders, Comment, Locked, MaxLength, all three on one label, a doubled quote in the text and in the comment, a local label passed to StrSubstNo and Error)
- [x] table code: `test/corpus/table_triggers.txt` (OnInsert, OnModify, OnDelete, OnRename, field OnValidate and OnLookup, Rec, xRec, CurrFieldNo, TableRelation with `where(... const(...), ... filter(A | B))`, a conditional TableRelation with `if`, `else if` and `else`, a codeunit declared after a table in one file)
- [x] events: `test/corpus/events.txt` (`[IntegrationEvent(true, false)]`, `[BusinessEvent(false)]`, `[InternalEvent(false)]`, raising an event, EventSubscriber with `Database::"Tour Member"` and `'OnAfterInsertEvent'`, with `Codeunit::Publisher`, with an element name, with a plain object id)
- [x] enums: `test/corpus/enums.txt` (declaration with quoted value names, `Enum "X"` variables, `Enum::"X"::Value`, `"X"::"Value"`, AsInteger, Names, Ordinals, `Enum::"X".FromInteger(1)`, `"X"::Value.AsInteger()`, enum case labels)
- [x] record methods: `test/corpus/record_methods.txt` (Init, Validate with two arguments, `Insert(true)`, `Modify(true)`, `Delete(true)`, SetRange, ModifyAll, `DeleteAll(true)`, Rename with two key values, TestField with and without a value, `Page.RunModal(0, Rec)`, `Page.RunModal(Page::"X", Rec) = Action::LookupOK`, bare field names and bare record methods in table code)
- [x] builtins: `test/corpus/builtins.txt` (TextBuilder, Guid, CreateGuid, IsNullGuid, `array[3] of Text`, `array[2, 3] of Integer`, `Txt[i]`, `Txt[i - 1]`, `Grid[1, 2]`, ArrayLen, Format with picture strings and a format number, CalcDate, Evaluate, StrSubstNo, `Database::"X"`, `Codeunit::"X"`, `Codeunit.Run`, `Report.Run`)
- [x] test codeunits: `test/corpus/test_codeunits.txt` (`Subtype = Test`, TestPermissions, `[Test]`, `[HandlerFunctions(...)]`, `[ConfirmHandler]`, asserterror on a call, on a block and on Error, GetLastErrorText, quoted identifiers with doubled quotes as object, procedure, variable and field names)
- [x] the attribute defect in `Docs/campaign/findings/grammar-attribute-after-var.md`: GR2-1
- [x] every extracted interpreter and router source parsed with the new parser: no ERROR or MISSING node except two sources that are broken on purpose (a string fragment in `crates/al-test/src/router/mod.rs:967` and `codeunit 50112 Broken` in the mutation tests) and GR2-5
- [x] a token check over every source above and the Microsoft corpus: every keyword token is one word (this is how GR2-1 shows up, since it leaves no ERROR node)
- [x] `tests/run_repo_tests.sh`, `cargo test --all-targets` and the generator tests in the grammar repository
- [x] the queries under `queries/` and `languages/al/*.scm` compile against the new parser with `tree-sitter query`, and `grammar.js`, `src/grammar.json`, `src/parser.c` and `src/node-types.json` are unchanged
- [x] `cargo test -p al-syntax` in the superproject on the new scanner

## Findings

### [GR2-1] an attributed member with a modifier after a global var section loses its attribute and its modifier
- where: tree-sitter-al/src/scanner.c `scanner_scan_var_attribute_marker` and its call in `tree_sitter_al_external_scanner_scan` (the same code is in generator/tools/al-gen/templates/scanner.c.template). Reported in `Docs/campaign/findings/grammar-attribute-after-var.md`.
- severity: high
- scenario: the example from that file reproduces at f211aef:
  ```al
  codeunit 50199 "Tick Counter"
  {
      var
          Calls: Integer;

      [EventSubscriber(ObjectType::Codeunit, Codeunit::Ticker, 'OnTick', '', false, false)]
      local procedure Count(var Seen: Integer)
      begin
      end;
  }
  ```
  ```
  (procedure_declaration [5, 4] - [8, 8]
    (kw_procedure [5, 4] - [6, 19])
    name: (name [6, 20] - [6, 25] ...
  ```
  One `kw_procedure` token runs from the `[` to the end of `procedure`, so the procedure has no `attribute` and no `member_modifier` child. The tree has no ERROR node, so the corpus parse rate (46,389 of 46,389) did not show it. It takes a global var section, an attribute and a modifier word (`local`, `internal`, `protected`) after the attribute. The marker lookahead reads over `[...]` and the next word, decides the attribute belongs to the member, and returns false, and the scan then carries on from the read-ahead position and lexes `procedure` as a keyword that starts at the `[`.
- fix: the patch from the report. The marker function records that it read past the `[`, and the scan returns false when it did and produced no marker, so tree-sitter lexes the `[` again. Applied to `src/scanner.c` and the template. Pinned by two tests in `test/corpus/attributes.txt`: "attributed local procedure after a global var section" and "attributed internal and local event publishers after a global var section". In the superproject, `subscriber_codeunits_with_globals_run_on_a_fresh_instance` (crates/al-runtime/src/interpreter/records_tests.rs:3811) can move its var section back above the subscriber once the submodule pointer moves.
- status: fixed 290ef3c (grammar)

### [GR2-2] variables and parameters named after type or object keywords parse as keyword nodes that the interpreter does not read
- where: tree-sitter-al/grammar.js `primary_expression` (accepts `object_keyword` and `type_keyword`) and the scanner, which returns those classes for `Value`, `Code`, `Text`, `Date`, `Time`, `Version`, `File`, `Page` and the other words in `data/keywords.json` "object" and "type". The consumer is crates/al-runtime/src/interpreter/eval_expr.rs:134 (the identifier branch matches `identifier`, `variable_reference` and `name` only) and :162 (every other kind is an error).
- severity: medium
- scenario:
  ```al
  procedure KeywordNamedLocal()
  var
      Value: Text;
      Result: Text;
  begin
      Value := 'abc';
      Result := Value;
  end;
  ```
  The declaration name is `(name_or_keyword (object_keyword))` and the read of `Value` is `(primary_expression (object_keyword))`. `al-explorer test-run` on a scratch project fails the test with `unsupported expression kind: object_keyword`, and the same test with `Code: Code[10]` fails with `unsupported expression kind: type_keyword`. The assignment to `Value` works, because the left side is read as text. A parameter named `Value` fails the same way. The grammar keeps this shape on purpose: `Page.RunModal`, `Codeunit.Run`, `Database::"X"` and `Enum::"X"` receivers are the same node kinds, and the highlight query colours them.
- fix: in the interpreter, evaluate an `object_keyword` or `type_keyword` under `primary_expression` as a name when the scope binds that text, and keep the current handling otherwise. Pinned in the grammar by `test/corpus/json.txt` "variable named Value reads as an object keyword".
- status: fixed 985d68a6. `eval_expr` reads such a node as a name when a variable, a clock builtin or a field of the implicit record binds it, which covers reads, assignment targets, `var` arguments, member calls and bare field names in table code. A method with no arguments called without parentheses (`Report.Length`) now runs, and the router no longer sends a test with a variable named `Page` or `Report` to live BC. Pinned by `variables_named_after_keywords_read_and_write` and the router test `variables_named_after_object_keywords_stay_local`. `Text` has no `Length` method, so the test reads `Report.Length` on a TextBuilder.

### [GR2-3] the interpreter does not evaluate a case label with a leading minus
- where: tree-sitter-al/grammar.js `case_label_expression` (a leading `-` label is one external `signed_case_label` token), crates/al-runtime/src/interpreter/eval_stmt.rs:565 (each label goes through `eval_expr`), crates/al-runtime/src/interpreter/eval_expr.rs:162 (no branch for `signed_case_label`)
- severity: medium
- scenario:
  ```al
  X := -1;
  case X of
      -1:
          Result := 1;
      else
          Result := 2;
  end;
  ```
  The label is `(case_label_expression (signed_case_label))`. `al-explorer test-run` fails the test with `unsupported expression kind: signed_case_label`. No crate reads the `signed_case_label` kind.
- fix: in the interpreter, evaluate a `signed_case_label` by parsing its text as an expression, the way `indexing.rs` evaluates the text of an index. Pinned in the grammar by `test/corpus/statements.txt` "case labels with a leading minus".
- status: fixed f2cec26c. `eval_expr` evaluates a `signed_case_label` through `indexing::eval_standalone_expression`, so `-1`, `-2.5` and `-Limit` labels match. Pinned by `case_labels_with_a_leading_minus_match`. A negative range label such as `-5..-2` still fails: the scanner makes `-5..` one `signed_case_label` token followed by a subtraction, and the runtime reports "'-5..' is not an expression". That is a scanner defect of the GR2-4 kind.

### [GR2-4] a minus followed by a space in a case label is an error, and `-X::Y` splits at the scope operator
- where: tree-sitter-al/src/scanner.c, the `SIGNED_CASE_LABEL` branch of `tree_sitter_al_external_scanner_scan`
- severity: low
- scenario:
  ```al
  case X of
      - 2:
          exit(2);
      -Level::Gold.AsInteger():
          exit(3);
  end;
  ```
  `- 2:` gives `(ERROR [7, 12] - [7, 13])` for the minus and a label `2`. `-Level::Gold.AsInteger()` gives `(signed_case_label)` for `-Level`, then `(binary_operator (operator))` for `::` and a unary expression for the rest. The AL compiler 17.0.34 accepts both. Neither form occurs in the Microsoft corpus, and the interpreter cannot run either (GR2-3). When the branch consumes only the `-`, the scan also goes on to read a word after it, which is the same kind of read-ahead as GR2-1.
- fix: after the `-`, skip blanks before the label text and read past `::`, and end the scan when only the `-` was consumed. Or give `case_label_expression` an optional unary minus and drop the external token, which also settles GR2-3.
- status: open

### [GR2-5] the interpreter binds an EventSubscriber only in the `Type::Name` form, and its own fixture uses `Codeunit::50170`, which is not AL
- where: crates/al-runtime/src/interpreter/dispatch/events.rs:256 (`publisher.split_once("::")?` drops a subscriber whose ObjectId is a plain integer), :110 (the lookup by ID, reached only through `Codeunit::<number>`), crates/al-runtime/src/interpreter/records_tests.rs:3162
- severity: medium
- scenario: the EventSubscriber attribute page on Microsoft Learn says of ObjectId: "You can specify the object by its ID (integer) or by its name using the syntax `<ObjectType>::<ObjectName>`". The fixture `EVENT_SUBSCRIBERS` has `[EventSubscriber(ObjectType::Codeunit, Codeunit::50170, 'OnBeforePost', '', false, false)]`. The AL compiler 17.0.34 rejects that with `AL0104: Syntax error, ',' expected` at the `50170`, and the grammar gives `(ERROR (integer))` inside the attribute argument list (pinned by `test/corpus/recovery.txt` "object id after a scope operator is an error confined to that argument"). The documented form is not bound: a publisher with two subscribers, `Codeunit::Publisher` adding 1 and `50101` adding 10, returns 1 in `al-explorer test-run`, where BC returns 11.
- fix: in `subscriber_binding`, take a bare integer as the object ID and the kind from the first argument (`ObjectType::Codeunit` is a codeunit, `ObjectType::Table` a table), and change the fixture to `50170`.
- status: fixed a59621d8. `subscriber_binding` takes a bare integer as the object ID with the kind from the first argument, and `EVENT_SUBSCRIBERS` uses `50170`. Pinned by `subscribers_bound_by_object_id_run` (a codeunit publisher with a `Codeunit::"Id Publisher"` subscriber adding 1 and a `50287` subscriber adding 10 returns 11, and a table subscriber bound by ID runs on Insert). The call graph already resolved the form through `name_numeric_subscriber_targets`, and the router follows it, pinned by `subscribers_bound_by_object_id_are_reached` in al-insight and in al-test. No persisted summary changed, so `SCHEMA_VERSION` stays 4. `get_events` in al-symbols reported the ID as the target name and now reports the publisher's name, pinned by `subscriber_bound_by_object_id_reports_the_publisher_name`.
