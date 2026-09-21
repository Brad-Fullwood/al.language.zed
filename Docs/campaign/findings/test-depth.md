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

### Formatter properties that hold

At `PROPTEST_CASES=20000` over generated objects and mutated fixtures, with the fix in place:

- `format(format(x)) == format(x)` at the default options and at every option combination
- formatting a file that parses without ERROR nodes leaves it parsing without them
- formatting preserves the tree-sitter leaf token stream, modulo whitespace inside a token
- CRLF stays CRLF, LF stays LF, and no stray CR appears in LF output

## Coverage

(filled in during step 2)
