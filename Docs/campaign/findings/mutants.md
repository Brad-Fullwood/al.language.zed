# Mutation testing pass, campaign/test-mutants

`cargo mutants` over the shortlist in `test-depth.md`. Each missed mutant is a line that
the crate's own tests execute but do not check. The verdict per mutant is `test added
<sha>`, `equivalent <why>`, or `deferred <why>`.

Branch: `campaign/test-mutants`, cut from `campaign/2026-09-21` at fa4fcf8f.

## Real bugs

None found so far.

## Setup

- cargo-mutants 27.1.0, installed with `cargo install cargo-mutants --locked`.
- Every run uses
  `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3`
  and `cargo mutants --in-place -p <crate> --file <path>`. cargo-mutants 27 rejects
  `--jobs` together with `--in-place`, and `--in-place` runs one mutant at a time. The repository
  `.cargo/mutants.toml` excludes `build.rs`, `impl Debug`, `impl Display` and `fn fmt`.
- `-p <crate>` runs only that crate's tests. A mutant that another crate's tests would
  catch still counts as missed here.
- An `--in-place` run writes proptest seeds. When a mutant makes a property test fail,
  proptest appends the failing seed to the `.proptest-regressions` file next to the test,
  and the seed stays after cargo-mutants restores the source. The `sort.rs` run left three
  seeds in `crates/al-syntax/tests/property_formatting.proptest-regressions`. All three pass
  on the clean source (`cargo test -p al-syntax --test property_formatting`, at the default
  128 cases and at `PROPTEST_CASES=1`), so they were mutant artifacts and were discarded
  with `git checkout`. After every later run, `git status` is checked and any changed seed
  file is restored the same way. `PROPTEST_DISABLE_FAILURE_PERSISTENCE=1` would stop the
  writes, but it also stops proptest from replaying the committed seeds, so it is not used.

## Shortlist mapped to current paths

| Shortlist entry | Current path |
| --- | --- |
| `crates/al-syntax/src/formatting.rs` | `crates/al-syntax/src/formatting/{mod,casing,indent,options,passes,range,text}.rs`, split in c8f38561 |
| `crates/al-syntax/src/sort.rs` | unchanged |
| `crates/al-syntax/src/lint.rs` | unchanged |
| `crates/al-runtime/src/mock/record.rs` | unchanged |
| `crates/al-runtime/src/mock/filter.rs` | unchanged |
| `crates/al-source/src/documents.rs` | unchanged |
| `crates/al-symbols/src/composition.rs` | unchanged |
| `crates/al-emit/src/method_id.rs` | unchanged |
| `crates/al-test/src/output/cobertura.rs` | unchanged |
| `crates/al-bc/src/http_auth.rs` | unchanged |

## Summary

| File | Mutants | Caught | Missed | Unviable | Timeout |
| --- | ---: | ---: | ---: | ---: | ---: |
| `al-emit/src/method_id.rs` | 42 | 40 | 2 | 0 | 0 |
| `al-bc/src/http_auth.rs` | 26 | 21 | 3 | 2 | 0 |
| `al-syntax/src/sort.rs` | 107 | 78 | 20 | 1 | 8 |
| `al-source/src/documents.rs` | 147 | 76 | 29 | 42 | 0 |
| `al-runtime/src/mock/filter.rs` | 112 | 97 | 1 | 9 | 5 |

## Runs

### al-emit: `crates/al-emit/src/method_id.rs`

```bash
cargo mutants --in-place -p al-emit --file crates/al-emit/src/method_id.rs
```

42 mutants in 5 minutes: 40 caught, 2 missed, 0 unviable, 0 timeout.

`missed.txt`:

```text
crates/al-emit/src/method_id.rs:60:30: replace % with / in adjust_for_system_codeunits
crates/al-emit/src/method_id.rs:60:30: replace % with + in adjust_for_system_codeunits
```

- `method_id.rs:60`, `%` to `/`: test added f5e9269a. Every alc vector came from a user
  codeunit (id 50100), so the fold for system codeunits ran in no test. The new test uses
  four method ids from the `SymbolReference.json` of Microsoft's BC 28 System symbols
  package, which confirms the fold against alc output, plus a boundary test at
  2_000_000_000.
- `method_id.rs:60`, `%` to `+`: test added f5e9269a, same test.

Re-run with `--re adjust_for_system_codeunits`: 6 mutants, 6 caught.

### al-bc: `crates/al-bc/src/http_auth.rs`

```bash
cargo mutants --in-place -p al-bc --file crates/al-bc/src/http_auth.rs
```

26 mutants in 2 minutes: 21 caught, 3 missed, 2 unviable, 0 timeout.

`missed.txt`:

```text
crates/al-bc/src/http_auth.rs:87:5: replace warn_insecure_tls with ()
crates/al-bc/src/http_auth.rs:104:5: replace build_http_client -> Result<reqwest::Client, reqwest::Error> with Ok(Default::default())
crates/al-bc/src/http_auth.rs:137:27: replace && with || in apply_snapshot_auth
```

- `http_auth.rs:87`, `warn_insecure_tls` emptied: test added c13d58b5. The tests only
  checked the message text. The new test captures the `tracing` output of
  `build_http_client(true, …)` and checks for a WARN line naming the surface, and checks
  that `build_http_client(false, …)` logs nothing.
- `http_auth.rs:104`, `build_http_client` returns a default client: test added c13d58b5.
  The tests only checked `is_ok()`. The new test points a client with a 1 second timeout
  at a local socket that accepts and never answers, and expects a reqwest timeout error
  within 10 seconds.
- `http_auth.rs:137`, `&&` to `||`: test added c13d58b5. With a username and no password
  the mutant sends no `Authorization` header. The new test expects the bearer override for
  a username without a password and for a password without a username.

Re-run of the whole file after c13d58b5: 26 mutants, 24 caught, 2 unviable.

Not covered by any mutant: `danger_accept_invalid_certs(accept_invalid_certs)`.
cargo-mutants does not mutate a boolean argument, and checking it needs a TLS server with
a self-signed certificate.

### al-syntax: `crates/al-syntax/src/sort.rs`

```bash
cargo mutants --in-place -p al-syntax --file crates/al-syntax/src/sort.rs
```

107 mutants in 11 minutes: 78 caught, 20 missed, 1 unviable, 8 timeout. The 8 timeouts
are loops in `contains_begin_keyword` and `procedure_name_part` that stop advancing. A
timeout fails the test run, so they count as detected.

`missed.txt`:

```text
crates/al-syntax/src/sort.rs:99:43: replace || with && in object_body_spans
crates/al-syntax/src/sort.rs:228:5: replace is_pure_reordering -> bool with true
crates/al-syntax/src/sort.rs:280:31: replace > with < in split_into_members
crates/al-syntax/src/sort.rs:282:32: replace += with -= in split_into_members
crates/al-syntax/src/sort.rs:282:32: replace += with *= in split_into_members
crates/al-syntax/src/sort.rs:283:19: replace += with -= in split_into_members
crates/al-syntax/src/sort.rs:283:19: replace += with *= in split_into_members
crates/al-syntax/src/sort.rs:292:42: replace && with || in split_into_members
crates/al-syntax/src/sort.rs:308:32: replace += with -= in split_into_members
crates/al-syntax/src/sort.rs:308:32: replace += with *= in split_into_members
crates/al-syntax/src/sort.rs:314:19: delete ! in split_into_members
crates/al-syntax/src/sort.rs:330:15: replace += with -= in split_into_members
crates/al-syntax/src/sort.rs:330:15: replace += with *= in split_into_members
crates/al-syntax/src/sort.rs:346:9: replace || with && in is_plain_var_start
crates/al-syntax/src/sort.rs:354:9: replace || with && in is_var_start
crates/al-syntax/src/sort.rs:393:5: replace extract_member_name -> String with String::new()
crates/al-syntax/src/sort.rs:393:5: replace extract_member_name -> String with "xyzzy".into()
crates/al-syntax/src/sort.rs:399:35: replace || with && in extract_member_name
crates/al-syntax/src/sort.rs:399:28: replace == with != in extract_member_name
crates/al-syntax/src/sort.rs:409:35: replace || with && in extract_member_name_procedure
```

- `sort.rs:99`, `||` to `&&`: test added 43aeae8f (`declines_a_brace_that_shares_its_line`).
  No test had an object whose `{` or `}` shares a line with other code.
- `sort.rs:228`, `is_pure_reordering` returns true: test added 43aeae8f
  (`a_dropped_or_repeated_line_is_not_a_reordering`). The guard never fires on today's
  splitter, so only a direct test of the helper reaches it.
- `sort.rs:280`, `>` to `<`: test added 43aeae8f
  (`a_three_line_attribute_moves_with_its_procedure`). The existing multi-line attribute
  test put the attribute on the procedure that sorts first, so a detached attribute (sort
  key `""`) still landed right above it.
- `sort.rs:282`, `+=` to `-=` and `*=`: test added 43aeae8f, same test. `*=` needs an
  attribute of three lines or more.
- `sort.rs:283`, `+=` to `-=` and `*=`: equivalent. The line is an attribute continuation,
  which in AL that parses has no brace outside a literal, so the added count is 0, and
  `depth` is 0 there.
- `sort.rs:292`, `&&` to `||`: test added 43aeae8f
  (`a_trigger_inside_a_field_stays_in_its_field`). No sort test had a trigger nested in a
  field.
- `sort.rs:308`, `+=` to `-=` and `*=`: test added 43aeae8f
  (`a_three_line_attribute_moves_with_its_procedure`).
- `sort.rs:314`, `!` deleted: test added 43aeae8f
  (`a_comment_between_an_attribute_and_its_procedure_keeps_them_together`).
- `sort.rs:330`, `+=` to `-=`: equivalent. `depth` is only compared with 0, and negating
  every step keeps the zero crossings.
- `sort.rs:330`, `+=` to `*=`: test added 43aeae8f
  (`a_trigger_inside_a_field_stays_in_its_field`). `depth` stays 0 under this mutant.
- `sort.rs:346` and `sort.rs:354`, `||` to `&&`: test added 43aeae8f
  (`a_var_section_with_a_declaration_on_its_header_line_hoists`). Every var test put the
  declarations on the lines after `var`, and `var Counter: Integer;` on one line was not
  covered.
- `sort.rs:393`, `extract_member_name` returns a constant, and `sort.rs:399:28`, `==` to
  `!=`: test added 43aeae8f (`triggers_sort_alphabetically`). No test had two triggers, so
  trigger order was unchecked.
- `sort.rs:399:35`, `||` to `&&`: equivalent. The key becomes the rest of the line, such
  as `oninsert()`. Object triggers have unique names, and the next character after a name
  is `(` or whitespace, which sorts below every identifier character, so the order is the
  same.
- `sort.rs:409`, `||` to `&&`: test added 43aeae8f (`overloads_keep_their_source_order`).
  With the mutant the key includes the parameter list, and two overloads swap.

Re-run with `--iterate` after 43aeae8f, then `--re 'is_var_start|is_plain_var_start'`
after the var test was tightened: the 4 equivalent mutants above remain missed, every
other missed mutant is caught, and the 8 timeouts repeat.

### al-source: `crates/al-source/src/documents.rs`

```bash
cargo mutants --in-place -p al-source --file crates/al-source/src/documents.rs
```

147 mutants in 7 minutes: 76 caught, 29 missed, 42 unviable, 0 timeout. 39 of the
unviable mutants replace a function that returns `std::sync::Arc<…>` with `Arc::new(…)`
or `Mutex::new(…)`. The file names `Arc` and `Mutex` by full path, so the replacement does
not compile. The run wrote two seeds to
`crates/al-source/tests/property_positions.proptest-regressions`, which pass on the clean
source and were deleted.

`missed.txt`, with the 18 `memory_stats` operator mutants folded into one line:

```text
crates/al-source/src/documents.rs:183:9: replace DocumentStore::memory_stats -> DocumentStoreMemoryStats with Default::default()
crates/al-source/src/documents.rs:{193,194,197,202,203,206,211,212,219}: replace + with - and with * in DocumentStore::memory_stats
crates/al-source/src/documents.rs:296:24: replace > with >= in DocumentStore::validate_max_doc_bytes
crates/al-source/src/documents.rs:339:9: replace DocumentStore::validate_document_text -> Result<(), DocumentMutationError> with Ok(())
crates/al-source/src/documents.rs:397:34: replace += with -= in DocumentStore::replace_or_open
crates/al-source/src/documents.rs:397:34: replace += with *= in DocumentStore::replace_or_open
crates/al-source/src/documents.rs:517:9: replace DocumentStore::get_text_and_client_version -> Option<(std::sync::Arc<String>, i32)> with None
crates/al-source/src/documents.rs:532:9: replace DocumentStore::len -> usize with 0
crates/al-source/src/documents.rs:532:9: replace DocumentStore::len -> usize with 1
crates/al-source/src/documents.rs:536:9: replace DocumentStore::is_empty -> bool with true
crates/al-source/src/documents.rs:536:9: replace DocumentStore::is_empty -> bool with false
crates/al-source/src/documents.rs:550:9: replace DocumentStore::open_uris -> Vec<Url> with vec![]
```

- `documents.rs:183` and the 18 operator mutants in `memory_stats`: test added 055f1110
  (`memory_stats_counts_text_and_every_map_entry`). No al-source test called
  `memory_stats`. The test opens two documents, caches one tree and takes one parse lock,
  and checks every field against the per-entry sizes.
- `documents.rs:296`, `>` to `>=`: test added 055f1110
  (`prospective_cap_equal_to_an_open_document_is_accepted`). The existing test only had a
  document larger than the prospective cap.
- `documents.rs:339`, `validate_document_text` returns `Ok(())`: test added 055f1110
  (`validate_document_text_applies_the_cap_without_storing`). No test called it.
- `documents.rs:397`, `+=` to `-=` and `*=`: test added 055f1110
  (`replace_or_open_bumps_the_version_of_an_open_document`). No al-source test replaced an
  open document, so the version bump was unchecked. `*=` keeps the version at 0.
- `documents.rs:517`, `get_text_and_client_version` returns `None`: test added 055f1110
  (`get_text_and_client_version_reads_one_snapshot`).
- `documents.rs:532`, `536` and `550`, `len`, `is_empty` and `open_uris` return constants:
  test added 055f1110 (`len_is_empty_and_open_uris_follow_open_and_close`).

Re-run with `--iterate` after 055f1110: 29 mutants, 29 caught.

### al-runtime: `crates/al-runtime/src/mock/filter.rs`

```bash
cargo mutants --in-place -p al-runtime --file crates/al-runtime/src/mock/filter.rs
```

112 mutants in 13 minutes: 96 caught, 2 missed, 9 unviable, 5 timeout. The 5 timeouts are
loops in `Parser::remaining`, `Parser::peek` and `Parser::advance` that stop advancing. A
timeout fails the test run, so they count as detected. The 9 unviable mutants replace a
parser method that returns `Result<FilterExpr, FilterParseError>`, `Result<FilterAtom,
FilterParseError>`, `Result<Pattern, FilterParseError>`, `Result<OrderableValue,
FilterParseError>` or `Option<std::cmp::Ordering>` with `Ok(Default::default())` or
`Some(Default::default())`. None of `FilterExpr`, `FilterAtom`, `Pattern`, `OrderableValue`
or `std::cmp::Ordering` implement `Default`, so the replacement does not compile.

`missed.txt`:

```text
crates/al-runtime/src/mock/filter.rs:173:13: delete match arm None in Parser<'a>::parse_atom
crates/al-runtime/src/mock/filter.rs:380:9: delete match arm Value::Option{member, ..} in value_to_filter_string
```

- `filter.rs:173`, `None` arm deleted in `parse_atom`: equivalent. `peek()` returns `None`
  exactly when `at_end()` is true. The catch-all arm below calls `read_token`, which on an
  empty remainder also returns `FilterParseError::UnexpectedEnd` without moving `self.pos`.
  Deleting the direct return produces the same error through the same fallback path.
- `filter.rs:380`, `Value::Option { member, .. }` arm deleted in `value_to_filter_string`:
  test added c1e5bb57. The function's only caller already matches `Value::Option` and
  returns before reaching it, so the arm was dead from that call site. The function is
  still private-module API that the test module can call directly, and the member-name
  rendering is a real, documented behaviour, so a direct test pins it.

The `97dd3c56` test commit predates this record. It added `whitespace_around_operators_is_skipped`,
`scalar_cells_match_their_text_form`, `integer_cell_compares_with_a_decimal_bound`,
`option_cell_compares_by_ordinal` and `empty_cell_compares_as_zero_against_decimal_and_text_bounds`,
which is why the missed count above is already down to 2 mutants.

Re-run with `--iterate` after c1e5bb57: 4 mutants, 1 caught, 1 missed (the equivalent
`None` arm), 2 timeouts.

The run left proptest seeds in `crates/al-runtime/proptest-regressions/mock/filter.txt`
and `crates/al-runtime/tests/property_filter.proptest-regressions`. Both pass on clean
source (`cargo test -p al-runtime --lib mock::filter` and
`cargo test -p al-runtime --test property_filter`), so they were mutant artifacts and were
deleted.
