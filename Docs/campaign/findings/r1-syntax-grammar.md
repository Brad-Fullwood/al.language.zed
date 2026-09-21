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
- [ ] tree-sitter-al/bindings/rust
- [ ] tree-sitter-al/generator/
- [ ] tree-sitter-al/test/corpus + tests/
- [ ] crates/al-syntax/src/lib.rs
- [ ] crates/al-syntax/src/parser.rs
- [ ] crates/al-syntax/src/tokens.rs
- [ ] crates/al-syntax/src/type_resolver.rs
- [ ] crates/al-syntax/src/symbols.rs
- [ ] crates/al-syntax/src/formatting.rs
- [ ] crates/al-syntax/src/sort.rs
- [ ] crates/al-syntax/src/lint.rs
- [ ] crates/al-syntax/src/navigation.rs
- [ ] crates/al-syntax/src/complexity.rs
- [ ] crates/al-syntax/src/folding.rs
- [ ] crates/al-syntax/src/context.rs
- [ ] crates/al-syntax/src/language_data.rs
- [ ] crates/al-syntax/src/lexical.rs
- [ ] crates/al-syntax/src/traversal.rs
- [ ] crates/al-syntax/src/types.rs
- [ ] crates/al-syntax/benches/parser.rs

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

### [GAP] `outline.scm` emits an outline item for every block statement
- where: languages/al/outline.scm:39-66 (identical in tree-sitter-al/queries/outline.scm)
- severity: medium
- scenario: the last eight patterns capture `(begin_end_block (kw_begin) @name) @item`,
  `(if_statement (kw_if) @name) @item`, and the same for `case`, `for`, `foreach`,
  `while`, `repeat`, `with`. Zed drives its outline view from `@item`/`@name`, so a
  1000-line codeunit with 200 `begin` blocks and 150 `if` statements produces ~350
  extra outline rows literally named `begin`, `if`, `case`. The object and procedure
  rows are buried. `Docs/features/language-assets.md:22` documents outline.scm as
  "objects, procedures/triggers, events, keys, enum values" only, so the docs and the
  query disagree.
- fix: drop the eight block-statement patterns, or move them to a separate query if
  the breadcrumb bar really needs them (Zed uses the same file for both surfaces, so
  a separate file is the only way to have one without the other).
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
- status: open

### [SIMPLIFY] `line_range` takes an unused `_line` parameter
- where: crates/al-syntax/src/lint.rs:117
- severity: low
- scenario: `fn line_range(text: &str, line_idx: usize, _line: &str) -> Range` never
  reads the third argument. Call sites pass either the real line (lint.rs:358) or `""`
  (lint.rs:397 and elsewhere), which reads as if the two cases behave differently.
- fix: delete the parameter and update the call sites.
- status: open

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
- status: open

### [TEST] No sort test covers a file with two objects
- where: crates/al-syntax/src/sort.rs:350-860 (test module)
- severity: medium
- scenario: the 20 tests cover var hoisting, attributes, CRLF, string braces and
  pure-reordering, but every fixture is a single object, so the corruption above ships
  green. `sorting_is_always_a_pure_reordering` (sort.rs:507) is the test that looks
  like it would catch it and cannot, because the bug *is* a pure reordering.
- fix: add a test asserting `sort_members` returns `None` for a two-object source, and
  one asserting each object keeps its own members when the guard is added.
- status: open
