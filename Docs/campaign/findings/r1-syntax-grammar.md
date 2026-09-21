# R1 review: al-syntax and tree-sitter-al

Adversarial read-only review of `crates/al-syntax`, the `tree-sitter-al` submodule
(grammar, scanner, queries, generator, bindings) and `languages/al/*.scm`.
Baseline: the 2026-07-31 audit in `AUDIT-BACKLOG.md` section "Syntax & Grammar".
Findings already fixed in current code are not repeated. Still-open audit items
are tagged `[STILL-OPEN]`.

## Coverage

- [x] AUDIT-BACKLOG.md "Syntax & Grammar" section
- [x] tree-sitter-al/grammar.js
- [x] tree-sitter-al/src/scanner.c
- [x] tree-sitter-al/queries/*.scm (7 files)
- [x] languages/al/*.scm + config.toml + semantic_token_rules.json + tasks.json
- [x] tree-sitter-al/bindings/rust
- [x] tree-sitter-al/generator/
- [x] tree-sitter-al/test/corpus + tests/ (59 corpus parses, all pass)
- [x] crates/al-syntax/src/lib.rs
- [x] crates/al-syntax/src/parser.rs
- [x] crates/al-syntax/src/tokens.rs
- [x] crates/al-syntax/src/type_resolver.rs
- [x] crates/al-syntax/src/symbols.rs
- [x] crates/al-syntax/src/formatting.rs
- [x] crates/al-syntax/src/sort.rs
- [x] crates/al-syntax/src/lint.rs
- [x] crates/al-syntax/src/navigation.rs
- [x] crates/al-syntax/src/complexity.rs
- [x] crates/al-syntax/src/folding.rs
- [x] crates/al-syntax/src/context.rs
- [x] crates/al-syntax/src/language_data.rs
- [x] crates/al-syntax/src/lexical.rs
- [x] crates/al-syntax/src/traversal.rs
- [x] crates/al-syntax/src/types.rs
- [x] crates/al-syntax/benches/parser.rs

## Findings

### Audit items re-checked and confirmed fixed

Verified against the current submodule (`tree-sitter-al` @ c754f82) with the
tree-sitter 0.26.9 CLI, so they are not repeated as findings below:
`object_declaration name:` field now populates (prec.dynamic at grammar.js:288),
so outline/highlights/locals `name:` patterns match; lowercase `0d`/`20240131d`
parse as `date_literal`; an unterminated `'` no longer swallows following lines
(the ERROR is confined to one line); `#define`/`#undef` update the scanner's
defines table (scanner.c:637-647) and are serialized; `directive` maps to
`PREPROCESSOR_KEYWORD` (tokens.rs:320); `op_and`/`op_or` dead-node matching in
complexity.rs is replaced by `operator_word` text matching with
`eq_ignore_ascii_case` plus tests; `clean_identifier_text` unescapes doubled
quotes; `TABLE_FIELD` is reachable via `classify_table_field_name`;
`TypeResolver` has a `line_starts` table and a per-scope memo, and globals /
`Rec` are scoped to the enclosing `object_declaration`; the `true|false`
highlight match is case-insensitive; `languages/al/*` is byte-identical to
`tree-sitter-al/queries/*` and to the al-gen `zed-language` templates (no drift);
`test/corpus/` now has 10 corpus files.

### [GAP] outline.scm's block-statement items are undocumented and unbounded
- where: languages/al/outline.scm:39-66 (identical in tree-sitter-al/queries/outline.scm)
- severity: low
- scenario: the last eight patterns capture `(begin_end_block (kw_begin) @name) @item`,
  `(if_statement (kw_if) @name) @item`, and the same for `case`, `for`, `foreach`,
  `while`, `repeat`, `with`. Every block in the file becomes an outline entry whose
  name is the bare keyword, so a 1000-line codeunit with 200 `begin` blocks and 150
  `if` statements adds ~350 rows named `begin`/`if`/`case` to the outline and to the
  fuzzy-symbol search that reads it. `Docs/features/language-assets.md:22` documents
  outline.scm as "objects, procedures/triggers, events, keys, enum values" only.
  symbols.rs:271-279 does the same thing for the LSP `documentSymbol` tree, where the
  entries at least nest under their procedure, so the two surfaces agree on behaviour
  but neither agrees with the doc.
- fix: decide whether block scopes belong in the outline. If they do, update
  `language-assets.md:22` and give them a name with content (the `if` condition text,
  the `case` selector) instead of the keyword. If they do not, drop the eight
  patterns and the `executable_scope_metadata` path in symbols.rs.
- status: open

### [GAP] AL-NL001 misses paren-less `FindFirst` / `FindLast`
- where: crates/al-syntax/src/lint.rs:352
- severity: low
- scenario: the check is `lower.contains(".findfirst()") || lower.contains(".findlast()")`.
  AL accepts an argument-less call without parentheses, so
  `repeat Rec2.SetRange(...); Rec2.FindFirst; until Rec.Next() = 0;` raises no
  AL-NL001 even though it is exactly the N+1 pattern the rule exists to catch.
  `Rec.FindFirst( )` (space between the parens) is missed too.
- fix: match on `.findfirst` / `.findlast` followed by end-of-token (`;`, whitespace,
  `(`, `)` or `t` of `then`), or drive the rule off `member_call_suffix` nodes instead
  of masked line text.
- status: fixed 01893af7

### [SIMPLIFY] `line_range` takes an unused `_line` parameter
- where: crates/al-syntax/src/lint.rs:117
- severity: low
- scenario: `fn line_range(text: &str, line_idx: usize, _line: &str) -> Range` never
  reads the third argument. Call sites pass either the real line (lint.rs:358) or `""`
  (lint.rs:397 and elsewhere), which reads as if the two cases behave differently.
- fix: delete the parameter and update the call sites.
- status: fixed 333faa60

### [BUG] `sort_members` moves members between objects in a multi-object file
- where: crates/al-syntax/src/sort.rs:22-31 and 194-277
- severity: high
- scenario: `body_open` is the first line whose trim is `{` and `body_close` is the
  *last* line whose trim is `}` (`rposition`), so for a file holding two objects the
  "body" spans both. In `split_into_members` the intervening `}` drives `depth` to -1
  and the second object's `{` brings it back to 0, so the second object's members are
  split as members of the first. Input:

  ```al
  codeunit 50100 A
  {
      procedure Zebra()
      begin
      end;
  }

  codeunit 50101 B
  {
      procedure Alpha()
      begin
      end;
  }
  ```

  Output (verified with a line-for-line simulation of `split_into_members` +
  `sort_members`): `Alpha` is emitted inside codeunit A above `Zebra`, and codeunit B
  is left with an empty body. `is_pure_reordering` (sort.rs:164) cannot catch it
  because the output is a permutation of the same lines. `sort_members_strict`
  (crates/al-lsp/.../organize.rs:228) only rejects sources with parse errors, and a
  two-object file parses cleanly, so the LSP `sortMembers` command and
  `al-explorer lsp sort-members --all` both write the corrupted file to disk.
- fix: bail out when the body contains more than one object. Cheapest check that
  matches the existing text-based design: count lines whose trim is exactly `{` at
  depth 0 (or count `object_declaration` children of the parsed root in
  `sort_members_strict`) and return `None` when it is not 1. The doc comment at
  sort.rs:13-14 already claims this behaviour.
- status: fixed 24ffeeea

### [TEST] No sort test covers a file with two objects
- where: crates/al-syntax/src/sort.rs:350-860 (test module)
- severity: medium
- scenario: the 20 tests cover var hoisting, attributes, CRLF, string braces and
  pure-reordering, but every fixture is a single object, so the corruption above ships
  green. `sorting_is_always_a_pure_reordering` (sort.rs:507) is the test that looks
  like it would catch it and cannot, because the bug *is* a pure reordering.
- fix: add a test asserting `sort_members` returns `None` for a two-object source, and
  one asserting each object keeps its own members when the guard is added.
- status: fixed 24ffeeea

### [PERF] `ts_range_to_syntax` rescans the file from byte 0 for every range
- where: crates/al-syntax/src/lib.rs:231-263 (`get_source_line`, `ts_range_to_syntax`)
- severity: medium
- scenario: `get_source_line` does `source.splitn(row + 2, |&b| b == b'\n').nth(row)`,
  which walks the file from byte 0 to the requested row on every call, and
  `ts_range_to_syntax` calls it once or twice per range. `extract_document_symbols`
  calls `ts_range_to_lsp` twice per symbol (symbols.rs has 32 call sites). Opening a
  10 000-line table with 800 field/variable symbols therefore walks roughly
  800 x 2 x 2 x 250 KB of source, and `textDocument/documentSymbol` runs on every
  open and after every debounce. The same helper backs folding.rs (one call per fold
  range) and the rename/binding/code-lens loops in al-analysis.
- fix: this crate already builds a line-start table twice; hoist it into a small
  `LineIndex` type and add `ts_range_to_syntax_with(range, source, &LineIndex)` for
  the loop callers, keeping the existing signature for one-off use.
- status: fixed caa16426

### [SIMPLIFY] `build_line_starts` is implemented twice, byte for byte
- where: crates/al-syntax/src/tokens.rs:179-189 and crates/al-syntax/src/type_resolver.rs:237-249
- severity: low
- scenario: both build `once(0).chain(source.iter().enumerate().filter(|(_, &b)| b == b'\n').map(|(i, _)| i + 1)).collect()`,
  and each has its own private `get_line`/`source_line` accessor with slightly
  different trimming (`tokens::get_line` keeps the `\n`,
  `TypeResolver::source_line` strips `\n` and `\r`). A third caller that needs one
  will add a third copy.
- fix: one `LineIndex` in lib.rs with both accessors, shared by tokens.rs,
  type_resolver.rs and the `ts_range_to_syntax` fix above.
- status: fixed caa16426

### [BUG] `clean_identifier_text` is bypassed by ~20 call sites that still use `trim_matches('"')`
- where: crates/al-syntax/src/navigation.rs:158,258,308,330,342,449;
  crates/al-syntax/src/symbols.rs:102,340,506,573,722,823,1142,1153;
  crates/al-syntax/src/type_resolver.rs:561,571,752,789,868,872
- severity: medium
- scenario: lib.rs:106 added `clean_identifier_text`, which strips exactly one quote
  pair and unescapes `""` -> `"`. `TypeResolver` name extraction goes through it
  (type_resolver.rs:914 -> lib.rs:95), but the al-analysis query layer reaches
  `resolve_type` with `node_clean_name` (al-analysis/src/queries/mod.rs:68-76), which
  is still `text.trim_matches('"')`. For

  ```al
  var
      "Cust ""Main"" Rec": Record Customer;
  ```

  (verified to parse as one `quoted_identifier` node), `VariableDecl.name` is
  `Cust "Main" Rec` while the lookup key is `Cust ""Main"" Rec`, so the
  `eq_ignore_ascii_case` in `resolve_type` (type_resolver.rs:111) never matches:
  hover shows no type, go-to-definition on the variable fails, and rename
  (rename.rs:65) falls through. `trim_matches` also strips quote *runs*, so
  `"Name"""` (the identifier `Name"`) is over-stripped to `Name` at every one of
  these sites.
- fix: route every one of these through `crate::clean_identifier_text` /
  `node_text_clean`, including `al-analysis`'s `node_clean_name`, and delete the
  ad-hoc `trim_matches('"')` calls.
- status: open

### [TEST] The incremental-parse benchmark never performs an edit
- where: crates/al-syntax/benches/parser.rs:97-121
- severity: medium
- scenario: `bench_parse_incremental` parses `SMALL_AL`, keeps the tree, then calls
  `parser.parse_incremental(SMALL_AL, &prev)` in the measurement loop. `Tree::edit`
  is never called and the text is byte-identical, so tree-sitter finds no changed
  ranges and reuses the entire old tree. The numbers measure tree reuse, not the
  keystroke path, while the comment on line 98 states it "models a single-character
  edit between calls". A regression in incremental re-lexing (the external scanner's
  serialize/deserialize, for instance) would not move this benchmark at all.
- fix: mutate the source (insert one character), call `prev.edit(&InputEdit{…})` with
  the matching byte/point deltas, then `parse_incremental` the edited text.
- status: fixed 886e00a6

### [GAP] Benchmarks cap out at 80 lines, below where the hot paths hurt
- where: crates/al-syntax/benches/parser.rs:15-77
- severity: low
- scenario: the two fixtures are 30 and 80 lines, and only `parse` and `format_al`
  are benchmarked. The costs identified above (`ts_range_to_syntax` per symbol,
  `extract_semantic_tokens` over a whole file) are linear or worse in file size and
  are invisible at 80 lines. Real AL files in the BC base app run to 10 000+ lines.
- fix: add a generated large fixture (say a table with 500 fields) and benchmark
  `extract_document_symbols`, `extract_semantic_tokens` and `lint` on it.
- status: fixed 886e00a6

### [BUG] `format_range` returns the caller's unclamped `end_line` in the edit
- where: crates/al-syntax/src/formatting.rs:477 and 536-542
- severity: low
- scenario: `end` is clamped to `orig_lines.len() - 1` at line 477 and the replacement
  text is computed from that clamped range, but the returned `FormatTextEdit` carries
  the raw `end_line` parameter together with `end_character` taken from the *clamped*
  last line. `format_range(text, 0, 9999)` on a 10-line buffer therefore yields an edit
  whose end position is `(9999, <len of line 9>)`. The sibling early-return at line 514
  correctly uses `end as u32 + 1`, so the two exits disagree about which value is the
  end of the edit.
- fix: return `end as u32` (and the matching `end_character`) in both branches.
- status: fixed 89643845

### [BUG][UNVERIFIED] scanner.c hand-rolls `strlen`/`memcpy` for wasm but calls `strcmp`/`strncmp` unguarded
- where: tree-sitter-al/src/scanner.c:5-32 vs 296-299, 340, 351, 375, 387, 468, 472,
  603-645, 1192-1204 (20 `strcmp`/`strncmp` calls); same shape in
  tree-sitter-al/generator/tools/al-gen/templates/scanner.c.template
- severity: medium
- scenario: under `__wasm__` the file includes only `<stdlib.h>` and defines
  `al_strlen`/`al_memcpy` by hand, with the comment "Zed's WASI SDK does not provide
  tree_sitter/alloc.h". `<string.h>` is included only in the `#ifndef __wasm__` arm,
  yet `strcmp`/`strncmp` are called 20 times outside any guard, including from
  `scanner_is_defined` and the directive keyword dispatch that every `#if` line hits.
  Whether this compiles depends on the wasi-sdk headers Zed happens to ship; clang 16
  and later treat an implicit function declaration as an error. I could not reproduce
  a wasm build here (no wasi-sysroot installed), and CI's `wasm` job builds
  `-p zed-al` for `wasm32-wasip2` rather than the grammar, so the wasm compile of
  scanner.c is not covered anywhere. Verify before acting.
- fix: add `al_strcmp`/`al_strncmp` next to `al_strlen`/`al_memcpy` and use them
  unconditionally, or include `<string.h>` in both arms. Either way, add a CI step
  that compiles `scanner.c` for a wasm target.
- status: rejected `scanner.c` includes `keywords.c` at line 234, before the first
  `strcmp` at line 296, and `keywords.c` includes `<string.h>` unconditionally
  (keywords.c:7), so every `strcmp`/`strncmp` in the translation unit is declared
  under `__wasm__` too. Verified by compiling `src/scanner.c` and `src/parser.c`
  for `wasm32-wasip1` against wasi-sysroot 25 with
  `-Werror=implicit-function-declaration` (clean, two unused-function warnings),
  and by `tree-sitter build --wasm`, which produced a 304 KB module. The template
  at `generator/tools/al-gen/templates/scanner.c.template` has the same include
  order. The missing wasm coverage was real: the grammar CI now runs
  `tree-sitter build --wasm` (tree-sitter-al 020b437).

### [SLOP] Stale comment claims the grammar has no object `name` field
- where: crates/al-syntax/src/symbols.rs:88-89
- severity: low
- scenario: the comment reads "Grammar doesn't assign a field name to the object name;
  use the shared extract_object_name helper". The grammar does assign it
  (grammar.js:288, `field('name', $.name_or_keyword)` under `prec.dynamic`), verified
  by parsing `codeunit 50100 Test { }` and reading `name:`. The comment sends the next
  reader to the positional scan below it, which is now the fallback path rather than
  the real one. lib.rs:132-137 describes the same code correctly.
- fix: delete the stale sentence; `extract_object_name` already documents the field-
  first, scan-as-fallback order.
- status: fixed 520766fa

### [BUG] al-gen slices the TextMate XML at raw byte offsets
- where: tree-sitter-al/generator/tools/al-gen/src/main.rs:659-662
- severity: low
- scenario: `let start_search = mat.start().saturating_sub(1000);` and
  `&xml[start_search..end_search]` index the XML by byte. `mat.start() - 1000` is not
  guaranteed to fall on a UTF-8 character boundary, so a non-ASCII character (an
  accented word in a TextMate scope comment or description) within 1000 bytes before a
  keyword-list match panics the generator with "byte index N is not a char boundary".
  Everything downstream (grammar.js, scanner.c, keywords.c, highlights.scm) is
  regenerated by this tool, so a Microsoft grammar update carrying one accented
  character stops the regeneration.
- fix: snap both offsets to char boundaries (`xml.floor_char_boundary` / a
  `is_char_boundary` loop) before slicing.
- status: fixed tree-sitter-al eef111e (gitlink 48cbd3b0)

## Review complete

- Nearly every 2026-07-31 audit item in this area is fixed and pinned by tests or by
  the 59-case grammar corpus; no audit item is still open, so nothing is tagged
  `[STILL-OPEN]`.
- `sort_members` corrupts a multi-object `.al` file: it takes the first `{` and the
  last `}` as one object body and moves the second object's procedures into the first,
  and `is_pure_reordering` cannot catch it because the result is a permutation.
- `clean_identifier_text` fixed quoted-identifier unescaping in lib.rs but ~20 call
  sites in navigation.rs, symbols.rs and type_resolver.rs (plus al-analysis's
  `node_clean_name`) still use `trim_matches('"')`, so hover, definition and rename
  silently fail on a name containing a doubled quote.
- `ts_range_to_syntax` walks the file from byte 0 for every range, which makes
  `documentSymbol` on a large table quadratic in file size; the crate already builds a
  line-start table twice and should share one.
- The incremental-parse benchmark never calls `Tree::edit`, so it measures tree reuse
  rather than the keystroke path it claims to model.
