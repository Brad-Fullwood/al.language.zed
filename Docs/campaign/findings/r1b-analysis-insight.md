# R1b review: al-analysis and al-insight, modules not covered by r1

Adversarial read-only review of every module marked NOT COVERED in
`r1-analysis-insight.md`, plus the files that checklist does not list at all.

## Coverage

Priority (edits user files / core resolution):
- [x] queries/rename.rs (plus queries/binding.rs, which it depends on)
- [x] resolution.rs
- [x] queries/breaking_changes.rs

Rest of scope:
- [x] xliff.rs
- [ ] scaffold.rs  — findings pending from orchestrator
- [ ] generators.rs  — findings pending from orchestrator
- [ ] queries/source.rs  — NOT COVERED
- [ ] queries/audit.rs  — NOT COVERED
- [x] queries/obsolescence.rs + queries/obsolete_usage.rs
- [x] queries/upgrade.rs
- [x] al-insight/src/index.rs
- [x] al-insight/src/discovery.rs
- [ ] queries/test_diagnostics.rs  — NOT COVERED
- [ ] queries/code_actions/test_support.rs  — NOT COVERED
- [ ] queries/suggest_event.rs, profiler_hints.rs, test_coverage.rs  — NOT COVERED

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
- status: fixed eae29618

### [BUG] Rename does not update `[EventSubscriber]` references, so renaming an event breaks every subscriber
- where: crates/al-analysis/src/queries/rename.rs:112-167
- severity: high
- scenario: `[IntegrationEvent(false, false)] local procedure OnAfterPostSalesDoc(...)` in codeunit A, and in codeunit B `[EventSubscriber(ObjectType::Codeunit, Codeunit::"A", 'OnAfterPostSalesDoc', '', false, false)]`. Rename the publisher procedure to `OnAfterPostSales`. `find_variable_references` only matches `identifier`/`quoted_identifier`/`name` nodes (crates/al-syntax/src/navigation.rs:255-259), and the subscriber names the event in a *string literal*, so B is never edited. The result compiles to AL0132 (or, on older runtimes, a subscriber that silently never fires). `references.rs:59-64` already calls `al_syntax::find_event_subscriber_references` for exactly this reason, with the comment "surface them so `references` on an event lists its subscribers". `rename` never calls it, so Find All References and Rename disagree about what the symbol's references are.
- fix: in the per-file loop, also walk `find_event_subscriber_references(tree, text, clean_name)` and emit an edit for the event-name argument. Note the helper returns the whole `attribute_argument` range including the `'` quotes, so the replacement text has to be `'NewName'`. Guard it on the cursor node actually being an event procedure declaration (an `[IntegrationEvent]`/`[BusinessEvent]` attribute on the enclosing procedure) so an ordinary rename does not rewrite unrelated attribute strings.
- status: fixed 9148954c

### [BUG] `is_valid_rename_target` rejects legal AL identifiers because it treats every type keyword as reserved
- where: crates/al-analysis/src/queries/rename.rs:214-215
- severity: medium
- scenario: `!al_syntax::language_data::is_keyword(name)` tests against `KEYWORD_SET` (crates/al-syntax/src/language_data.rs:215-224), which is `control + object + type + operator` from `tree-sitter-al/data/keywords.json`. That set contains `value`, `version`, `view`, `views`, `file`, `list`, `none`, `dialog`, `database`, `cookie`, `joker` and 120 more type names. AL accepts all of these as declaration names, and the grammar says so itself at `tree-sitter-al/grammar.js:452`: "(declaration names are `name_or_keyword`, so those are all legal names)". The base app writes `field(5; Value; Text[2048])` unquoted. So renaming a field or variable to `Value` returns `Ok(None)` and the editor does nothing, with no message explaining why.
- fix: check the new name against the set of words that are genuinely not usable as an unquoted identifier (the statement keywords: `begin`, `end`, `if`, `then`, `var`, `procedure`, ...), not against the full keyword table. The grammar's own `name_or_keyword` rule is the authority for what a declaration name may be.
- status: fixed eae29618

### [BUG] A quoted new name may contain a newline and split the identifier across lines
- where: crates/al-analysis/src/queries/rename.rs:201-204
- severity: low
- scenario: the already-quoted branch accepts any interior that is non-empty and has no `"`. `rename(ws, uri, pos, "\"My\nField\"")` passes validation and is spliced into every reference, producing `"My` / `Field"` on two lines, which does not parse. Reachable from any client that does not sanitise the rename input box (the LSP spec places no constraint on `newName`), and from the daemon path at crates/al-lsp/src/server/daemon/lsp_dispatch.rs:241 where `new_name` comes straight off the wire.
- fix: reject interiors containing any control character, and reject a leading or trailing space.
- status: fixed eae29618

### [BUG] A field name containing a parenthesis is invisible to hover, go-to-definition and completion
- where: crates/al-analysis/src/resolution.rs:1517-1527 (`parse_field_line`), used by `find_workspace_field` (1600-1620) and `workspace_field_items` (1622-1638)
- severity: high
- scenario: `field(50; "Amount (LCY)"; Decimal) { }` in a workspace table. `parse_field_line` does `trimmed.strip_prefix("field(")?.split(')').next()?`, which cuts at the *first* `)`, giving `50; "Amount (LCY"`. `splitn(3, ';')` then yields only two segments, so `parts.next()?` for the type returns `None` and the whole field is dropped. `find_workspace_field` never matches it, so hover and go-to-definition on `Rec."Amount (LCY)"` return nothing, and `workspace_field_items` omits it from the `Rec.` completion list. `"Amount (LCY)"`, `"Sales (LCY)"`, `"Profit (LCY)"`, `"Qty. (Base)"` are standard Business Central field names that developers copy into custom tables. The same cut breaks any field whose name contains `;`, for example `field(1; "A;B"; Text[10])`, which yields `name_part = "A` and `ty = B"`.
- fix: `field_decl_nodes` already hands `parse_field_node` a tree-sitter node, so the id, name and type are available as child nodes. Read them from the node instead of re-splitting the text. If the text split has to stay, make it quote-aware the way `split_last` (1640-1657) already is.
- status: fixed be039a6c

### [TEST] No `parse_field_line` test uses a quoted field name with punctuation
- where: crates/al-analysis/src/resolution.rs:2490-2510
- severity: medium
- scenario: the four `parse_field_line` tests cover `field(1; Name; Text[50])`, a non-field line, `field(1)` and an empty name. None uses a quoted name, and none uses a name containing `(`, `)` or `;`. The field-resolution tests at 1788-1860 use `Name`, `Amount`, `Qty`, `Ørnamental` and `München`, all punctuation-free. The bug above is therefore entirely invisible to the suite.
- fix: add `field(50; "Amount (LCY)"; Decimal) { }` to both `parse_field_line_extracts_name_and_type` and `workspace_field_items_lists_fields`.
- status: fixed be039a6c

### [PERF] Every member completion re-reads and re-parses the receiver's whole symbol-package source from disk
- where: crates/al-analysis/src/resolution.rs:1021-1056 (`symbol_package_proc_docs`), called at 1128
- severity: high
- scenario: typing `Cust.` where `Cust: Record Customer`. `queries/completions.rs:99` calls `completion_items_for_receiver` on every `textDocument/completion` request with no caching. For each symbol entry named `Customer`, `symbol_package_proc_docs` calls `get_or_create_virtual_file`, which itself does `fs::read_to_string` inside `find_object_range` (crates/al-symbols/src/virtual_file.rs:316-317), then reads the same file again at resolution.rs:1034, runs `AlParser::parse_quick` over it, and runs `extract_document_symbols` plus `extract_doc_comment` per procedure. The extracted Customer source from the base app is several thousand lines. That is two synchronous full-file reads and a full tree-sitter parse per keystroke, and the entire result is used only to fill in the `documentation` field of the completion items at line 1143.
- fix: cache the `name -> docs` map keyed on the virtual file's path and mtime, or drop `documentation` from the initial completion list and fill it in `completionItem/resolve`, which is what that LSP request exists for.
- status: open

### [BUG] `resolve_object_path` is kind-blind, so a table and a page sharing a name resolve to whichever was indexed last
- where: crates/al-analysis/src/resolution.rs:1344-1368, called from `resolve_member` (593) and `completion_items_for_receiver` (1076)
- severity: medium
- scenario: a project with `table 50100 "Sales Setup"` in Tab50100.al and `page 50100 "Sales Setup"` in Pag50100.al, which is the normal AL naming convention for a setup table and its card. `file_index.object_path` (crates/al-source/src/file_index.rs:629-633) returns `owners.first()`, and `add_file_with_tree` (file_index.rs:540-546) removes the re-indexed path's entry and pushes it to the *back* of the owners vector. So editing Tab50100.al moves the table to the end and `object_path("sales setup")` starts returning the page. After that, `resolve_member` for `Setup."Posting No. Series"` searches the page file, and `completion_items_for_receiver` offers the page's globals and procedures instead of the table's fields. The receiver's `type_name` is `Record` at both call sites, so the correct kind is known and simply not used. `resolve_workspace_object_definition_of_type` (935-966) already documents and solves this exact problem for go-to-definition.
- fix: give `resolve_object_path` an optional AL type keyword and route it through `file_index.object_path_of_kind` (file_index.rs:638-645), passing `receiver.type_name` from both call sites.
- status: open

### [BUG] `extract_return_type` reports a return type for procedures that have none
- where: crates/al-analysis/src/resolution.rs:1711-1714, used at 1404-1408
- severity: medium
- scenario: `procedure GetCustomer(var Cust: Record Customer)` has no return type, so `extract_procedure_symbol` (crates/al-syntax/src/symbols.rs:359-363) sets `detail = "(var Cust: Record Customer)"`. `extract_return_type` is `detail.rsplit_once(": ")`, which finds the `": "` inside the *parameter list* and returns `Record Customer)`. `parse_type_expr` turns that into `{ type_name: "Record", type_subtype: Some("Customer)") }`. `workspace_member` attaches it as the procedure's `type_info`, so `Helper.GetCustomer.` (a parameterless-style call, legal AL syntax) offers the full `TableClass` builtin method list through `builtin_for` as if the void procedure returned a record. The trailing `)` on the subtype also makes every real field lookup miss.
- fix: return `None` unless the detail's closing `)` is followed by `: `, i.e. split on the `": "` that occurs *after* the last `)`, not the last one anywhere in the string.
- status: open

### [BUG] Hover and go-to-definition fire on identifiers inside comments and string literals
- where: crates/al-analysis/src/resolution.rs:101-111 (`access_path_at` tries the text scan before the tree) and 208-276 (`access_path_from_text`)
- severity: low
- scenario: the line `        // Update Cust.Name before posting` inside a procedure that declares `Cust: Record Customer`. Hovering `Name` reaches `hover` (queries/hover.rs:44), whose `find_node_at_position` returns the `comment` node with non-empty text, so the early return at hover.rs:32-35 does not trigger. `access_path_at` calls `access_path_from_text` first, which works purely on the raw line text with no notion of comments or literals, and returns `receiver: "Cust", member: "Name"`. The tooltip shows `Name: Text[100]`. The same happens for `Error('Cust.Name is required');` and for Ctrl+Click, since `definition` uses the same helper.
- fix: before the text scan, check whether the node at the position is a `comment` or a string literal and return `None`. The tree branch already cannot fire inside a comment, so only the text shortcut needs the guard.
- status: open

### [GAP] Length-qualified types lose their builtin members
- where: crates/al-analysis/src/resolution.rs:1659-1674 (`parse_type_expr`) and 45-68 (`builtin_for`)
- severity: low
- scenario: `field(3; Description; Text[100]) { }` in a workspace table. `parse_field_line` returns `ty = "Text[100]"`, and `parse_type_expr` finds no space so it produces `{ type_name: "Text[100]", type_subtype: None }`. `builtin_for` then looks up `Text[100]Class` and `Text[100]`, both of which miss, so `Rec.Description.` offers no Text methods at all. `al_syntax`'s own `parse_type_reference` strips the length (crates/al-syntax/src/type_resolver.rs:544 documents `Text[100] -> ("Text", None)`), so local variables work and table fields do not. The same applies to `Code[20]` and to `array[10] of Text`, where `split_once(' ')` produces `type_name = "array[10]"` and `type_subtype = "of Text"`.
- fix: strip a trailing `[...]` in `parse_type_expr` the way `parse_type_reference` does, and handle `array[N] of T` explicitly.
- status: open

### [TEST] No test covers a cross-file rename that should produce edits
- where: crates/al-analysis/src/queries/rename.rs:132-167, tests at 229-625
- severity: medium
- scenario: the two multi-file tests (`rename_procedure_does_not_touch_same_name_in_other_object`, `rename_field_does_not_touch_same_name_in_other_table`) both assert only that *no* edit lands in the other file. No test asserts that a genuine cross-file reference *is* renamed, and the harness tests (crates/al-test-harness/tests/{e2e,integration_full,real_world,completeness,zed_simulation}.rs) are all single-file. The whole `file_index` loop plus its `decl_loc_for_reference` filter could return no cross-file edits at all and every test would still pass, which is exactly the failure mode the binder filter risks: `decl_loc` falls back to `(uri, pos.line, pos.character)` when `definition()` resolves nothing (binding.rs:47), and that fallback key can never equal `cursor_decl`, so any reference the definition query cannot resolve is silently dropped from the rename.
- fix: add a test with a table declaring `field(2; Amount; Decimal)` and a separate codeunit using `Cust.Amount`, and assert the codeunit gets exactly one edit.
- status: fixed eae29618

### [BUG] `xlf.untranslated` always reports an empty object type, id and name
- where: crates/al-analysis/src/xliff.rs:836-838, surfaced at crates/al-lsp/src/server/daemon/build_dispatch/xliff.rs:341-352
- severity: high
- scenario: `parse_xliff` builds each `TranslationUnit` with `object_type: String::new(), object_id: 0, object_name: String::new()` and the comment `// reconstructed from id`. Nothing reconstructs them. `dispatch_xlf_untranslated` reads a language `.xlf` through `parse_xliff` and emits `"objectType": u.object_type, "objectId": u.object_id, "objectName": u.object_name` for every item, so every row of the `xlf.untranslated` response carries `""`, `0`, `""`. A translator asking which object a missing string belongs to gets nothing back, for every string.
- fix: parse the id back into its parts (`<ObjectType> <hash> - ...`) and fill at least `object_type`, or drop the three fields from the response and from `TranslationUnit` when it came from a parse.
- status: open

### [BUG] Only the first object in a multi-object `.al` file gets translation units, and the rest collide or vanish
- where: crates/al-analysis/src/xliff.rs:158-166 (`extract_from_file`), 467-546 (`detect_object_header`)
- severity: high
- scenario: AL allows several objects in one file and the index explicitly supports it (crates/al-source/src/file_index.rs:695). `detect_object_header` returns on the *first* declaration it finds, and `extract_from_file` then attributes every `Caption`, `ToolTip` and `Label` in the whole file to that one object. Given a file holding `table 50100 "Shipment Header"` followed by `table 50101 "Shipment Line"`, the second table's captions get ids built from `name_hash("Shipment Header")` and note text naming the wrong object. When both tables have a field of the same name (`"Document No."`, near-universal in BC), the two produce a byte-identical id, and `extract_translation_units` (134-145) drops the second as a duplicate. The translation for the second table's field is then simply absent from the `.g.xlf`, with only a `tracing::warn!` that no CLI surface shows.
- fix: scan for every object declaration in the file and re-anchor the object context when the brace depth returns to zero, rather than detecting a single header up front.
- status: open

### [BUG] `Caption='X';` without spaces around `=` produces no translation unit
- where: crates/al-analysis/src/xliff.rs:582-591 (`parse_property_value`)
- severity: medium
- scenario: `prefix = format!("{} =", property)` then `line_lower.starts_with(&prefix_lower)`, so the match requires exactly one space between the property name and `=`. `Caption='Posted Shipment';` and `Caption  = 'Posted Shipment';` both fail `starts_with("caption =")` and the string is silently left out of the generated `.g.xlf`. alc accepts either spelling, so the file compiles and ships with an untranslatable caption that nobody is told about. `parse_label_declaration` (595-610) does not have this problem because it matches on `:` and `label ` separately.
- fix: split the line on the first `=`, trim both sides, and compare the left side to the property name case-insensitively.
- status: open

### [BUG] A `Comment` containing the word "locked" suppresses the translation unit
- where: crates/al-analysis/src/xliff.rs:371-410 (`property_is_locked`)
- severity: medium
- scenario: the function finds the first `'`, skips that one literal honouring `''`, then searches the *raw remainder* for "locked". It never skips the later literals. Given `Caption = 'Closed', Comment = 'Shown when the period is locked; %1 is the date';`, the remainder after the first literal still contains the Comment's text. `lower.find("locked")` hits inside the comment string, `after` is `; %1 is the date';`, `strip_prefix('=')` returns `None`, and the `None` arm accepts `after.starts_with(';')` as the bare-`Locked` shorthand. The caption is treated as locked and dropped from the `.g.xlf`, so it can never be translated. `Comment = 'locked, see the manual'` triggers the same through the `,` branch.
- fix: skip every single-quoted literal on the line before searching for the modifier, reusing the same doubled-quote-aware scan the function already has for the first literal.
- status: open

### [BUG] An action group inside `actions` is keyed as `Control` instead of `Action`
- where: crates/al-analysis/src/xliff.rs:259-275 (`MemberBlock::in_actions` and `id_kind`), set only at 302-308
- severity: medium
- scenario: `in_actions: true` is assigned in exactly one place, on the unnamed `actions` marker block itself. Every real member is constructed at 332-336 with `in_actions: false`, and `id_kind` reads only `self.in_actions`, never an ancestor's. So for
  ```
  actions { area(Processing) { group(Posting) { Caption = 'Posting'; action(Post) { Caption = 'Post'; } } } }
  ```
  the `action(Post)` caption is keyed `Action` through the `self.keyword == "action"` test, but the `group(Posting)` caption is keyed `Control`, because `group` is not in that keyword test and the enclosing `actions` flag is never consulted. alc emits `Action` for action groups, so the generated id does not match the one in the translator's file: `refresh_xliff` reports the unit as both added and removed on every run and the existing translation is lost.
  - the field is therefore close to dead: it is written once and can only ever be read on the marker block, which has an empty `name`.
- fix: propagate `in_actions` when pushing onto the stack (inherit it from the nearest enclosing entry), and drop the per-keyword special case.
- status: open

### [BUG] A `/* */` block comment containing an unbalanced brace corrupts the member stack for the rest of the file
- where: crates/al-analysis/src/xliff.rs:342-365 (`strip_literals_for_structure`), used at 173 and 239-247
- severity: medium
- scenario: `strip_literals_for_structure` blanks single-quoted literals and stops at `//`, and handles neither `/*` nor `*/`. AL supports block comments, and `detect_object_header` (467-494) handles them, so the module knows they exist. A line such as `    /* the old layout used a { here */` pushes an extra entry onto `stack` at line 241 that is never popped. From that point every `Caption` in the file resolves its anchor one level too deep, so table fields declared after the comment get ids built from the wrong member, and the closing `}` of the object pops the wrong frame. The generated ids no longer match alc's, so those strings cannot be matched to existing translations.
- fix: track `/* */` state in `strip_literals_for_structure` the way `detect_object_header` already does, and blank the comment body.
- status: open

### [BUG] A property on the same line as its member block is attributed to the enclosing block
- where: crates/al-analysis/src/xliff.rs:172-247
- severity: low
- scenario: `anchor` is read at line 180 from the stack as it stands *before* the current line's braces are processed at 239-247. For a one-line member such as `field(1; "No."; Code[20]) { Caption = 'No.'; }`, `pending` is set at 175 but not yet pushed, so `anchor` is the enclosing `fields` frame (or `None`). The caption is emitted with the object-level id `Table <hash> - Property <hash>` instead of `Table <hash> - Field <hash> - Property <hash>`. If two one-line fields both carry a `Caption`, they produce the same object-level id and `extract_translation_units` drops the second as a duplicate.
- fix: push `pending` for braces that open before the property's position on the line, or detect the single-line form and use `pending` as the anchor when it is set on the same line.
- status: open

### [SLOP] `find_untranslated`'s doc claims a sort the code does not do
- where: crates/al-analysis/src/xliff.rs:964-978
- severity: low
- scenario: the doc comment says "Returns units where `target` is `None` or empty, sorted by object type and ID." The body is a `filter().filter().collect()` with no sort. The caller at crates/al-lsp/src/server/daemon/build_dispatch/xliff.rs:338-341 builds its input with `units_map.into_values()`, which is `HashMap` iteration order, so the `xlf.untranslated` output is in a different order on every run. The same module fixed exactly this for obsolete units at xliff.rs:944-951 with the comment "huge spurious VCS diffs".
- fix: sort by `id` (the object type and id are blank anyway, per the first finding), or correct the doc.
- status: open

### [SLOP] Dead reset of `current_target` after the trans-unit is emitted
- where: crates/al-analysis/src/xliff.rs:860
- severity: low
- scenario: `current_target = None;` runs after the `if let` block that already did `current_target.take()` at line 840, and the next `<trans-unit ` line resets it again at 800. It can only matter for a `</trans-unit>` whose `<source>` was missing, and in that case the next `<trans-unit ` clears it anyway.
- fix: delete the line.
- status: open

### [BUG] Renaming a procedure parameter is reported as a breaking change, and AL has no named arguments
- where: crates/al-analysis/src/queries/breaking_changes.rs:582-600 (`check_matching_signature`)
- severity: medium
- scenario: baseline `procedure Post(SalesHeader: Record "Sales Header")`, current `procedure Post(Header: Record "Sales Header")`. The parameter types and `var` modifiers match, so `parameter_contract_matches` finds the exact overload and `check_matching_signature` runs. Lines 585-599 then emit a `SignatureChanged` with `is_breaking: true`, which `upgrade.rs:102-108` turns into severity `error` with the hint "Update all callers of 'Post' to match the new signature". AL calls are positional only, so no caller needs to change. On any release where a developer tidies up parameter names, the breaking-change gate fails on a change that breaks nothing.
- fix: either drop the parameter-name comparison or emit it with `is_breaking: false` so it lands as a warning.
- status: open

### [BUG] `is_breaking` is `true` at every construction site, so the whole warning path is dead
- where: crates/al-analysis/src/queries/breaking_changes.rs:42-43 (declaration), every `BreakingChange` literal in the file, and crates/al-analysis/src/queries/upgrade.rs:107
- severity: medium
- scenario: the field's doc comment says "Whether this is definitely breaking (vs potentially non-breaking)", and `upgrade.rs:107` branches on it: `if change.is_breaking { "error" } else { "warning" }`. A grep across the workspace finds no `is_breaking: false` anywhere, so the `"warning"` arm cannot be reached and the field carries no information. Combined with the finding above, every detected change is reported at severity `error`, including the ones that do not break a caller.
- fix: set `is_breaking: false` on the genuinely soft cases (parameter rename, and an `ObsoleteState` that only advanced to `Pending`), or delete the field and the dead branch.
- status: open

### [GAP] Table keys and page controls are never diffed
- where: crates/al-analysis/src/queries/breaking_changes.rs:252-489 (`diff_object`)
- severity: medium
- scenario: `SymbolEntry` carries `keys` and `controls`, and `diff_object` reads neither (a grep for `.keys` and `controls` in the file hits only the test fixtures' `Vec::new()`). Baseline `table 50100 "Shipment Log" { keys { key(PK; "Entry No.") { Clustered = true } } }`, current changes the primary key to `key(PK; "Document No.", "Line No.")`. That invalidates every stored record's identity and breaks every `Get()` call in dependent apps, and `analyze_breaking_changes` returns nothing. The same holds for removing a named page control, which breaks any `pageextension` that does `addafter(ControlName)` or `modify(ControlName)`.
- fix: diff `keys` by key name and field list, and `controls` by name, emitting new `BreakingChangeKind` variants.
- status: open

### [SIMPLIFY] A field type change is reported twice by `upgrade_report`
- where: crates/al-analysis/src/queries/upgrade.rs:407-444 (`check_data_migration_needs`) duplicating crates/al-analysis/src/queries/breaking_changes.rs:403-414
- severity: low
- scenario: `field(5; Amount; Decimal)` becomes `field(5; Amount; Text[30])`. `diff_object` emits `FieldTypeChanged`, which `breaking_change_to_upgrade_issue` (upgrade.rs:144-150) maps to an `error` with the hint "Add upgrade code that preserves existing table data and update all field references." `check_data_migration_needs` then independently rebuilds the same name-keyed field maps, applies the same lowercased type comparison, and emits a second `error` saying "data migration required". One change, two errors, with near-identical advice.
- fix: drop `check_data_migration_needs` and attach the `OnUpgradePerCompany` hint to the existing `FieldTypeChanged` arm.
- status: open

### [BUG] Object-level obsolescence is never detected, because AL expresses it as a property and the scan only reads attributes
- where: crates/al-analysis/src/queries/obsolescence.rs:112-126 and 180-209 (`extract_obsolete_from_preceding_attr`)
- severity: high
- scenario: AL marks an *object* obsolete with properties inside the object body, not with an attribute:
  ```al
  table 50100 "Old Shipment Buffer"
  {
      ObsoleteState = Pending;
      ObsoleteReason = 'Use table 50101 instead';
      ObsoleteTag = '24.0';
      ...
  }
  ```
  `scan_file_for_obsolete` calls `extract_obsolete_from_preceding_attr(root, source)` with the tree *root*. `root.prev_sibling()` is `None`, so the sibling walk at 184-196 does nothing, and the child loop at 198-207 only accepts children of kind `attribute` or `attribute_list`. The object's properties are not attribute nodes, so `obj_obsolete` is always `None` and the query never emits a `kind: "object"` entry for any real AL object. The whole `ObsoleteState` branch of `parse_obsolete_attr` (214-226) is therefore unreachable, which also hides its own ordering bug: it tests `lower.contains("pending")` before `lower.contains("removed")`, so `ObsoleteState = Removed; ObsoleteReason = 'Pending removal was announced in 24.0';` would be classified `Pending`.
- fix: read the object's property block (the same `Caption`-style property scan other queries use) rather than looking for an attribute, and test for `removed` before `pending`.
- status: open

### [GAP] Obsolete table fields are never scanned, though `kind` documents "field"
- where: crates/al-analysis/src/queries/obsolescence.rs:23 (doc) and 139-178 (`scan_procedures_for_obsolete`)
- severity: high
- scenario: `ObsoleteEntry::kind` is documented as `"object"`, `"procedure"`, or `"field"`. The only two producers set `"object"` (line 118, unreachable per the finding above) and `"procedure"` (163, 82). Nothing walks `field_declaration` nodes, so
  ```al
  field(5; "Discount Amount"; Decimal) { ObsoleteState = Removed; ObsoleteReason = 'Replaced by "Line Discount Amount"'; ObsoleteTag = '23.0'; }
  ```
  produces no entry. An obsolete field is the most common obsolescence in Business Central because it is the one that forces data migration, and it is the one case the timeline query cannot see. No test covers it either (tests at 314-418 cover an obsolete procedure, a non-obsolete procedure, an empty workspace and a symbol-package method).
- fix: add a `field_declaration` arm to the tree walk that reads the field's property block, and add a test.
- status: open

### [PERF] Reference counting re-walks every workspace tree once per obsolete symbol
- where: crates/al-analysis/src/queries/obsolescence.rs:292-297 (`count_references_in_files`), called at 114 and 158
- severity: medium
- scenario: `count_references_in_files` maps `al_syntax::find_call_references(tree, text, name)` over *all* files, and it is called once per obsolete symbol found. `find_call_references` (crates/al-syntax/src/navigation.rs:283-289) is a full tree walk. A project with 2000 `.al` files and 50 obsolete procedures does 100,000 full tree walks for one `obsolescence` query. al-syntax already ships the fix and documents it for exactly this shape: `collect_call_site_names` (navigation.rs:291-303) says "Single-pass companion to `find_call_references`: instead of asking 'is this one name called here?' N times, walk the tree once and collect the full set of called names."
- fix: build one `HashSet<String>` of call-site names per file with `collect_call_site_names`, then look each obsolete symbol up in it.
- status: open

### [PERF] The obsolete-usage diagnostic snapshots the whole workspace twice and throws away the expensive half
- where: crates/al-analysis/src/queries/obsolete_usage.rs:36 and 56, reached from crates/al-analysis/src/queries/diagnostics.rs:289-290
- severity: high
- scenario: `obsolete_usages` calls `workspace_sources::snapshot(workspace)` at line 36, which clones the text and tree of every indexed `.al` file. At line 56 it then calls `obsolescence_timeline(workspace)`, whose first statement (obsolescence.rs:41) is *another* full `snapshot`. The timeline also runs `count_references_in_files` for every obsolete symbol, a full tree walk of every file per symbol (see the finding above), and `obsolete_usages` discards `caller_count` entirely: lines 57-67 read only `kind`, `file`, `symbol`, `reason` and `tag`. This runs inside `workspace_diagnostics`, which crates/al-lsp/src/server/lsp.rs:1257-1261 schedules on every `did_change` when the scope is `Project` and the trigger is `Continuous`. On a 2000-file project with 50 obsolete procedures, each debounced keystroke costs two whole-workspace snapshots plus 100,000 tree walks whose result is dropped.
- fix: give `obsolescence_timeline` a variant that takes an existing `sources` snapshot and skips reference counting, and have `obsolete_usages` call that.
- status: open

### [SLOP] `EdgeKind::TriggerInvocation` can never appear in a call graph, but four consumers branch on it
- where: crates/al-insight/src/index.rs:9-10 (module doc), 52 (variant), 215-222 (`add_trigger_invocation`); consumers at crates/al-insight/src/search.rs:142 and 430, crates/al-analysis/src/queries/test_coverage.rs:327
- severity: low
- scenario: `add_trigger_invocation` is the only function that constructs a `TriggerInvocation` edge, and a grep across the repo finds exactly one call, in its own unit test at index.rs:646. No production code path creates one. The module header still advertises it as one of "three kinds of edges tracked", `search.rs:142` includes it in the traversal filter, `search.rs:430` assigns it a hop cost of 1, and `test_coverage.rs:327` matches it when deciding what counts as coverage. All four are unreachable. The feature the doc describes, "Trigger A invokes Procedure B", is not implemented.
- fix: either wire it up where triggers are parsed (calls.rs already emits `RecordTrigger` at line 890) or delete the variant and the four dead branches.
- status: open

### [SLOP] `CallGraph::remove_edges_from` is dead, and it would leave stale resolution state if it were used
- where: crates/al-insight/src/index.rs:245-254
- severity: low
- scenario: the doc says "Used for invalidation". Its only caller is the test at index.rs:810. If it were called, it would remove the node's edges but leave `self.resolution` untouched, and `calls.rs:1878` and `1934` gate lazy edge resolution on `resolution_state(proc_id)` not being `Resolved`. An invalidated node would therefore stay marked `Resolved` and never have its edges rebuilt, leaving the node permanently edge-less. The bug is latent only because nothing calls the function.
- fix: delete it, or have it also `self.resolution.remove(&node)` and add a test that re-resolves after invalidation.
- status: open

### [BUG] `discover_events` output order is not deterministic when two nodes share an object and event name
- where: crates/al-insight/src/discovery.rs:91-97, 101-105, 157, and the sorts at 206-226
- severity: low
- scenario: `graph.index` is a `HashMap` whose values are `Vec<NodeIndex>` (line 93 iterates `indices`, so one `NodeKey::Event` can map to several nodes). Two workspace files both declaring `codeunit 50100 "Publisher"` with `[IntegrationEvent] procedure OnPost()`, which is what a half-finished copy-paste refactor looks like, produce two distinct event nodes under one key. Both become `DiscoveredEvent`s with identical sort keys, so the final `sort_by` (stable) preserves whatever order `event_subscribers`, itself a `HashMap`, happened to yield. `al subscribers` then prints the two in a different order between runs. `search.rs:373` and `xliff.rs:944-951` both call out and fix this exact nondeterminism elsewhere in the codebase.
- fix: add the node index as the final tiebreaker in the `events` and `orphans` comparators.
- status: open

### [SLOP] The event type in the JSON output is a `Debug` format of an enum
- where: crates/al-insight/src/discovery.rs:169
- severity: low
- scenario: `event_type: format!("{:?}", event_type)` puts the `Debug` rendering of the event-type enum into `PublisherInfo::event_type`, a `#[serde]`-exposed field of the `al subscribers` JSON. The tests pin the resulting strings (`"Integration"` at discovery.rs:363, `"Business"` at 378), so renaming the enum variant silently changes the public JSON contract with no compiler error. Every other serialized enum in the file uses `#[serde(rename_all = "camelCase")]`.
- fix: give the event-type enum a `Display` impl or derive `Serialize` on it and store the typed value rather than a formatted string.
- status: open

### [SIMPLIFY] The same case-insensitive string comparator is spelled out five times
- where: crates/al-insight/src/discovery.rs:174-187, 206-226, 228-241
- severity: low
- scenario: each of the five comparisons is written as `a.field.as_bytes().iter().map(u8::to_ascii_lowercase).cmp(b.field.as_bytes().iter().map(u8::to_ascii_lowercase))`, which is 34 lines of sort code for three sorts on what are two-field keys. A `fn lower_key(s: &str) -> impl Iterator<Item = u8> + '_` (or just `str::to_ascii_lowercase` on the two keys) collapses it, and the tiebreaker fix above would then have one place to go.
- fix: extract the comparator into one helper.
- status: open

### [GAP] A brand-new permission set produces one issue per permission entry
- where: crates/al-analysis/src/queries/upgrade.rs:333-385 (`detect_new_permissions`)
- severity: low
- scenario: when `surface_key(current_entry)` is absent from `baseline_map`, `old_permissions` falls back to `&[]` (line 351-354), so `old_value` is 0 for every permission and `added == permission.value` is non-zero for all of them. Adding one `permissionset 50100 "My App Objects"` that grants RIMD on 200 tables yields 200 separate `NewPermission` warnings, all saying "Review the added privilege against least-privilege and AppSource policy." The signal that matters (a *new* permission set exists) is buried in 200 identical rows.
- fix: when the permission set itself is new, emit a single issue naming the set and the number of grants, and keep the per-permission breakdown for sets that already existed.
- status: open

## Not covered by this pass

`queries/source.rs`, `queries/audit.rs`, `queries/test_diagnostics.rs`,
`queries/code_actions/test_support.rs`, `queries/suggest_event.rs`,
`queries/profiler_hints.rs`, `queries/test_coverage.rs`, and every file in the
"not listed at all" group above. `scaffold.rs` and `generators.rs` were reviewed
by a sub-agent whose report went to the orchestrator; those findings are pending
and are not in this file.

## Review complete

Rename is the weakest surface. Renaming a quoted identifier is a silent no-op,
because prepare_rename hands the editor an unquoted placeholder that the
validator then rejects, and every quotable BC name goes through that path.

Rename also skips `[EventSubscriber]` string references that Find All References
already collects, so renaming a published event leaves every subscriber pointing
at a name that no longer exists.

In resolution.rs, `parse_field_line` cuts the field declaration at the first `)`,
so any field named like `"Amount (LCY)"` is invisible to hover, go-to-definition
and completion, and no test uses a field name with punctuation.

Two hot paths do heavy synchronous work per request: member completion re-reads
and re-parses the receiver's whole symbol-package source from disk on every
keystroke, and the obsolete-usage diagnostic snapshots the whole workspace twice
per debounced change while discarding the expensive half of what it computed.

xliff.rs and obsolescence.rs both miss the normal AL spelling of what they
target: xliff drops every object after the first in a multi-object file, and
obsolescence never sees object- or field-level `ObsoleteState`, because it only
reads attributes and AL expresses both as properties.
