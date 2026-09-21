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

### Emitter and package-reader properties that hold

`crates/al-emit/tests/property_roundtrip.rs`:

- a generated project (codeunit, table, enum, interface, with names carrying spaces, dots,
  parentheses and non-ASCII) packs to an `.app` that `al-symbols` reads back with the same
  object set, the same ids and the same app identity
- two builds of one project yield the same object set
- `checked_entry_name` accepts only names that stay inside the directory they are joined
  to, every accepted name has only `Component::Normal` parts, and `write_zip` stores an
  accepted name verbatim and refuses every rejected one

Two properties I wrote first were wrong about the emitter, not the other way round, and are
worth recording so the next reader does not re-raise them:

- an `interface` declaration carries no object id, so the emitter synthesises one. The id
  comparison excludes interfaces.
- `build_app_from_project` calls `random_package_guid()` per build, so an `.app` is
  deliberately not byte-reproducible. The property is on the object set instead.

`crates/al-symbols/tests/property_app_reader.rs`: `read_app_bytes` answers with a package or
an error, never a panic, for arbitrary bytes, for bytes carrying the NAVX magic, for bytes
carrying NAVX plus a ZIP local-header signature, for every truncation of the
`representative.app` benchmark fixture, and for the fixture with up to 8 bytes flipped.
Reading is deterministic. No defect found.

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

## For the orchestrator

### Proposed CI job for the property tests

Every property test file reads `PROPTEST_CASES` and defaults to 128 (32 in al-emit, whose
cases each write a project and run a full build). Measured wall-clock test time on this
machine, excluding compilation:

| Target | 256 cases | 4096 cases |
| --- | ---: | ---: |
| `al-syntax --test property_formatting` | 1.1 s | 18.3 s |
| `al-source --test property_positions` | 0.1 s | 0.8 s |
| `al-runtime` (3 files) | 0.5 s | 4.1 s |
| `al-symbols --test property_app_reader` | 0.4 s | 4.3 s |
| `al-emit --test property_roundtrip` | 2.8 s | 71.0 s |
| total | 4.9 s | 98.5 s |

Per-PR job, `PROPTEST_CASES=256`, about 5 seconds of test time on top of the compile the
workspace gate already pays:

```yaml
  property-tests:
    runs-on: ubuntu-latest
    env:
      PROPTEST_CASES: 256
      CARGO_INCREMENTAL: 0
      CARGO_PROFILE_TEST_DEBUG: 0
    steps:
      - uses: actions/checkout@v4
        with: { submodules: true }
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo test -p al-syntax --test property_formatting
      - run: cargo test -p al-source --test property_positions
      - run: cargo test -p al-runtime --test property_filter --test property_round --test property_record_model
      - run: cargo test -p al-symbols --test property_app_reader
      - run: cargo test -p al-emit --test property_roundtrip
```

Nightly profile, `PROPTEST_CASES=8192`, about 3.5 minutes: the same steps with the env value
changed and `PROPTEST_MAX_SHRINK_ITERS: 4096` so a nightly failure arrives already minimised.
Commit any `*.proptest-regressions` file a nightly run produces; the per-PR job then replays
it at no extra cost, because proptest runs a persisted regression before the random cases.

One caveat worth stating: proptest draws a fresh seed on every run, so the per-PR 256-case
job accumulates search across runs rather than testing the same 256 inputs each time. Of the
four bugs below, the 256-case budget would have found F2, F3 and F4 within a handful of cases.
F1 took about 900 and 1500 cases in the two runs that surfaced it, so it is the nightly
profile that earns its keep.

### Shortlist for `cargo mutants`

Files with high line coverage, where mutation testing answers the question coverage cannot:
are the assertions load-bearing, or do the tests merely execute the code? Ordered by how much
logic sits behind the coverage.

| File | Lines | Line coverage | Why it is worth mutating |
| --- | ---: | ---: | --- |
| `crates/al-syntax/src/formatting.rs` | 1845 | 98.2% | a state machine with many near-miss branches, and F1 showed a whole pass could be mis-ordered while the suite stayed green |
| `crates/al-syntax/src/sort.rs` | 864 | 99.0% | comparator and grouping logic, where an off-by-one or a flipped comparison still produces plausible output |
| `crates/al-runtime/src/mock/record.rs` | 1260 | 97.8% | F3 and F4 both lived in a line that every test executed and none asserted on |
| `crates/al-runtime/src/mock/filter.rs` | 593 | 97.8% | boundary conditions in range and wildcard matching |
| `crates/al-source/src/documents.rs` | 1160 | 95.2% | clamping and offset arithmetic, where F2 showed a wrong answer looks like a right one |
| `crates/al-syntax/src/lint.rs` | 1239 | 97.3% | one rule per branch, so a mutant that disables a rule should be caught by a named test |
| `crates/al-emit/src/method_id.rs` | 116 | 98.3% | reproduces alc's hash byte for byte, so any surviving mutant is a real gap |
| `crates/al-symbols/src/composition.rs` | 635 | 98.9% | package merge and override precedence |
| `crates/al-test/src/output/cobertura.rs` | 558 | 99.5% | a report format that is asserted mostly by snapshot, the classic place for vacuous coverage |
| `crates/al-bc/src/http_auth.rs` | 166 | 99.4% | small, security-relevant, fully covered |

Suggested first run, since `cargo mutants` is slow:

```bash
cargo mutants --in-place -p al-syntax --file crates/al-syntax/src/formatting.rs --file crates/al-syntax/src/sort.rs
cargo mutants --in-place -p al-runtime --file crates/al-runtime/src/mock/record.rs --file crates/al-runtime/src/mock/filter.rs
```

## Coverage

`cargo llvm-cov --summary-only`, one run per batch of crates, on this branch with the new
property tests in place (`PROPTEST_CASES=32`). Line coverage, not region coverage.

| Crate | Lines | Line coverage |
| --- | ---: | ---: |
| al-types | 223 | 77.1% |
| al-protocol | 1259 | 86.6% |
| al-source | 2822 | 94.3% |
| al-project | 2966 | 88.8% |
| al-workspace | 2616 | 89.8% |
| al-snapshot | 510 | 86.3% |
| al-syntax | 10721 | 93.0% |
| al-semantic | 1162 | 61.4% |
| al-emit | 6851 | 89.4% |
| al-symbols | 10797 | 87.4% |
| al-bc | 3276 | 96.8% |
| al-compile | 1140 | 83.0% |
| al-publish | 643 | 84.3% |
| al-insight | 5875 | 94.2% |
| al-runtime | 16059 | 90.4% |
| al-test | 9495 | 87.8% |
| al-dap | 8296 | 87.8% |

Workspace aggregate over the batches measured: about 89% of lines.

### The five least-covered files over 100 lines

| File | Lines | Line coverage | Reachable from user input |
| --- | ---: | ---: | --- |
| `crates/al-semantic/src/lifecycle.rs` | 107 | 0.0% | no, it drives the external .NET semantic host process |
| `crates/al-test/src/backends/snapshot.rs` | 209 | 18.2% | yes, it reads a user's recorded snapshot files |
| `crates/al-project/src/analyzers.rs` | 278 | 62.6% | yes, it resolves analyzer paths out of `app.json` and settings |
| `crates/al-semantic/src/bridge.rs` | 723 | 63.2% | no, it is the wire protocol to that same external host |
| `crates/al-symbols/src/oauth.rs` | 1260 | 73.0% | partly, but the uncovered half is network and OS-keystore calls |

Next by absolute uncovered lines, all user-input driven:
`crates/al-emit/src/verification.rs` (1616 lines, 76.1%, 387 uncovered),
`crates/al-dap/src/dap/native_dap.rs` (2803 lines, 77.7%, 626 uncovered),
`crates/al-runtime/src/interpreter/records.rs` (1480 lines, 73.0%, 400 uncovered).

Per-crate worst files, for the record:

- al-types: `profiler.rs` 0% (12 lines), `test_result.rs` 0% (27 lines), both under 100 lines
- al-protocol: `client.rs` 85.8%
- al-source: `parsing.rs` 88.2%, `file_index.rs` 94.1%
- al-project: `analyzers.rs` 62.6%, `toolchain.rs` 87.5%, `config.rs` 90.6%
- al-workspace: `semantic_lifecycle.rs` 80.2%, `doctor.rs` 81.0%
- al-snapshot: `diff.rs` 84.6%, `format.rs` 88.3%
- al-syntax: `navigation.rs` 86.1%, `type_resolver.rs` 88.2%, `language_data.rs` 88.3%
- al-semantic: `lifecycle.rs` 0%, `bridge.rs` 63.2%, `host.rs` 83.3%
- al-emit: `verification.rs` 76.1%, `project.rs` 85.9%
- al-symbols: `oauth.rs` 73.0%, `nuget.rs` 79.1%, `app_inspect.rs` 80.6%, `virtual_file.rs` 84.1%
- al-compile: `lib.rs` 83.0%
- al-publish: `lib.rs` 84.3%
- al-insight: `graph.rs` 93.5%, `discovery.rs` 93.9%
- al-runtime: `records.rs` 73.0%, `eval_stmt.rs` 81.3%, `library_variable_storage.rs` 82.5%
- al-test: `backends/snapshot.rs` 18.2%, `router.rs` 84.1%, `mutate.rs` 85.9%
- al-dap: `native_dap.rs` 77.7%, `bc_debug/session.rs` 84.0%

### Gap tests written

Two of the three worst user-input-reachable files now have tests. The third does not, for a
reason worth recording.

`crates/al-project/src/analyzers.rs`, 62.6% -> 92.8%. The new cases cover the builtin and
blank entries, absolute and directory explicit paths, a missing configured probing path
against a missing default root, a probing path naming the assembly directly or naming an
unrelated file, configured search order, the `packages/` fallback, symlink skipping, the
depth limit, version-key ordering, entry suffix and casing, `dedup_paths` and the
editor-extension directory matcher.

`crates/al-emit/src/verification.rs`, 76.1% -> 84.5%, and al-emit as a whole 89.4% -> 90.4%.
This was the largest block of uncovered user-input-driven logic in the workspace and almost
all of it was the diagnostic arms: the conditions were reachable, nothing triggered them.
`crates/al-emit/tests/verification_diagnostics.rs` writes a project to disk and reads the
codes back out of `build_verified_app_from_project`, covering the record and assignment
checks (ALN2401 to ALN2405), local procedure semantics (ALN2203, ALN2204, ALN2206 to
ALN2209), object identity (ALN1001 to ALN1003), duplicate members (ALN1105 and the ALN11xx
family) and the `app.json` checks (ALN01xx), plus a clean project that reports nothing and a
malformed `app.json` that yields no `.app`.

`crates/al-test/src/backends/snapshot.rs`, 18.2%, is left alone. The file is one async
function that drives a live Business Central debug session and test runner. Both are
concrete types (`NativeDebugSession`, `TestRunnerClient`) with no trait seam, so covering it
means restructuring non-test source in al-test to introduce one. That is a design change, not
a test-depth change, and it belongs in its own piece of work. The 18% it does have is the
error enum's `Display` impls. Recommendation for the orchestrator: put a trait behind
`NativeDebugSession` for the three calls this backend makes (`set_breakpoints`, `state`,
`current_object`), and the breakpoint grouping, the ambiguous-stop narrowing and the
timeout path all become testable without a server.

## Tests left behind

| File | What it checks |
| --- | --- |
| `crates/al-syntax/tests/algen/mod.rs` | grammar-driven AL generator plus fixture mutators, shared by the formatter properties |
| `crates/al-syntax/tests/property_formatting.rs` | format idempotence, parse cleanliness, token preservation, line endings, every option combination |
| `crates/al-syntax/tests/property_formatting.proptest-regressions` | the two F1 seeds, replayed before the random cases on every run |
| `crates/al-source/tests/property_positions.rs` | the document store against a reference implementation of the LSP position rules |
| `crates/al-runtime/tests/property_filter.rs` | filter parse, print, re-parse stability and the Boolean algebra of the operators |
| `crates/al-runtime/tests/property_round.rs` | `Round` against scaled-integer reference arithmetic |
| `crates/al-runtime/tests/property_record_model.rs` | record operations against a `BTreeMap` model, the canonical delete loop, `Next` reversibility |
| `crates/al-emit/tests/property_roundtrip.rs` | project to `.app` to symbol index round trip, archive entry-name containment |
| `crates/al-emit/tests/verification_diagnostics.rs` | every native verification diagnostic family, from a project on disk |
| `crates/al-symbols/tests/property_app_reader.rs` | `read_app_bytes` on arbitrary, NAVX-shaped, truncated and mutated bytes |

Unit regressions added alongside the fixes: two in `al-syntax::formatting::tests`, two in
`al-source::documents::tests`, four in `al-runtime::mock::record::tests`, and twenty in
`al-project::analyzers::tests`.

## Gates

On the tip of `campaign/test-depth`:

```
cargo fmt -p al-syntax -p al-source -p al-runtime -p al-emit -p al-symbols -p al-project -- --check
cargo clippy -p al-syntax -p al-source -p al-runtime -p al-emit -p al-symbols -p al-project --all-targets -- -D warnings
cargo test -p al-syntax -p al-source -p al-runtime -p al-emit -p al-symbols -p al-project --no-fail-fast
```

all clean. The workspace `ropey` feature change in F2 reaches every consumer, so
`al-workspace`, `al-analysis`, `al-insight` and `al-lsp` were run as well: 1137 and 743 tests
respectively, all passing.

## Test depth pass complete
