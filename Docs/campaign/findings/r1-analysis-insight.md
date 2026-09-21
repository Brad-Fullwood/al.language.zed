# R1 review: al-analysis and al-insight

Adversarial read-only review. Baseline: AUDIT-BACKLOG.md section "Analysis & Insight" (2026-07-31).
Findings marked [STILL-OPEN] were in that audit and are still present in current code.

## Coverage

Code actions (edit user files) — highest priority:
- [ ] queries/code_actions/mod.rs
- [ ] queries/code_actions/add_parens.rs
- [ ] queries/code_actions/if_to_case.rs
- [ ] queries/code_actions/with_elimination.rs
- [ ] queries/code_actions/implement_interface.rs
- [ ] queries/code_actions/events.rs
- [ ] queries/code_actions/promoted.rs
- [ ] queries/code_actions/make_local.rs
- [ ] queries/code_actions/namespace.rs
- [ ] queries/code_actions/doc_region.rs
- [ ] queries/code_actions/test_support.rs
- [x] queries/bulk_fix.rs
- [ ] queries/rename.rs
- [ ] generators.rs / scaffold.rs (file-writing)
- [ ] xliff.rs (writes user .xlf)

Diagnostics:
- [ ] queries/diagnostics.rs
- [ ] queries/sql_patterns.rs
- [ ] queries/dead_code.rs
- [ ] queries/arch_lint.rs
- [ ] queries/transaction_lint.rs
- [ ] queries/native_check.rs
- [ ] queries/audit.rs
- [ ] queries/breaking_changes.rs
- [ ] queries/obsolescence.rs / obsolete_usage.rs
- [ ] queries/test_diagnostics.rs
- [x] queries/duplicates.rs
- [ ] queries/upgrade.rs
- [x] queries/impact.rs
- [x] permissions.rs
- [x] workspace_sources.rs
- [ ] resolution.rs
- [ ] queries/source.rs
- [ ] queries/inlay_hints.rs, code_lens.rs, hover.rs, completions.rs, signature.rs
- [ ] queries/suggest_event.rs, profiler_hints.rs, test_coverage.rs

Insight:
- [x] al-insight/src/graph.rs
- [ ] al-insight/src/calls.rs
- [ ] al-insight/src/search.rs
- [x] al-insight/src/analysis.rs
- [ ] al-insight/src/index.rs
- [ ] al-insight/src/discovery.rs

## Findings

### [BUG] Eliminate-with drops the statement's terminating semicolon on the single-statement form
- where: crates/al-analysis/src/queries/code_actions/with_elimination.rs:171-183 (edit range) and 206-229 (`extract_with_body`)
- severity: high
- scenario: `with Cust do Name := 'X';` followed by `Message('done');`. `tree-sitter-al/grammar.js:981` defines `with_statement` without a trailing semicolon (the `;` is consumed by `statement_list`, grammar.js:860-866), so `with_node` ends at `'X'`. `extract_with_body` returns the body text `Name := 'X'` with no `;`, and the edit replaces the whole line range `(start_row, 0)..(end_row + 1, 0)`, which deletes the `;` that lived outside the node. Result is `Cust.Name := 'X'` followed by `Message('done');`, a parse error. The begin/end form happens to work because each inner statement carries its own `;`. if_to_case.rs:65-68 was fixed for exactly this and with_elimination was not.
- fix: use the node's own start/end columns for the edit range as if_to_case.rs:74-92 does, and append `;` to the last emitted line when the source form was a single statement (`extract_with_body` already returns `is_begin_end`, currently discarded at line 156 as `_is_begin_end`).
- status: open

### [BUG] Eliminate-with deletes any code before `with` on the same line
- where: crates/al-analysis/src/queries/code_actions/with_elimination.rs:172-181
- severity: high
- scenario: `if Found then with Cust do begin` ... `end;`. The edit range starts at `character: 0` of `with_node.start_position().row`, not at the node's own column, so applying the action deletes the `if Found then ` prefix. The body statements are then emitted unconditionally, changing control flow as well as breaking the parse if the `if` had an `else`.
- fix: start the edit at `byte_col_to_utf16_col(start_line, with_node.start_position().column)` and end at the node's end column, the same pattern if_to_case.rs:74-92 uses.
- status: open

### [BUG] Eliminate-with flattens all nesting inside the with body to one indent level
- where: crates/al-analysis/src/queries/code_actions/with_elimination.rs:247-258
- severity: medium
- scenario: `with Cust do begin if Amount > 0 then begin Name := 'X'; Modify(); end; end;`. `qualify_with_references` emits `format!("{}{}\n", indent, trimmed)` for every line, where `indent` is the `with` statement's own indent. Every nested `begin`/`if` body comes back at the same column, so a 3-level-deep body is returned as a flat block. The same defect was fixed in if_to_case.rs:117-154 (`push_branch_body` preserves relative indentation); with_elimination still has the original code.
- fix: reuse the relative-indent logic from `push_branch_body`: measure the minimum indentation across continuation lines and shift each line by its offset from that minimum.
- status: open

### [BUG] Make-local edits the attribute line when a procedure has attributes, rewriting text inside the attribute
- where: crates/al-analysis/src/queries/code_actions/make_local.rs:82-133
- severity: high
- scenario: `tree-sitter-al/grammar.js:755-760` puts `repeat($.attribute)` inside `procedure_declaration`, so `node.start_position().row` is the *attribute* line, not the `procedure` line. Given
  ```
      [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPostProcedure', '', false, false)]
      procedure Handle()
  ```
  `line_text` is the attribute line, `lower[declaration_col..].find("procedure")` matches the `Procedure` inside `'OnAfterPostProcedure'`, and the edit replaces those nine characters with `local procedure`. The file becomes `'OnAfterPostlocal procedure'` and the procedure is still global. The same happens with any attribute containing the word "procedure", such as `[Obsolete('Use the other procedure instead', '25.0')]`. For an attribute *without* the word (`[NonDebuggable]`), `find("procedure")?` returns None and the action is silently never offered on attributed procedures.
- fix: locate the `kw_procedure`/`kw_function` child node of the declaration (or the first `member_modifier`) and derive `proc_line`/columns from that node rather than from `node.start_position().row` and a substring search.
- status: open

### [PERF] Make-local lowercases every indexed workspace file on every code-action request
- where: crates/al-analysis/src/queries/code_actions/make_local.rs:15-53, called unconditionally from mod.rs:93
- severity: medium
- scenario: `source_actions` runs on every `textDocument/codeAction` request, which editors fire as the cursor moves. With the cursor anywhere inside a procedure body, `external_caller_exists` iterates `workspace.file_index.files` and calls `entry.value().to_lowercase()` on the full text of every file, allocating a fresh lowercase copy of the entire workspace source each time. On a 47k-line project that is megabytes of allocation per cursor move.
- fix: hold a per-file lowercase cache (or a lowercase identifier set) in `FileIndex`, or at minimum use `entry.value().len()` and a case-insensitive substring scan that does not allocate. `make_local` could also reuse the reference index instead of a raw text scan.
- status: open

### [BUG] Move-ToolTip computes the table-side edit against the on-disk text, not the open buffer
- where: crates/al-analysis/src/queries/code_actions/events.rs:55-69
- severity: medium
- scenario: the table's text comes from `workspace.file_index.files` (the indexed on-disk snapshot), and `find_table_field_declaration` / `annotation_edit` produce a line number and column against that snapshot. If the table file is also open in the editor with unsaved edits that shift line numbers (say three lines inserted above the field), the client applies the returned `TextEdit` to the *buffer*, so the `ToolTip = ...;` lands three lines off, inside a different field block or mid-property. The page-side delete uses the live document text, so the two halves disagree about which version of the world they are editing.
- fix: prefer `workspace.documents.get_text_arc(&table_uri)` when the table is an open document and fall back to the file index only for closed files.
- status: open

### [BUG] Promoted-action conversion counts braces inside comments and captions
- where: crates/al-analysis/src/queries/code_actions/promoted.rs:159-185 (`find_block_extent`)
- severity: medium
- scenario: `find_block_extent` iterates the raw characters of each line and counts every `{` and `}`. A line inside an action body such as `// TODO: rework the { } layout` or `Caption = 'Open }';` shifts the depth counter, so `body_end_line` lands on the wrong line. Downstream that means `remove_lines` can miss the `Promoted = true` line (the action is then not offered, or worse the `area(Promoted)` insert point computed from `actions_block.close` is off) and the generated page no longer parses. events.rs:181-204 already has `strip_literals_and_comment` for exactly this problem; promoted.rs duplicates the brace counting without it.
- fix: move `strip_literals_and_comment` into the shared `code_actions/mod.rs` and run every line through it before counting braces in `find_block_extent` and `find_block_in`.
- status: open

### [GAP] Report-layout conversion does not check for an existing `rendering` section
- where: crates/al-analysis/src/queries/code_actions/promoted.rs:578-671
- severity: low
- scenario: `find_rendering_insert_line` only looks for `requestpage` or the last bare `}`. A report that already has a `rendering { }` section plus one legacy `WordLayout = '...';` property gets a *second* `rendering` block inserted, which alc rejects as a duplicate section. The conversion should merge the new `layout(...)` into the existing section.
- fix: scan for an existing `rendering` header first and, when found, insert the `layout(...)` entries inside it rather than emitting a new `rendering` block.
- status: open

### [BUG] Bulk tooltip fix prepends "Specifies" to text that already starts with "Specifies"
- where: crates/al-analysis/src/queries/bulk_fix.rs:674-679
- severity: high
- scenario: `inject_tooltips` emits `format!("ToolTip = 'Specifies {escaped}';")`, hardcoding the "Specifies " prefix. The only production caller (crates/al-lsp/src/server/daemon/build_dispatch/fixes.rs:681-691, driven by `al-explorer fix tooltips --from-table Customer`) takes the *base-app table field's own `ToolTip` property value*, which by Microsoft's own convention (UICop AA0218) already reads `Specifies the number of the customer.`. Running `fix.tooltips` non-dry-run writes `ToolTip = 'Specifies Specifies the number of the customer.';` into every matching page field across the project, atomically and with no undo. The doc comment at bulk_fix.rs:77-81 describes `tooltips` as "tooltip text", not a sentence fragment.
- fix: emit the tooltip value verbatim and leave prefixing to the caller, or prepend "Specifies " only when the value does not already begin with it (case-insensitive).
- status: open

### [TEST] The bulk tooltip test never asserts the generated text
- where: crates/al-analysis/src/queries/bulk_fix.rs:956-975 (`inject_tooltips_adds_missing_tooltip`)
- severity: medium
- scenario: the test passes the tooltip `"the item number"` and asserts only `result.contains("ToolTip")`. Because it never compares the emitted string, the hardcoded `Specifies ` prefix above is invisible to the suite. Every other bulk-fix test asserts the re-parse, not the content.
- fix: assert the full emitted line, and add a case whose input already starts with "Specifies".
- status: open

### [GAP] Impact analysis still misses conditional `TableRelation` branches
- where: crates/al-analysis/src/queries/impact.rs:346-349, calling crates/al-insight/src/analysis.rs:200-202
- severity: medium
- scenario: the audit's conditional-`TableRelation` fix added `extract_table_relation_tables` (plural), but `check_object_consumers` still calls the singular `extract_table_relation_table`, which is `extract_table_relation_tables(value).into_iter().next()`. Line 250 of analysis.rs sorts the branch tables alphabetically before returning them, so for `Sales Line."No."` with `TableRelation = IF (Type=CONST(Item)) Item."No." ELSE IF (Type=CONST(Resource)) Resource."No."` the singular helper returns `Item`. An impact query on `Resource` never lists `Sales Line` as a `Filter` consumer, and a query on `Item` succeeds only by alphabetical accident.
- fix: have `check_object_consumers` call `extract_table_relation_tables` and test every branch with `eq_ignore_ascii_case`.
- status: open

### [BUG] `extract_table_relation_tables` sorts its result while its doc says declaration order
- where: crates/al-insight/src/analysis.rs:204 (doc) vs 250-252 (code)
- severity: low
- scenario: the doc comment says "Every table referenced by a `TableRelation` value, **in declaration order**", but the function ends with `tables.sort_unstable(); tables.dedup();`. `extract_table_relation_table` then documents itself as the "single-table helper" while actually returning the alphabetically first branch. Any caller that treats element 0 as the primary relation gets the wrong table.
- fix: either drop the sort and dedup while preserving order, or correct both doc comments and make the singular helper explicit about which table it returns.
- status: open

### [GAP] One unparseable `.al` file aborts the whole permission-set generation
- where: crates/al-analysis/src/permissions.rs:66-92
- severity: medium
- scenario: `collect_permissions` walks every indexed file and returns `Err(PermissionCollectionError::ParseSource)` on the first file whose cached tree has an error node. A single scratch or work-in-progress `.al` in the workspace therefore blocks permission generation for the entire project. `workspace_sources::snapshot_with_skipped` (workspace_sources.rs:88-113) was changed to *skip* such files and report them, but permissions.rs keeps its own whole-workspace failure.
- fix: route `collect_permissions` through `workspace_sources::snapshot_with_skipped`, or apply the same skip-and-report policy so one broken file does not take the query down.
- status: open

### [BUG] Add-parentheses is never offered for `Rec.` / `CurrPage.` member calls
- where: crates/al-analysis/src/queries/code_actions/add_parens.rs:73-110
- severity: low
- scenario: `CurrPage.Update;` or `Rec.Modify;` — both are bare parameterless calls that AL0604 asks you to parenthesise. `is_callable_identifier_path` splits on `.` and rejects the whole path if *any* segment is in `NON_CALL_STATEMENT_WORDS`, which contains `rec`, `xrec`, `currpage`, `currreport`, `currxmlport`. Those words are only non-calls when they stand alone, so the guard added to stop `end();` also suppresses the action on the most common real targets.
- fix: only reject those words when the path has a single segment (`segments == 1`), or check the list against the full `body` rather than each segment.
- status: open

