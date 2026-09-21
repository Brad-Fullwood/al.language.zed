# R1b review: al-analysis and al-insight, modules not covered by r1

Adversarial read-only review of every module marked NOT COVERED in
`r1-analysis-insight.md`, plus the files that checklist does not list at all.

## Coverage

Priority (edits user files / core resolution):
- [ ] queries/rename.rs
- [ ] resolution.rs
- [ ] queries/breaking_changes.rs

Rest of scope:
- [ ] xliff.rs
- [ ] scaffold.rs
- [ ] generators.rs
- [ ] queries/source.rs
- [ ] queries/audit.rs
- [ ] queries/obsolescence.rs + queries/obsolete_usage.rs
- [ ] queries/upgrade.rs
- [ ] al-insight/src/index.rs
- [ ] al-insight/src/discovery.rs
- [ ] queries/test_diagnostics.rs
- [ ] queries/code_actions/test_support.rs
- [ ] queries/suggest_event.rs, profiler_hints.rs, test_coverage.rs

Files the first checklist does not list at all:
- [ ] lsp.rs
- [ ] queries/mod.rs
- [ ] queries/binding.rs
- [ ] queries/complexity.rs
- [ ] queries/definition.rs
- [ ] queries/deps.rs
- [ ] queries/folding.rs
- [ ] queries/format.rs
- [ ] queries/implementation.rs
- [ ] queries/references.rs
- [ ] queries/search.rs
- [ ] queries/semantic_tokens.rs
- [ ] queries/symbols.rs
- [ ] queries/code_lens.rs, hover.rs, completions.rs, signature.rs

## Findings

### [BUG] Renaming any quoted identifier is a silent no-op
- where: crates/al-analysis/src/queries/rename.rs:23-26 (`prepare_rename`) and 194-216 (`is_valid_rename_target`)
- severity: high
- scenario: cursor on the field `"Posting Date"`. `prepare_rename` returns the node's full range (quotes included) and `node_clean_name` as the placeholder, which is `Posting Date` with the quotes stripped (queries/mod.rs:68-76). `handle_prepare_rename` (crates/al-lsp/src/server/definition.rs:95-98) sends that as `PrepareRenameResponse::RangeWithPlaceholder`, so the editor pre-fills the box with `Posting Date`. The user edits it to `Posted Date` and the client sends `newName = "Posted Date"` with no quotes. `is_valid_rename_target` takes the unquoted branch, `chars.all(is_ascii_alphanumeric || '_')` fails on the space, and `rename` returns `Ok(None)`. The editor reports nothing. Every quotable AL name (`"No."`, `"Posting Date"`, `"Sales Header"`, any object name with a space) is unrenameable, which is most of the names a BC developer touches.
- why the tests miss it: `make_rename_text_quoted_identifier` (rename.rs:606-611) calls the helper directly with `"New Name"`, bypassing `is_valid_rename_target`. `invalid_new_names_rejected` (548-558) only asserts the rejections. No test drives `rename` on a quoted identifier.
- fix: in `rename`, re-quote before validating when the cursor node is a `quoted_identifier` (or when the name needs quoting), i.e. validate `new_name` as the *interior* of a quoted identifier in that case. Alternatively have `prepare_rename` return the placeholder with quotes so the round trip is closed.
- status: open

### [BUG] Rename does not update `[EventSubscriber]` references, so renaming an event breaks every subscriber
- where: crates/al-analysis/src/queries/rename.rs:112-167
- severity: high
- scenario: `[IntegrationEvent(false, false)] local procedure OnAfterPostSalesDoc(...)` in codeunit A, and in codeunit B `[EventSubscriber(ObjectType::Codeunit, Codeunit::"A", 'OnAfterPostSalesDoc', '', false, false)]`. Rename the publisher procedure to `OnAfterPostSales`. `find_variable_references` only matches `identifier`/`quoted_identifier`/`name` nodes (crates/al-syntax/src/navigation.rs:255-259), and the subscriber names the event in a *string literal*, so B is never edited. The result compiles to AL0132 (or, on older runtimes, a subscriber that silently never fires). `references.rs:59-64` already calls `al_syntax::find_event_subscriber_references` for exactly this reason, with the comment "surface them so `references` on an event lists its subscribers". `rename` never calls it, so Find All References and Rename disagree about what the symbol's references are.
- fix: in the per-file loop, also walk `find_event_subscriber_references(tree, text, clean_name)` and emit an edit for the event-name argument. Note the helper returns the whole `attribute_argument` range including the `'` quotes, so the replacement text has to be `'NewName'`. Guard it on the cursor node actually being an event procedure declaration (an `[IntegrationEvent]`/`[BusinessEvent]` attribute on the enclosing procedure) so an ordinary rename does not rewrite unrelated attribute strings.
- status: open

### [BUG] `is_valid_rename_target` rejects legal AL identifiers because it treats every type keyword as reserved
- where: crates/al-analysis/src/queries/rename.rs:214-215
- severity: medium
- scenario: `!al_syntax::language_data::is_keyword(name)` tests against `KEYWORD_SET` (crates/al-syntax/src/language_data.rs:215-224), which is `control + object + type + operator` from `tree-sitter-al/data/keywords.json`. That set contains `value`, `version`, `view`, `views`, `file`, `list`, `none`, `dialog`, `database`, `cookie`, `joker` and 120 more type names. AL accepts all of these as declaration names, and the grammar says so itself at `tree-sitter-al/grammar.js:452`: "(declaration names are `name_or_keyword`, so those are all legal names)". The base app writes `field(5; Value; Text[2048])` unquoted. So renaming a field or variable to `Value` returns `Ok(None)` and the editor does nothing, with no message explaining why.
- fix: check the new name against the set of words that are genuinely not usable as an unquoted identifier (the statement keywords: `begin`, `end`, `if`, `then`, `var`, `procedure`, ...), not against the full keyword table. The grammar's own `name_or_keyword` rule is the authority for what a declaration name may be.
- status: open

### [BUG] A quoted new name may contain a newline and split the identifier across lines
- where: crates/al-analysis/src/queries/rename.rs:201-204
- severity: low
- scenario: the already-quoted branch accepts any interior that is non-empty and has no `"`. `rename(ws, uri, pos, "\"My\nField\"")` passes validation and is spliced into every reference, producing `"My` / `Field"` on two lines, which does not parse. Reachable from any client that does not sanitise the rename input box (the LSP spec places no constraint on `newName`), and from the daemon path at crates/al-lsp/src/server/daemon/lsp_dispatch.rs:241 where `new_name` comes straight off the wire.
- fix: reject interiors containing any control character, and reject a leading or trailing space.
- status: open

### [TEST] No test covers a cross-file rename that should produce edits
- where: crates/al-analysis/src/queries/rename.rs:132-167, tests at 229-625
- severity: medium
- scenario: the two multi-file tests (`rename_procedure_does_not_touch_same_name_in_other_object`, `rename_field_does_not_touch_same_name_in_other_table`) both assert only that *no* edit lands in the other file. No test asserts that a genuine cross-file reference *is* renamed, and the harness tests (crates/al-test-harness/tests/{e2e,integration_full,real_world,completeness,zed_simulation}.rs) are all single-file. The whole `file_index` loop plus its `decl_loc_for_reference` filter could return no cross-file edits at all and every test would still pass, which is exactly the failure mode the binder filter risks: `decl_loc` falls back to `(uri, pos.line, pos.character)` when `definition()` resolves nothing (binding.rs:47), and that fallback key can never equal `cursor_decl`, so any reference the definition query cannot resolve is silently dropped from the rename.
- fix: add a test with a table declaring `field(2; Amount; Decimal)` and a separate codeunit using `Cust.Amount`, and assert the codeunit gets exactly one edit.
- status: open
