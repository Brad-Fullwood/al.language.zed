# Grammar query drift check over the round 3 and round 4 node shapes

Scope: the shipped queries in `tree-sitter-al/queries/` against every entry in
`tree-sitter-al/test/corpus/`, with the corpus round 3 shapes (List and Dictionary types, glued
signs, keyword names, signed case labels) and the round 4 shapes (a quoted or keyword `for`
variable, the `argument` field) as the brief. `STATE.md` row F and work item 3 of
`HANDOFF-tree-sitter.md`. The work is on AL-Tree-Sitter branch `campaign/query-drift`, started at
fc80b85 (`dev`) and fast-forwarded into `dev` at baf782b, and on superproject branch
`campaign/grammar-queries`.

Tools: tree-sitter CLI 0.26.9, the tree-sitter Rust binding 0.25.10, alc 17.0.34.45391.

## What was checked

`bindings/rust/corpus_queries.rs` is a test module in the grammar crate. It parses each corpus
entry, skips the entries whose tree has an error (`recovery.txt`), and runs the shipped queries
over the rest. A highlight is resolved the way Zed resolves it: Zed pushes each capture onto a
stack in the order tree-sitter yields them and paints a position with the last pushed capture
that covers it. tree-sitter yields a capture on a wrapper node before the capture on the leaf
inside it. For `Caption = 'x';` the binding yields `name` @property and then `identifier`
@variable, so Zed shows `Caption` as a variable. The ten tests:

- `every_corpus_leaf_has_a_highlight`: each leaf has a capture.
- `highlight_captures_land_on_leaves`: no highlight capture sits on a wrapper node.
- `a_name_is_highlighted_by_its_role_and_not_its_spelling`: a name in one position (a parameter
  name, a `for` counter, an expression followed by an index) has one capture whether it is an
  identifier, a quoted identifier or a keyword. Before a member or scope suffix the keyword
  capture stays, since `Page.RunModal(...)` names the object and only a symbol table tells it
  from a variable named `Page`.
- `type_names_are_highlighted_as_types`: every word of a type reference is a type.
- `listed_shapes_have_their_highlight`: named leaves in named entries, such as `-1` as a number.
- `a_construct_is_captured_in_every_context`: a node kind that folds, indents, is an outline item,
  a text object or a scope in one place does so everywhere.
- `every_outline_item_has_a_name`.
- `every_declared_name_is_a_local_definition`: variables, labels, parameters, procedures,
  triggers, events and objects.
- `a_variable_highlight_is_a_local_reference_or_definition`.
- `a_multi_line_attribute_starts_the_fold_of_its_declaration` (see the folds decision below).

Four corpus entries were added so each shape the tests name exists: nested List and Dictionary
types with array element types (`collections.txt`), record and codeunit types qualified by a
namespace (`declarations.txt`), a page extension layout with move directives
(`object_extensions.txt`) and an `EventSubscriber` attribute over two lines (`attributes.txt`).
alc 17.0.34 with no symbol packages compiles each input with only missing symbol and unknown
namespace errors.

## What was missing

Round 3 and round 4 shapes:

| Shape | Before | After | Commit |
| --- | --- | --- | --- |
| a sign, glued (`X:=-1`) or spaced (`unary_operator`) | no capture, 23 leaves | `@operator` | 1731ae2 |
| a signed case label (`-1`, `- 2`, `-Limit`, `-Level::Gold.AsInteger()`) | no capture, 11 leaves | `@number` for a number, `@variable` otherwise | 1731ae2 |
| element types in `List of [...]` and `Dictionary of [...]` | `@keyword.control` (they are `control_keyword` tokens) | `@type.builtin`, three bracket levels deep, and the inner `of` stays `@keyword.control` | 1731ae2 |
| an array element type named after a keyword (`array[3] of Enum "Level"`) | `@keyword` | `@type.builtin` | 1731ae2 |
| a variable, parameter or label named after a keyword (`Page`, `Value`, `Code`, `Grid`) | `@keyword` or `@type.builtin`, and no local definition (32 variable and 7 parameter names) | `@variable.declaration` or `@variable.parameter`, and a local definition | e6a8f9f |
| a keyword name as a `for` or `foreach` counter, a plain value or an indexed target | `@keyword` or `@type.builtin` | `@variable` and a local reference | e6a8f9f |

A quoted `for` variable already had `@variable` and an outline name. The `argument` field needs
no query change: no query reads the children of `expression_list`, and `(comma) @punctuation`
still matches because the comma stays a named node. Outline, indents, folds, text objects and
brackets needed nothing: `a_construct_is_captured_in_every_context` and
`every_outline_item_has_a_name` passed before any change.

Older shapes the same tests found:

| Shape | Before | After | Commit |
| --- | --- | --- | --- |
| trigger and event names (`OnRun`) | `@function` on the wrapper, shown as `@variable` (10 names) | `@function` on the leaf | 6d21fe5 |
| property names that are identifiers (`Caption`, `TableRelation`) | `@property` on the wrapper, shown as `@variable` (14 names) | `@property` on the leaf | 6d21fe5 |
| a type qualified by a namespace (`Record Microsoft.Foundation.Company."Company Information"`) | `@type.builtin` on the wrapper, shown as `@variable` | `@type.builtin` on each name | 6d21fe5 |
| label names | no local definition (9 names) | a local definition, and `@variable.declaration` like variable names | e6a8f9f |
| a method named after a keyword called bare (`TestField(...)` in table code) | `@type.builtin` | `@function.call` | e6a8f9f |
| `.`, `::`, `:` after a case label, `=` of a property | no capture, 168 leaves | `@punctuation`, and `@operator` for `=` | f742ea8 |
| `keys`, `key`, `movefirst`, `moveafter`, `movebefore`, `movelast` | no capture | `@keyword` | f742ea8 |

`highlights.scm` comes from `generator/tools/al-gen/templates/highlights.scm.template` and
`locals.scm` from `generate_locals_scm` in `generator/tools/al-gen/src/main.rs`, so each fix
changed the generator source and the query file together. `make grammar` with the installed AL
extension regenerates both byte for byte, apart from the keyword noted under Open.

## Folds decision: the doc line is corrected

`Docs/features/language-assets.md` said `folds.scm` folds attribute lists. It has no
`(attribute)` entry, and adding one does not fold sensibly. An attribute is the first child of the
procedure it decorates, so the procedure's fold starts at the attribute's `[`. On the two line
attribute in `attributes.txt` the folds query gives:

| Query | Captures |
| --- | --- |
| `folds.scm` as shipped | procedure (2,4)-(6,8), block (5,4)-(6,7), procedure (8,4)-(11,8), block (10,4)-(11,7) |
| with `(attribute)` added | the same, plus attribute (2,4)-(3,49), which starts on the procedure's row, and attribute (8,4)-(8,100), a fold within one line |

The native folding in `crates/al-syntax/src/folding.rs` has no attribute fold either, and an
attribute fold only in `folds.scm` would make the two surfaces differ again. So `folds.scm` stays
as it is, and the doc line now lists what it folds: objects, sections, keys, enum values,
procedures, triggers, events, `var` sections, blocks, control statements, case branches, argument
lists and `#region` blocks. `a_multi_line_attribute_starts_the_fold_of_its_declaration` fails if
the grammar moves attributes out of their declaration or an attribute gets a fold of its own. The
row in `audit-backlog-triage.md` is marked fixed.

## Open

- Table field types. In `field(1; "No."; Code[20])` the header is a flat `parenthesized_block`,
  and every field type in it is a `control_keyword` shown as `@keyword.control`. All 15 types
  tried (`Code`, `Text`, `Decimal`, `Integer`, `Boolean`, `Enum`, `Option`, `Blob`, `DateTime`,
  `Guid`, `Date`, `RecordId`, `MediaSet`, `BigInteger`, `DateFormula`) lex that way. A query can
  reach the type only by its position between semicolons, so the fix is a grammar change: a field
  declaration node with `id`, `name` and `type` fields, as table keys got `key_declaration`. This
  is not a round 3 or round 4 shape and is not changed here.
- `make grammar` with the installed AL extension 18.0.2732683 adds one keyword,
  `datasourcecontext`, to `grammar.js`, the scanner, the parser, the data files and one line of
  `highlights.scm`. That comes from the newer extension, is outside this work and was not
  committed.
- A procedure's fold starts on the row of its first attribute, in `folds.scm` and in the native
  folding, so a folded procedure with an attribute shows the attribute row and hides the
  `procedure` row. Not changed.
- `audit-backlog-triage.md` still counts the folds item as open in its "Triage complete" totals
  and in the Analysis and Insight list. Only its row was changed.

## Gates

Grammar at baf782b: `tree-sitter test` 125 of 125 (121 before), `cargo test --all-targets` 30
passed (20 before), `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean for
the crate and the generator, generator tests 16 passed, `tests/run_repo_tests.sh` 46,389 of
46,389 files. `cargo package` builds, and the packaged crate passes its 30 tests now that the
package carries `test/corpus/`.

Superproject at 83820e8d (the merge of this branch): fmt, clippy, clippy with the semantic
feature, rustdoc clean, 125 suites, 5822 passed, 0 failed, 20 ignored, harness green
(`.campaign/gates-83820e8d.log`). The seven shared query files match `tree-sitter-al/queries`
byte for byte after the merge.

## Commits

AL-Tree-Sitter, `campaign/query-drift`, fast-forwarded into `dev` at baf782b:

- 0e5f7a1 test: run the shipped queries over every corpus entry
- 1731ae2 fix(highlights): signs, signed case labels and List and Dictionary element types
- 6d21fe5 fix(highlights): capture trigger, event, property and namespaced type names on the leaf
- e6a8f9f fix(highlights,locals): variables named after keywords
- f742ea8 fix(highlights): punctuation, key sections and layout move directives
- e772d2e test: pin why a multi-line attribute has no fold of its own
- baf782b build: package the corpus the query tests read

Superproject, `campaign/grammar-queries`:

- a104f535 chore(grammar): move the pointer to baf782b for the query drift fixes (gitlink,
  `extension.toml` rev, `languages/al/highlights.scm` and `locals.scm`, the folds doc line)
- this file
