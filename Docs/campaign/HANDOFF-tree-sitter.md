# Hand-off: grammar work for tree-sitter-al

For an agent picking up the grammar changes the campaign asked for. Written 2026-09-27 at the
pause. Read `CHECKPOINT-2026-09-27.md` and `CHECKPOINT-2026-09-27-close.md` first for the
repository state.

## Where the grammar is

- The grammar is the `tree-sitter-al/` submodule, the Brad-Fullwood/AL-Tree-Sitter repository.
- The superproject pins 142aba6. `extension.toml` names the same revision (`rev = ...`, line 39),
  and `scripts/check-release-hygiene.sh` fails when the two differ.
- AL-Tree-Sitter `dev` is at 82e89f8, a merge of the campaign branch and pull request 3. Its tree
  is byte-identical to 142aba6 (`git -C tree-sitter-al diff --quiet 142aba6 origin/dev`
  succeeds), so moving the pointer there changes no parser source. Move it anyway with the first
  grammar change below, so the pin is a commit on `dev`.

## How a grammar change lands (the pattern of 4c429ac5)

1. In `tree-sitter-al/`, branch from `origin/dev`. Edit `grammar.js` (and `src/scanner.c` for
   token-level issues).
2. Add the new shape to a file under `test/corpus/` (`statements.txt` for loop forms,
   `interpreter_kinds.txt` for any node kind the interpreter or router matches).
3. Regenerate and test: `make grammar` from the superproject root (needs the tree-sitter CLI;
   `tree-sitter-al/tests/check_cli_version.sh` names the version), then in `tree-sitter-al/`
   `tree-sitter test`, `cargo test --all-targets`, and `tests/run_repo_tests.sh`.
4. Push the branch and merge it into AL-Tree-Sitter `dev`.
5. In the superproject: move the gitlink to the new `dev` commit, set `extension.toml` `rev` to
   the same hash, and record the change in a findings file under `Docs/campaign/findings/`.
6. Run the full gates before pushing, because the parser source is part of the workspace build:
   `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude zed-al --all-targets -- -D
   warnings`, `cargo test --workspace --exclude zed-al`, `bash scripts/check-release-hygiene.sh`,
   and the node kind guard `cargo test -p al-test --test node_kind_literals`.
7. The seven shared query files must stay byte-identical between `tree-sitter-al/queries/` and
   `languages/al/`. `make language` copies them; the Makefile check fails on drift.

## Work items, in order

### 1. R12-GR-1: a quoted identifier as the `for` loop variable (open, blocks real code)

- Symptom: `for "My Index" := 1 to 3 do` parses as an `ERROR` at `for` and another at
  `1 to 3 do`. `foreach "My Item" in L do` and `"My Index" := 1` parse.
- Cause: `for_statement` (`grammar.js`, about line 947) takes `field('iterator',
  $.identifier)`. `foreach_statement` takes `field('iterator', $.name_or_keyword)`, which
  accepts a quoted identifier and a variable named after a keyword.
- Fix: give `for_statement`'s iterator `$.name_or_keyword`. Keep the field name `iterator`:
  `crates/al-runtime/src/interpreter/eval_stmt.rs` reads it (lines 255 and 387).
- Corpus: `for "My Index" := 1 to 3 do`, `for "I" := 3 downto 1 do`, and `for Value := 1 to 2
  do` (a keyword-named variable, which `name_or_keyword` also admits).
- Superproject follow-up: the scratch test `r12_scratch_quoted_loop` in
  `.campaign/r12-scratch-tests.patch` (git-ignored, on the machine that ran the campaign) becomes a real interpreter test once this lands
  (`crates/al-runtime/src/interpreter/records_tests.rs` holds the neighbours). Mark R12-GR-1
  fixed in `Docs/campaign/findings/r12-session-review.md`.

### 2. A field for each call argument (additive, low risk)

- Symptom: `argument_list` holds an `expression_list` whose named children are the expressions
  and a named `comma` node between each pair. Every caller that counts or walks arguments
  filters `comma` out by hand: `al-test/src/router/ast.rs` (`argument_count`, added for the
  `List.GetRange` var form), `al-insight/src/calls/call_sites.rs:246`,
  `al-insight/src/calls/object_symbols.rs:210`, `al-emit/src/symbol_extract.rs` (three
  places), `al-runtime/src/interpreter/eval_stmt.rs:1289`, `records.rs:758`,
  `al-analysis/src/queries/binding.rs:186` and `inlay_hints.rs` (two places).
- Fix: in `expression_list`, wrap each expression in `field('argument', ...)` so callers can use
  `children_by_field_name("argument")`. Do not make `comma` anonymous: the callers above and
  `highlights.scm` (`(comma) @punctuation`) match it by name.
- The superproject callers can move to the field in a later change; this item only adds it.

### 3. Query drift check over the round 3 node shapes (from `STATE.md`, row F)

- Corpus round 3 added shapes (List and Dictionary types, glued signs, keyword names, signed case
  labels). Check that `highlights.scm`, `outline.scm`, `indents.scm` and the other shared queries
  capture them, since the queries were last checked against round 2 shapes. Add a corpus-backed
  query test in the grammar repository for any capture that was missing.

## Already done, do not redo

GR2-1 (attribute kept after a var section, 290ef3c), GR2-4 (spaced minus and negative range case
labels, a108400), GR3-1 (`X:=-1`, 23b6f92), the object `name` field (pull request 3), and the
al-gen TextMate slicing fix (eef111e). The findings files `grammar-attribute-after-var.md`,
`grammar-corpus-r2.md` and `grammar-corpus-r3.md` hold the detail.
