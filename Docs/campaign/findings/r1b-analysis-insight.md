# R1b review: al-analysis and al-insight, modules not covered by r1

Adversarial read-only review of every module marked NOT COVERED in
`r1-analysis-insight.md`, plus the files that checklist does not list at all.

## Coverage

Priority (edits user files / core resolution):
- [x] queries/rename.rs (plus queries/binding.rs, which it depends on)
- [x] resolution.rs
- [x] queries/breaking_changes.rs

Rest of scope:
- [ ] xliff.rs
- [ ] scaffold.rs  — findings pending from orchestrator
- [ ] generators.rs  — findings pending from orchestrator
- [ ] queries/source.rs
- [ ] queries/audit.rs
- [ ] queries/obsolescence.rs + queries/obsolete_usage.rs
- [x] queries/upgrade.rs
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

### [BUG] A field name containing a parenthesis is invisible to hover, go-to-definition and completion
- where: crates/al-analysis/src/resolution.rs:1517-1527 (`parse_field_line`), used by `find_workspace_field` (1600-1620) and `workspace_field_items` (1622-1638)
- severity: high
- scenario: `field(50; "Amount (LCY)"; Decimal) { }` in a workspace table. `parse_field_line` does `trimmed.strip_prefix("field(")?.split(')').next()?`, which cuts at the *first* `)`, giving `50; "Amount (LCY"`. `splitn(3, ';')` then yields only two segments, so `parts.next()?` for the type returns `None` and the whole field is dropped. `find_workspace_field` never matches it, so hover and go-to-definition on `Rec."Amount (LCY)"` return nothing, and `workspace_field_items` omits it from the `Rec.` completion list. `"Amount (LCY)"`, `"Sales (LCY)"`, `"Profit (LCY)"`, `"Qty. (Base)"` are standard Business Central field names that developers copy into custom tables. The same cut breaks any field whose name contains `;`, for example `field(1; "A;B"; Text[10])`, which yields `name_part = "A` and `ty = B"`.
- fix: `field_decl_nodes` already hands `parse_field_node` a tree-sitter node, so the id, name and type are available as child nodes. Read them from the node instead of re-splitting the text. If the text split has to stay, make it quote-aware the way `split_last` (1640-1657) already is.
- status: open

### [TEST] No `parse_field_line` test uses a quoted field name with punctuation
- where: crates/al-analysis/src/resolution.rs:2490-2510
- severity: medium
- scenario: the four `parse_field_line` tests cover `field(1; Name; Text[50])`, a non-field line, `field(1)` and an empty name. None uses a quoted name, and none uses a name containing `(`, `)` or `;`. The field-resolution tests at 1788-1860 use `Name`, `Amount`, `Qty`, `Ørnamental` and `München`, all punctuation-free. The bug above is therefore entirely invisible to the suite.
- fix: add `field(50; "Amount (LCY)"; Decimal) { }` to both `parse_field_line_extracts_name_and_type` and `workspace_field_items_lists_fields`.
- status: open

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

### [GAP] A brand-new permission set produces one issue per permission entry
- where: crates/al-analysis/src/queries/upgrade.rs:333-385 (`detect_new_permissions`)
- severity: low
- scenario: when `surface_key(current_entry)` is absent from `baseline_map`, `old_permissions` falls back to `&[]` (line 351-354), so `old_value` is 0 for every permission and `added == permission.value` is non-zero for all of them. Adding one `permissionset 50100 "My App Objects"` that grants RIMD on 200 tables yields 200 separate `NewPermission` warnings, all saying "Review the added privilege against least-privilege and AppSource policy." The signal that matters (a *new* permission set exists) is buried in 200 identical rows.
- fix: when the permission set itself is new, emit a single issue naming the set and the number of grants, and keep the per-permission breakdown for sets that already existed.
- status: open
