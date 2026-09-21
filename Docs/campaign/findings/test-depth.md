# Test depth pass, campaign/test-depth

Property tests, fuzzed inputs and coverage measurement across the workspace. Goal: bugs no
hand-written test would reach, plus durable tests left behind.

Branch: `campaign/test-depth`, cut from `campaign/2026-09-21` at 49917423.

Constraint from the campaign: other agents own `crates/al-analysis` and
`crates/al-lsp/src/server/daemon` non-test source. Bugs found there are recorded here with a
minimal input and the test is marked `#[ignore = "finding: ..."]` so this branch stays green.
Bugs in other crates are fixed with the test.

## Findings

### F1. `BraceStyle::SameLine` formatting is not idempotent — fixed

`crates/al-syntax/src/formatting.rs`. Status: fixed on this branch.

`apply_brace_style` ran as the last pass, after the indentation state machine. Merging a
stand-alone `{` onto the line above changes what that state machine sees on the line
(`if … then` stops being a single-statement opener once it ends in `{`, and a merged
`field(…) {` both opens a brace and closes a single-statement body). The file was therefore
left indented for the pre-merge layout, and the next format run moved it again. "Format on
save" with `brace_style = SameLine` shifted indentation on every save.

Minimal input, `FormatOptions { brace_style: SameLine, ..default() }`:

```
        if Rec.Status = Rec.Status::Posted then
                {
                    ApplicationArea = All;
```

pass 1 indents the body 8 spaces, pass 2 indents it 4.

Second minimal input, same option plus `tab_size: 1, insert_spaces: false`:

```
                if "No." = '' then
        field(1; "No."; Code[20])
        {
            trigger OnValidate()
```

pass 1 puts `trigger OnValidate()` at one tab, pass 2 at two.

Fix: run the brace merge as a pre-pass, before the indentation pass, so the indentation is
computed for the final line layout. The second run finds no stand-alone `{` left to merge, so
it is a no-op and the pipeline is idempotent by construction. `is_mergeable_brace_target`
additionally now refuses to merge onto a line ending in `;` or `:`, or on `then`, `do`, `of`,
`repeat`, `var`, `else`, which would have produced invalid AL.

Found by `crates/al-syntax/tests/property_formatting.rs`,
`fixtures::mutated_fixture_every_option_is_idempotent`, at ~1500 cases. Regression tests:
`formatting::tests::brace_style_does_not_merge_onto_a_single_statement_opener` and
`brace_style_does_not_merge_onto_a_statement_terminator`.

### F2. Document line numbering disagreed with LSP for five Unicode separators — fixed

`Cargo.toml` (workspace `ropey` dependency), surfacing in
`crates/al-source/src/documents.rs`. Status: fixed on this branch.

`DocumentStore` holds open documents in a ropey `Rope` and converts LSP `(line, character)`
pairs with `Rope::line` / `len_lines`. Ropey's default `unicode_lines` feature counts
U+000B (vertical tab), U+000C (form feed), U+0085 (NEL), U+2028 and U+2029 as line breaks.
LSP counts only `\n`, `\r\n` and a lone `\r`. A document containing any of those five
characters, in a string literal, a comment or pasted text, put the server one line ahead of
the client, and every position after it mapped to the wrong offset: incremental edits
landed in the wrong place and silently corrupted the buffer.

Minimal input, from `edits_agree_with_the_lsp_position_reference`: open `"\u{0b}\u{0b}"`,
apply one change replacing `(0,0)..(0,2)` with `"@"`. Expected `"@"`, got `"@\u{0b}"`,
because the store treated the first VT as ending line 0 and clamped the end position to
offset 1.

Second minimal input, from `replacing_a_range_with_its_own_text_round_trips`: open
`"\u{0b}\na"` and replace `(0,0)..(1,0)` with the text that range covers. The store put
line 1 at offset 1 instead of 2, so the round trip grew the document to `"\u{0b}\n\na"`.

Fix: `ropey = { version = "1", default-features = false, features = ["cr_lines", "simd"] }`.
`cr_lines` alone is exactly the LSP line-break definition. Regression tests:
`documents::tests::unicode_separators_are_not_line_breaks` and
`edit_spanning_a_vertical_tab_replaces_the_whole_range`.

### F3. `MockRecord` iterated `Code` keys in case-sensitive order — fixed

`crates/al-runtime/src/mock/record.rs`, `SortKey::key_of`. Status: fixed on this branch.

A BC `Code` cell is caseless. `MockRecord` already indexed rows under a caseless primary
key (`normalize_key_value` uppercases `Code`), but `SortKey::key_of` handed the raw cell to
the sort, so `FindSet` ordered by the ASCII bytes. Every upper-case code therefore came
before every lower-case one, which is not the order BC returns.

Minimal input: insert `'a'`, then insert `'AA'`, then `FindSet` + `Next`. The table yielded
`["AA", "a"]`; BC yields `["a", "AA"]`, since `'A' < 'AA'`.

The same line also left an `Integer` cell and a `Decimal` cell in one sort field ordered by
`Value`'s variant tag rather than by value: `1`, `1.5`, `2` came back as `1`, `2`, `1.5`.

Impact: an AL test that walks a Code-keyed table with `FindSet` + `repeat … until Next() = 0`
and asserts on order, or on the first record, gets a different answer offline than against a
live server.

Fix: `SortKey::key_of` normalises each cell through `normalize_key_value`, the same function
the key index uses. Regression tests: `record::tests::find_set_orders_code_keys_caselessly`
and `find_set_orders_integer_and_decimal_cells_by_value`.

### F4. Caseless `Code` comparison folded ASCII only, the key index folded Unicode — fixed

`crates/al-runtime/src/mock/record.rs` (`field_cmp`, `flow_value_eq`) and
`crates/al-runtime/src/mock/filter.rs` (`cmp_value`). Status: fixed on this branch.

One caselessness rule, two implementations. `normalize_key_value` folds with
`to_uppercase()` (full Unicode); the three comparison sites folded with
`to_ascii_uppercase()` / `eq_ignore_ascii_case`. A row whose `Code` key contains a non-ASCII
letter is filed under the folded key but compared unfolded, so a filter that brackets it
rejects it.

Minimal input: `SetRange(KEY, 'A', 'É')`, then insert `'é'`. `Count` returned 0 although the
row sits inside the range (the index filed it under `'É'`).

Reachable three ways: `SetRange` bounds (`field_cmp`), `SetFilter` ordered comparisons on a
`Code` field (`cmp_value`, e.g. `>=É`), and FlowField `CONST` equality (`flow_value_eq`).

Fix: all three fold with `to_uppercase()`, matching the index. Regression tests:
`record::tests::set_range_folds_non_ascii_code_like_the_key_index` and
`set_filter_folds_non_ascii_code_like_the_key_index`.

### Runtime properties that hold

- `Round` agrees with scaled-integer reference arithmetic at every direction (`=`, `<`, `>`),
  returns an exact multiple of the precision, never moves more than one precision step, is
  idempotent and sign-symmetric, and rejects a non-positive precision or an unknown
  direction. Checked at quotients up to 10^18, where 96-bit decimal division starts to bite.
  No defect found.
- The filter parser never panics, on arbitrary text or on strings built from its own
  operator alphabet, and neither does `matches`. `parse` is deterministic;
  `parse -> print -> parse` returns the same AST and the same accepted values; `<>x` is the
  complement of `x`; `a..b` is `>=a & <=b`; `|` is disjunction and `&` is conjunction.
  No defect found.
- Record `Insert`/`Get`/`Modify`/`Delete`/`DeleteAll`/`SetRange`/`SetFilter`/`FindSet`/`Next`
  match a `BTreeMap` model over sequences of up to 30 operations, after F3 and F4.
  The canonical `if FindSet then repeat Delete until Next() = 0` loop removes exactly the
  filtered rows and terminates, and `Next(n)` then `Next(-n)` returns to the same row.

### Not implemented, so not tested

`CalcDate` and `DateFormula` have no implementation in al-runtime (`supports_global_builtin`
explicitly excludes `CalcDate`), so the planned date-arithmetic round trips have nothing to
run against. `mock/calcformula_parser.rs` is the FlowField `CalcFormula` parser, a different
thing.

### Formatter properties that hold

At `PROPTEST_CASES=20000` over generated objects and mutated fixtures, with the fix in place:

- `format(format(x)) == format(x)` at the default options and at every option combination
- formatting a file that parses without ERROR nodes leaves it parsing without them
- formatting preserves the tree-sitter leaf token stream, modulo whitespace inside a token
- CRLF stays CRLF, LF stays LF, and no stray CR appears in LF output

## Coverage

(filled in during step 2)
