# R1c review: al-analysis and al-insight, the remainder

Adversarial read-only review of every file under `crates/al-analysis/src` and
`crates/al-insight/src` that neither `r1-analysis-insight.md` nor
`r1b-analysis-insight.md` ticked. `scaffold.rs` and `generators.rs` are out of
scope here (covered by `r1b-scaffold-generators.md`).

## Coverage

Explicitly named as unreviewed:
- [x] queries/source.rs
- [x] queries/audit.rs
- [x] queries/test_diagnostics.rs
- [x] queries/code_actions/test_support.rs
- [x] queries/suggest_event.rs
- [x] queries/profiler_hints.rs
- [x] queries/test_coverage.rs

Listed by neither checklist:
- [x] lsp.rs
- [x] queries/mod.rs
- [x] queries/complexity.rs
- [x] queries/definition.rs
- [x] queries/deps.rs
- [x] queries/folding.rs
- [x] queries/format.rs
- [x] queries/implementation.rs
- [x] queries/references.rs
- [x] queries/search.rs
- [x] queries/semantic_tokens.rs
- [x] queries/symbols.rs
- [x] queries/code_lens.rs
- [x] queries/hover.rs
- [x] queries/completions.rs
- [x] queries/signature.rs
- [x] al-analysis/src/lib.rs, al-insight/src/lib.rs

## Findings

### [BUG] `al source` on any object but the first in a multi-object file returns the wrong id and the whole file
- where: crates/al-analysis/src/queries/source.rs:246-262 (`source_candidates`) and 327-349 + 388-400 (`try_workspace_source`)
- severity: high
- scenario: a file `Shipment.al` declaring `table 50100 "Shipment Header"` followed by `table 50101 "Shipment Line"`. `FileIndex::add_file_with_tree` (crates/al-source/src/file_index.rs:525-553) indexes *both* names into `objects`, so `object_paths("Shipment Line")` returns `Shipment.al`. But the query then reads `file_index.object_info` (the singular map, which file_index.rs:549-551 documents as "the first declaration in document order") and `al_syntax::find_object_declaration`, which also returns only the first. So `al source "Shipment Line"` reports `k: Table, id: 50100`, the *header's* id, under the name `Shipment Line`, and `code` is the text of the whole file including both tables. When the two objects have different kinds (`table` then `page`, the common setup-table-plus-card file), `declared_kind != kind` at line 338 fires and the query returns `InvalidWorkspaceDeclaration`. With an explicit `--kind page` the candidate is dropped at line 259 and the answer is `ObjectNotFound`. `file_index.object_infos` (the plural map, file_index.rs:167) already holds every declaration with its own kind and is never consulted here.
- fix: select the `CachedObjectInfo` from `object_infos` whose `name` matches the requested name, and slice `code` to that object's node range rather than returning `text.clone()`.
- status: fixed 4180a321 — candidate selection and the declaration re-read both match on name, `code` is sliced to the object's node range, and the member search runs from that object's node. Added `al_syntax::find_object_declarations` and `FileIndex::{object_infos_in, object_info_named, object_info_at_byte}`.

### [BUG] The reported signature of an attributed procedure is the attribute, truncated
- where: crates/al-analysis/src/queries/source.rs:566-599 (`extract_signature_from_text`), reached from `find_member_node` at 550 and `extract_member_from_text` at 604
- severity: medium
- scenario: `tree-sitter-al/grammar.js:755-757` puts `repeat($.attribute)` inside `procedure_declaration`, so the node text of
  ```al
  [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
  procedure MyHandler(var SalesHeader: Record "Sales Header")
  ```
  starts with the attribute. `extract_signature_from_text` scans for the first paren-balanced `(...)`, which is the attribute's argument list, so it stops at the `)` before `]` and returns `[EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)`, the attribute with its closing bracket cut off, and no procedure signature at all. Every subscriber, every `[IntegrationEvent]` publisher and every `[Test]` procedure reports this as `sig` from `al source --proc`. The `range.l` returned alongside it is the attribute's line for the same reason.
  The eight `extract_signature_*` tests (source.rs:1156-1250) all feed bare `procedure ...` text, so none of them sees an attribute.
- fix: take the signature from the declaration's own child nodes (`kw_procedure`/`kw_function`, the `name` field, the `parameters` field and the optional `return_type` field) instead of re-scanning the node's text. If the text scan stays, skip leading `[...]` attribute blocks first.
- status: open

### [BUG] One `system` permission grant makes the whole permission audit fail
- where: crates/al-analysis/src/queries/audit.rs:768-777 (`parse_permission_clause`), propagated through `extract_permission_grants` (753-758) to `permission_set_audit` (277)
- severity: high
- scenario: AL's `Permissions` property accepts `system` as an object type alongside tabledata/table/report/codeunit/xmlport/page/query, and real permission sets use it: `Permissions = tabledata Customer = RIMD, system "Tools, Debugger" = X;`. `parse_permission_clause` matches the lowercased type against a seven-entry list with no `system` arm, so the `other` arm returns `unsupported permission object type 'system'`. `extract_permission_grants` turns that into `AuditError::InvalidSource`, and `permission_set_audit` returns `Err` on the `?` at line 277. One `system` grant anywhere in the project therefore takes down coverage, over-broad and over-granted-rights for the entire workspace, not just that one clause. The rights check makes it worse: `system` grants are written `= X`, which the non-tabledata branch would accept, so only the type list is in the way.
- fix: add `"system" => "System"` to the type match and skip system grants in the usage scans (they have no workspace object to reference). More generally, collect per-clause parse failures into the report instead of aborting the audit, the way `whole_workspace_audits_skip_malformed_source` (audit.rs:1116-1135) already does for unparsable files.
- status: fixed 401e5022 — `system` parses and is skipped in the usage scans, and any clause that still fails degrades on its own into a new `parseIssues` report section instead of aborting the audit.

### [BUG] Over-granted-rights reports rights that are in use when the write sits in a repeated-name trigger
- where: crates/al-analysis/src/queries/audit.rs:570-611 (`collect_observed_writes`) and 626-657 (`collect_procedure_names`)
- severity: high
- scenario: `collect_procedure_names` de-duplicates by lowercased name, and `al_insight::calls::find_procedure_node` (crates/al-insight/src/calls.rs:184-193, via `find_procedure_in_node`) returns the *first* declaration with that name. AL repeats trigger names constantly: every field has its own `trigger OnValidate()`, every page action its own `trigger OnAction()`. Given
  ```al
  page 50100 "Ship Card" {
      actions { area(Processing) {
          action(Preview) { trigger OnAction() begin Message('x'); end; }
          action(Post)    { trigger OnAction() var L: Record "Ship Log"; begin L.Insert(); end; }
      } }
  }
  ```
  only the `Preview` trigger is ever scanned, so no write site for `Ship Log` is recorded. `compute_over_granted_rights` then reports `I` as over-granted on `tabledata "Ship Log" = RIMD` with the reason "no write site for I found in workspace". Acting on that removes a permission the page needs and the action fails at runtime with a permission error. The module doc at audit.rs:16-19 and the struct doc at 205-212 both promise the opposite ("false positives avoided", "observed writes are never reported as removable rights"); the comment at 628-631 calls the skip "the documented precision limit" without noting that it inverts the direction of the error.
- fix: walk the declaration nodes directly and scan every one, rather than collecting names and re-finding a single node per name. `al_insight::calls` would need node-taking variants of `extract_procedure_var_types` and `extract_call_sites`; both already work from a `proc_node` internally.
- status: fixed 5996424f — the scan walks declaration nodes through new `al_insight::calls::{collect_declaration_nodes, procedure_var_types_in_node, call_sites_in_node}`. A write through an unresolvable receiver (`RecordRef`) now keeps its right for every table instead of leaving it reportable.

### [PERF] Every grant costs two full-workspace tree walks, and the second is a recomputation of the first
- where: crates/al-analysis/src/queries/audit.rs:446-451 (`count_object_refs`), called at 423 (`compute_over_broad`) and again at 506 (`compute_over_granted_rights`)
- severity: medium
- scenario: `count_object_refs` runs `al_syntax::find_variable_references` over every non-permissionset file, which is a full tree walk each. `compute_over_broad` calls it once per unique grant; `compute_over_granted_rights` then calls it again with the same argument for every `tabledata` grant, recomputing a number the first pass already had. A permission set with 300 tabledata grants over a 2000-file project is 300 x 2000 walks for the first check plus another 300 x 2000 for the second. `collect_observed_writes` (570-611) adds its own quadratic term: for each file it calls `extract_procedure_var_types` and `extract_call_sites` once per procedure name, and each of those re-walks that file's tree from the root to find the procedure.
- fix: collect every referenced identifier name per file once (`al_syntax::collect_call_site_names` has the same shape and its doc, navigation.rs:291-303, argues exactly this point) into one multiset, then look each grant up in it. Pass the counts from `compute_over_broad` into `compute_over_granted_rights` rather than recomputing them.
- status: open

### [BUG] The "unknown location" line-0 sentinel lands on the codeunit header anyway
- where: crates/al-analysis/src/queries/test_diagnostics.rs:92-105, consumed by crates/al-lsp/src/server/diagnostics.rs:749-754 (`test_diag_to_lsp`)
- severity: medium
- scenario: the comment at test_diagnostics.rs:92-98 explains the choice of `0` for a test method that the static discovery did not see: "fall back to line 0 — matching the documented 'unknown location' contract ... Line 1 would point at the codeunit header, misleading jump-to-diagnostic." But `line` is documented as 1-based and `test_diag_to_lsp` converts with `td.line.saturating_sub(1)`, so `0` and `1` both become LSP line 0, the codeunit header. A test method present in the BC run results but missing from discovery (added since the last index, or a file whose parse failed) produces `file: "/src/MyTests.al", line: 0`, which the editor renders as a red squiggle on `codeunit 50100 "My Tests"`. The exact outcome the comment says it is avoiding. `discovered_codeunit_undiscovered_method_falls_back_to_line_zero` (373-398) asserts the sentinel and never follows it through the conversion.
- fix: make the unknown-location case explicit rather than numeric, for example `line: Option<u32>`, and have the LSP layer skip publishing (or attach to the file with no range) when it is `None`.
- status: open

### [TEST] No test asserts that a failing test resolves to its source line
- where: crates/al-analysis/src/queries/test_diagnostics.rs:234-291, 332-337
- severity: medium
- scenario: the module's stated job is "Failing tests become error-severity diagnostics pointing at the procedure declaration line inside the source file" (lines 5-6). Every `results_to_diagnostics` test builds `al_workspace::Workspace::new()`, an empty workspace, so `discover_tests` returns nothing, `cu_info` is `None` and every diagnostic comes out with `file: ""` and `line: 0`. The tests then assert only severity, message and `test_name`. `unrun_test_hints_returns_hints_for_empty_workspace` likewise asserts that an empty workspace produces no hints. The lookup at lines 91-105, which is the only logic in the module, could return a constant and every test would still pass. At the LSP boundary `file: ""` fails `Url::from_file_path` (crates/al-lsp/src/server/diagnostics.rs:776-781), so those diagnostics are dropped with a warning and never reach the editor.
- fix: build a workspace holding a `[Test]` codeunit, run `results_to_diagnostics` against a `TestCodeunitResult` with the matching id, and assert the returned `file` and `line` point at the procedure.
- status: open

### [SLOP] Three of the module's six public functions have no caller
- where: crates/al-analysis/src/queries/test_diagnostics.rs:136-138 (`clear_diagnostics`), 143-163 (`unrun_test_hints`), 179-189 (`find_proc_line`)
- severity: low
- scenario: a workspace-wide grep finds callers only for `group_by_file` and `results_to_diagnostics`. `clear_diagnostics` is `pub fn clear_diagnostics() -> Vec<TestDiagnostic> { Vec::new() }` with a test that asserts the empty vec is empty. `find_proc_line`'s own doc says it is "Used when the workspace file index is not available (e.g., in tests)", and that is its only use. The clearing path the dead function was presumably for is also broken on the consumer side: `publish_test_diagnostics` (crates/al-lsp/src/server/diagnostics.rs:766-786) says "Pass an empty `diagnostics` slice to clear test diagnostics", but an empty slice makes `group_by_file` return an empty map, so the publish loop never runs and no file is ever cleared.
- fix: delete the three functions, and if clearing is wanted, have the LSP layer track the files it last published to and publish an empty diagnostic list to each.
- status: open

### [TEST] The shared apply-and-reparse helper covers seven of eleven code actions, and would mis-apply a multi-file edit
- where: crates/al-analysis/src/queries/code_actions/test_support.rs:10-11 (the claim) and 125-128 (the loop)
- severity: medium
- scenario: the module doc says the helper "is used by the tests for every code action that produces AL source". It is called from add_parens, if_to_case, with_elimination, implement_interface, promoted, make_local and doc_region. It is called from neither `events.rs` (Move-ToolTip, the only action that edits two files) nor `namespace.rs` nor `bulk_fix.rs`, all of which write AL source. The gap lines up with the open r1 findings: the Move-ToolTip edit-range bug (events.rs:55-69) is in exactly the action the helper never sees.
  If it were used there it would not help, because the loop
  ```rust
  for (_, edits) in &workspace_edit.changes { updated = apply_text_edits(&updated, edits); }
  ```
  discards the URI and applies every file's edits to the same `source`. A two-file action would have the table's edits, whose line numbers are relative to the table file, spliced into the page's text before the re-parse, so the assertion result says nothing about either file.
- fix: key the helper on a map of `uri -> source` and apply each change list to its own document, then extend it to events.rs, namespace.rs and bulk_fix.
- status: open

### [BUG] The generated `[EventSubscriber]` for a table event does not compile
- where: crates/al-analysis/src/queries/suggest_event.rs:676-681 (`format_example`)
- severity: high
- scenario: `format_example` uses the same string for both the `ObjectType::` argument and the object-reference argument:
  ```rust
  "[EventSubscriber(ObjectType::{kind_str}, {kind_str}::\"{object_name}\", ...)]"
  ```
  AL does not name a table that way in the second argument. A table subscriber is written `[EventSubscriber(ObjectType::Table, Database::"Sales Header", 'OnAfterInsertEvent', '', false, false)]`, and every fixture in this repo spells it that way (crates/al-analysis/src/queries/code_actions/events.rs:459, dead_code.rs:1260, transaction_lint.rs:859, crates/al-insight/src/calls.rs:2637, crates/al-syntax/src/sort.rs:541). `suggest_event` emits `Table::"Sales Header"`, which alc rejects, because `Table` is not an object-reference scope. `IntegrationPoint::example` is documented as a "Ready-to-paste [EventSubscriber] attribute" and the `{type:'table'}` query source exists specifically to produce table events, so the paste-ready output is wrong on the query's main path. `TableExtension`, `PageExtension` and the other extension kinds are wrong the same way.
- fix: map `ObjectKind` to its AL object-reference scope (`Table`/`TableExtension` -> `Database`, `Page` -> `Page`, `Codeunit` -> `Codeunit`, `Report` -> `Report`, `XmlPort` -> `Xmlport`, `Query` -> `Query`) and use that for the second argument only. No test asserts the text of `example`, so add one per kind.
- status: open

### [BUG] Which integration points `suggest_event` returns depends on HashMap iteration order
- where: crates/al-analysis/src/queries/suggest_event.rs:259 and 602 (`insight.index.keys()`), 471-486 (`trace_from_node`'s shared `visited` plus `depth >= max_depth`)
- severity: medium
- scenario: `insight.index` is a `HashMap<NodeKey, Vec<NodeIndex>>` (crates/al-insight/src/graph.rs:125). `query_procedure` without a procedure name iterates its keys and traces from each match, and `visited` is shared across all of those traces while `max_depth` is 10. A procedure node X reached first at depth 9 is inserted into `visited`, its own callees are then refused at depth 10, and a later start that reaches X at depth 1 hits `if visited.contains(&edge.to) { continue; }` and skips it, so every event below X is missing from the result. Which start goes first is HashMap order, so the same query over the same unchanged workspace can return different sets of integration points on different runs of the process, and `partial` stays `false` because the depth cut-off never sets it. Even when the set is stable, `points` is never sorted, so the JSON array order changes run to run and `dedup_points` keeps whichever duplicate arrived first, which decides the `path` breadcrumb the user is shown.
- fix: iterate the index through a sorted key list, sort `integration_points` before returning (by object then event, case-insensitively), track the best depth per node instead of a plain visited set, and set `partial = true` when the depth limit prunes a branch.
- status: open

### [GAP] `filterField` matches parameter names and type names, not fields
- where: crates/al-analysis/src/queries/suggest_event.rs:712-720 (`apply_filters`), described at crates/al-lsp/src/server/mcp.rs:722-724
- severity: medium
- scenario: the MCP tool description says `filterTable` / `filterField` "restrict results to events exposing that table (field) as a `var` parameter", and `al-explorer`'s `--field` feeds it. The implementation is `param.name.to_lowercase().contains(fld) || param.type_name.to_lowercase().contains(fld)`, with no `is_var` check and no notion of a table field at all, since `ParamInfo` carries only a name, a type and `is_var`. So `--field Amount` against `OnAfterPostSalesDoc(var SalesHeader: Record "Sales Header")` returns nothing, although `Sales Header` has an `Amount` field, while `--field Record` returns every event with any record parameter and `--field e` returns nearly everything. The filter cannot do what it says without resolving the record type's fields through the symbol index.
- fix: resolve each `Record "T"` parameter's fields through `workspace.symbols` and match the field name, or drop `filterField` and the documentation that promises it.
- status: open

### [BUG] A profiler hint on an attributed procedure resolves to the attribute line
- where: crates/al-analysis/src/queries/profiler_hints.rs:456 (`collect_procs`), against the module's stated rule at line 34
- severity: medium
- scenario: `let line = node.start_position().row as u32 + 1;` takes the row of the `procedure_declaration` node, and `tree-sitter-al/grammar.js:755-757` puts `repeat($.attribute)` inside that node, so for
  ```al
  [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
  procedure HandlePost()
  ```
  the resolved line is the attribute line, one (or more, with several attributes) above the signature. The module header lists "Hints on a procedure's signature line, not body line" and "Must not show hints on wrong lines" as correctness rules. Subscribers and `[Test]` procedures are exactly the procedures that dominate a BC profile, so the common case is the broken one. The sibling function `collect_profiler_lenses` (537-548) gets this right by using `name_node.start_position().row`, so the two location paths in the same file disagree about where a procedure starts.
- fix: use `name_node.start_position().row` in `collect_procs`, matching `collect_profiler_lenses`. The tests (profiler_hints.rs:686-700, 984-1020) all use unattributed procedures, so add one with an attribute.
- status: open

### [SLOP] The profile's recording duration is computed and then discarded
- where: crates/al-analysis/src/queries/profiler_hints.rs:182-188
- severity: low
- scenario: `start_us`, `end_us` and `duration_ms` are read from the profile and the next line is `let _ = duration_ms; // total recording duration, kept for context only`. Nothing else in the function or the crate uses any of the three. `ProfilerHint` has no duration field and `ProfilerSession::new` takes only a path and the hints.
- fix: delete the three bindings, or put the duration on `ProfilerSession` where a percentage-of-total hint could use it.
- status: open

### [SLOP] The code lens reports sample counts as call counts
- where: crates/al-analysis/src/queries/profiler_hints.rs:584-589 (`profiler_lens_title`)
- severity: low
- scenario: the lens renders `⏱ {ms}ms · {calls} {call_word}` from `hint.hit_count`, which `parse_profile` reads straight from the profile node's `hitCount` (line 215-218). In a Chrome-format CPU profile, and in the `.alcpuprofile` the module header documents, `hitCount` is the number of *samples* whose top frame was this node, not the number of invocations. A procedure called once that runs for 300 ms shows as "300 calls"; a procedure called a million times that never lands on a sample shows as "0 calls" and is skipped entirely by the `hit_count == 0 && self_time_ms == 0.0` filter at line 235.
- fix: label it "samples", or drop the count from the lens and show self time only.
- status: open

### [BUG] The "conservative direct pass" matches four node kinds the grammar never produces, so it is dead
- where: crates/al-analysis/src/queries/test_coverage.rs:548-551, and everything it feeds: `collect_called_identifiers` (512-531), `collect_identifiers_recursive` (533-618), `find_callee_name` (620-633)
- severity: high
- scenario: the walk credits a call only when `node.kind()` is `"method_call"`, `"function_call"`, `"invocation_expression"` or `"call_expression"`. None of those four is a node type in tree-sitter-al: `grep -c` over `tree-sitter-al/grammar.js` returns 0 for each, and none appears in `tree-sitter-al/src/node-types.json`. The grammar spells calls with `postfix_expression` + `call_suffix` / `member_call_suffix`, which is what `crates/al-syntax/src/navigation.rs:282-303` matches. So the condition is never true, `collect_called_identifiers` always returns `(vec![], vec![])`, and every `TestCoverageEntry` leaves `collect_coverage_from_tree` with empty `covers`. All actual coverage comes from `augment_coverage_with_call_graph`. About 110 lines are unreachable, including the same-object candidate preference at 562-573 and the `"bare call matches multiple declarations; no candidate was credited"` unresolved reason at 595, which can never be emitted. The module header still describes this as one of the two passes ("A conservative direct pass handles uniquely named bare calls").
  The tests do not catch it: every coverage assertion (963-1088 and the later indirect-dispatch cases) goes through the graph pass, and `find_callee_name_returns_first_identifier` (886-918) calls the helper directly from its own tree walk instead of through `collect_identifiers_recursive`.
- fix: delete the direct pass and the module doc sentence describing it, or match the real node kinds. If it stays, note that `augment_coverage_with_call_graph` returns early when `coverage.is_empty()`, so the two passes are not independent.
- status: fixed 334afb5c — the pass now matches `postfix_expression` + `call_suffix`, and `crates/al-analysis/tests/node_kind_literals.rs` fails on any kind literal the grammar does not define. That guard found seven more dead branches (`attribute_list`, `property`, `local`, `line_comment`, `block_comment`, `call_arguments`, `return_type`), fixed in the same commit.

### [GAP] Test handler functions are reported as untested production procedures
- where: crates/al-analysis/src/queries/test_coverage.rs:176-189 (`untested` filter) and 385 (`is_test`)
- severity: medium
- scenario: `is_test` is true only for a procedure carrying a `[Test]` attribute inside a `Subtype = Test` codeunit, so `untested` includes every other non-local procedure in that codeunit. AL's test framework invokes handlers by attribute, never by a call:
  ```al
  [MessageHandler]
  procedure HandleMessage(Message: Text)
  begin
  end;
  ```
  It is not `local` (the framework requires it be reachable), nothing calls it, so it lands in `untested` on every run, alongside `[ConfirmHandler]`, `[ModalPageHandler]`, `[PageHandler]`, `[ReportHandler]`, `[RequestPageHandler]` and `[SendNotificationHandler]`. A test suite of any size therefore has its handlers reported as untested production code. The suite's own test (744-750) blesses the generic helper case but says nothing about handlers, which cannot be covered by construction.
- fix: treat the handler attributes the same way `[Test]` is treated. `crate::queries::tests::is_test_attribute` is the obvious place for a sibling `is_test_handler_attribute`.
- status: open

### [BUG] `object_infos` has no consumer, so every query still sees one object per file
- where: crates/al-source/src/file_index.rs:167 (the map) against its readers in crates/al-analysis/src/workspace_sources.rs:181 and crates/al-analysis/src/queries/{search.rs:47, diagnostics.rs:486, transaction_lint.rs:351, tests.rs:390, hover.rs:301, definition.rs:145, source.rs:247, suggest_event.rs:59}, crates/al-analysis/src/resolution.rs:941 and 1373, crates/al-insight/src/calls.rs:1053, 1142, 1842, 1916
- severity: high
- scenario: `FileIndex` was extended to index every object declaration in a file (`collect_object_declarations`, file_index.rs:689-700, whose doc says the single-object helper "made all subsequent objects invisible to name lookup"). The name index `objects` now holds them all, but `object_infos`, the map that carries each declaration's kind, id and name, has no reader anywhere outside file_index.rs and its own test. Every consumer above reads the singular `object_info`, which file_index.rs:549-551 documents as "the first declaration in document order", or calls `al_syntax::find_object_declaration`, which also returns only the first. So half the feature is wired: a lookup by name finds the *file*, and then every query reads the wrong object's metadata.
  `workspace_sources::validate_object_source` makes this systemic: it builds exactly one `WorkspaceSource` per file from `find_object_declaration`, and `audit`, `test_coverage`, `complexity`, `dead_code`, `impact`, `duplicates`, `obsolescence` and the profiler location pass all consume it. Concretely, in a file declaring `codeunit 50100 "Ship Mgt"` followed by `permissionset 50101 "Ship Perms"`:
  - `permission_set_audit` classifies the file as a codeunit, never parses the `Permissions` property, and then includes the file in the usage scan, so the grant clauses themselves count as references to the granted objects and mask the over-broad findings the audit exists to produce,
  - `test_coverage` labels the second object's procedures with the first object's name, so `ProcKey` and every `covers` / `untested` row names the wrong object;
  - `data_classification_audit` skips the file entirely when the first object is not a table.
- fix: have `workspace_sources` emit one `WorkspaceSource` per object declaration (with the object's own node range), and switch the metadata lookups to `object_infos` with a name or range predicate. Until then `object_infos` is dead weight that suggests the problem is solved.
- status: open

### [BUG] `workspace/symbol` returns an arbitrary subset once the limit is hit, and misses non-first objects
- where: crates/al-analysis/src/queries/search.rs:39-60 (`workspace_search`) and 84-127 (`workspace_search_children`)
- severity: medium
- scenario: both functions iterate a DashMap (`object_info`, `files`) and `break` as soon as `results.len() >= limit`. DashMap iteration order is shard order, which depends on key hashes and is not the file order, so for a query like `Sales` in a project with 400 matching objects and the LSP's limit, the editor's symbol picker shows an arbitrary 100 of them and the exact object the user typed the full name of may not be among them. Nothing ranks an exact or prefix match above a mid-string one, and the results are not sorted at all, so the list also reorders between identical requests. `workspace_search` additionally iterates `object_info`, the first-declaration-only map, so the second and later objects of a multi-object file are not findable by `workspace/symbol` at all.
- fix: collect all matches, rank them (exact, then prefix, then substring, then by name), and truncate after ranking. Iterate `object_infos` for the object list.
- status: open

### [GAP] Symbol search is case-sensitive for non-ASCII names
- where: crates/al-analysis/src/queries/search.rs:24-32 (`ascii_contains_ci`) with the `query.to_lowercase()` at 45 and 90
- severity: low
- scenario: the query is lowercased with Unicode-aware `str::to_lowercase`, and the haystack is folded with `u8::to_ascii_lowercase` per byte. For the object `"München Setup"`, `Ü` is `0xC3 0x9C` and `ü` is `0xC3 0xBC`; the ASCII fold leaves `0x9C` alone, so searching `MÜNCHEN` finds nothing while `München` does. The same applies to `Ø`, `Æ` and the accented names that appear in Nordic and German BC projects (resolution.rs:1788-1860 already tests `Ørnamental` and `München` elsewhere, so the codebase expects them).
- fix: compare with `str::to_lowercase` on both sides, or use a case-folding substring search. The comment "Case-insensitive ASCII substring check" is accurate about the mechanism and the callers treat it as generally case-insensitive.
- status: open

### [SLOP] Two doc comments in lsp.rs describe code that is not there
- where: crates/al-analysis/src/lsp.rs:4-6 and 138-146
- severity: low
- scenario: the module doc says "These impls live in `server` — the LSP transport boundary — so that the `queries` module stays greppably free of `lsp_types`; the architecture rule is that queries never speak a wire format)." The file is `crates/al-analysis/src/lsp.rs`, not `server`, and the sentence ends with an unmatched `)`. `flatten_document_symbols`'s doc says each emitted symbol carries "the parent's `range` as its `location` range". The code at line 168 uses `sym.range`, the symbol's own range, which is the correct behaviour and what the test asserts.
- fix: correct both comments to describe the current layout and the actual range.
- status: open

### [BUG] Signature help for `Receiver.Method(` prefers a same-named procedure in the current file
- where: crates/al-analysis/src/queries/signature.rs:185-206, which runs before `resolve_receiver_signature` at 208
- severity: high
- scenario: `al_syntax::find_call_context` returns the *bare* callee name: for the prefix `Cust.Modify(` it calls `extract_trailing_identifier("Cust.Modify")`, which is `extract_last_identifier` (crates/al-syntax/src/context.rs:122-144) and yields `Modify`. `signature_help_inner` then scans the current file's document symbols for any procedure named `Modify` and returns on the first hit, before it ever looks at the receiver. So in
  ```al
  codeunit 50100 "Ship Mgt"
  {
      procedure Modify(Reason: Text; Silent: Boolean) begin end;

      procedure Post(var Cust: Record Customer)
      begin
          Cust.Modify(
      end;
  }
  ```
  the editor shows `Modify(Reason: Text; Silent: Boolean)` for `Cust.Modify(`. `Get`, `Run`, `Init`, `Insert`, `Delete`, `Validate` and `Find` are all both common local procedure names and record methods, so this is not a rare collision. The receiver-aware `resolve_receiver_signature` exists and is simply reached too late.
- fix: detect the qualified form first, which `resolve_receiver_signature` already does from `prefix.rfind('(')` and `rfind('.')`, and only fall back to the unqualified same-file scan when there is no receiver before the callee.
- status: open

### [GAP] Type-position completion offers an arbitrary 50 objects per kind, each with a leading quote
- where: crates/al-analysis/src/queries/completions.rs:139-161
- severity: medium
- scenario: in a type position (`var Cust: Record `), the query pushes `workspace.symbols.get_by_kind(kind).iter().take(50)` for Table, Enum, Codeunit and Interface. `get_by_kind` (crates/al-symbols/src/index.rs:1065-1070) returns the per-kind vector in index insertion order, so the 50 are simply the first 50 tables loaded, not the 50 most relevant, and the Base Application has thousands. `Customer` is almost certainly not among them. There is no filtering by the prefix the user has typed, because the server does not look at it, so the client cannot recover what was never sent.
  The labels compound it: `label: format!("\"{}\"", arc.name)` always adds the quotes, so a user who has typed `Cust` gets no match from a client doing prefix filtering against `"Customer"`. The keyword completions on the same path are unquoted and do match.
- fix: read the partially-typed prefix at the cursor (`detect_context` already has the line) and filter the symbol index by it before capping, and quote the label only when the name needs quoting. The `insert_text` field, left `None` here, is the place for the quoted form.
- status: open

### [GAP] Go-to-implementations skips the current file entirely and lists one implementor per file
- where: crates/al-analysis/src/queries/implementation.rs:52-71 and 78-107
- severity: low
- scenario: the workspace scan does `if current_path.as_ref() == Some(&file_path) { continue; }`, so invoking go-to-implementation on `interface "IPostHandler"` in a file that also declares `codeunit 50100 "Sales Post Handler" implements "IPostHandler"` never lists that codeunit. `find_codeunit_implementing_interface` also `return`s on the first matching object declaration in a file, so a file with two implementing codeunits contributes one. Neither the package results nor the workspace results are sorted (both come from DashMap iteration), so the picker's order changes between identical requests.
- fix: exclude only the node under the cursor rather than the whole file, collect every matching object per file, and sort the result by object name.
- status: open

### [SLOP] `.alformat.json` ignores misspelled keys, and one of the two loaders is dead
- where: crates/al-analysis/src/queries/format.rs:29-31 (no `deny_unknown_fields`), 124-129 (`load_options`)
- severity: low
- scenario: `load_options_strict`'s doc says it loads options "without hiding an unreadable or invalid `.alformat.json`", and `validate` checks every value it knows about. But the struct has no `#[serde(deny_unknown_fields)]`, so `{"tabsize": 2}` or `{"keywordCase": "upper"}` parses into an all-default config and the strict loader returns `Ok` with defaults. A user who misspells a key gets no signal that the file was ignored. Separately, `load_options` (the swallowing variant, `_ => FormatOptions::default()`) has no caller outside this module, and `crates/al-lsp/src/server/daemon/build_dispatch/fixes.rs:152` uses the strict one. `to_format_options` also accepts a `tab_size` of 17 that `validate` rejects, so the two entry points disagree about what is valid.
- fix: add `deny_unknown_fields`, delete `load_options`, and route the range checks through one place.
- status: open

### [PERF] Every codeLens request runs an uncached go-to-definition for every call site in the workspace
- where: crates/al-analysis/src/queries/code_lens.rs:359-661 (`build_reference_counts`), calling `super::binding::decl_loc` at 403 and 471, plus 349 per lens
- severity: high
- scenario: `build_reference_counts` walks every workspace file twice, once in `record_member_bindings` and once in `record_file`, and calls the raw `binding::decl_loc` for every procedure declaration and every call site it sees. `decl_loc` (crates/al-analysis/src/queries/binding.rs:30-47) runs a full `definition::definition` query when the enclosing-declaration shortcut misses, which itself does symbol-index lookups, a `TypeResolver` construction and a `find_variable_references` fallback. On a 2000-file project with ~100k call sites that is ~100k go-to-definition resolutions per `textDocument/codeLens`, and `crates/al-lsp/src/server/lsp.rs:2048-2055` neither caches nor debounces the result, so it runs on every document open and after every change that invalidates lenses.
  The memoizing wrapper exists and is used elsewhere for exactly this reason: `references.rs:34-36` comments "Memoize binder lookups: without it every occurrence in every workspace file runs a full go-to-definition query", and `rename.rs:110` does the same. `code_lens.rs` never constructs a `DeclLocCache`.
- fix: build one `DeclLocCache` per request and thread it through `record_member_bindings` and `record_file`, then cache the resulting count map per workspace generation so repeated codeLens requests on an unchanged workspace are free.
- status: open

### [BUG] A parenthesis-less AL call is not counted, so those procedures show "0 references"
- where: crates/al-analysis/src/queries/code_lens.rs:542-578 (`is_call_site`)
- severity: medium
- scenario: the bare-call branch requires the `postfix_expression` parent to have a `call_suffix` child. AL permits a parameterless call with no parentheses (`MyProc;`, `Init;`, `CurrPage.Update;`), which produces no `call_suffix`, so those sites are never recorded in `seen`. A procedure that every caller invokes in that form gets `reference_count_for_declaration` = 0 and its lens reads "0 references", which is what a developer uses to decide the procedure is dead. The codebase knows the form exists: `code_actions/add_parens.rs` is entirely about offering to add the missing parentheses (AL0604), and the existing r1 finding on that file is about which of those calls it recognises.
- fix: also accept an identifier whose `postfix_expression` parent has no suffix at all when it stands alone as a statement, mirroring `add_parens::is_callable_identifier_path`.
- status: open

### [BUG] `node_clean_name` mangles a quoted identifier containing an escaped quote
- where: crates/al-analysis/src/queries/mod.rs:68-76, used by hover, definition, references, rename and implementation
- severity: low
- scenario: `tree-sitter-al/grammar.js:1206` defines `quoted_identifier: token(seq('"', repeat(choice(/[^"\r\n]/, '""')), '"'))`, so `""` is a legal escaped quote inside an AL quoted name. `node_clean_name` is `text.trim_matches('"')`, which strips *every* leading and trailing quote rather than one pair and never un-doubles the interior. For the field `"Order ""A"""` it returns `Order ""A` instead of `Order "A`, so the name never matches the symbol index or another occurrence and hover, go-to-definition, find-references and rename all come back empty on that identifier. The doc says the function strips "surrounding double-quotes", which describes one pair.
- fix: strip a single leading and trailing `"` when both are present, then replace `""` with `"`. `queries/source.rs:547`, `test_coverage.rs:390`, `audit.rs:642` and `profiler_hints.rs:454` repeat the same `trim_matches('"')` and should route through the shared helper.
- status: open

### [SLOP] al-analysis's crate doc still explains itself in terms of a crate that no longer exists
- where: crates/al-analysis/src/lib.rs:3 and 21-23
- severity: low
- scenario: the doc opens "Extracted from al-core" and the comment above `pub mod resolution` says it was promoted to `pub` "so it can be re-exported from al-core (`pub use al_analysis::resolution;`)". There is no `al-core` crate in `crates/`, because the split renamed it to `al-lsp`. A reader following that instruction has nowhere to put the re-export. The same stale name survives in Cargo.toml comments (crates/al-analysis/Cargo.toml:34, crates/al-dap/Cargo.toml:19, crates/al-workspace/Cargo.toml:29-30).
- fix: name the current crate in each comment, or drop the migration note now that the split has landed.
- status: open

## Opened while fixing

### [SLOP] `obsolescence.rs` tests for the node kind `attribute_list`, which the grammar does not define
- where: crates/al-analysis/src/queries/obsolescence.rs:186 and 200
- severity: low
- scenario: both lines read `s.kind() == "attribute" || s.kind() == "attribute_list"`. tree-sitter-al's node is `attribute`; there is no `attribute_list`, so the second test is always false. Harmless today because the first test is correct, but it states a grammar shape that does not exist and the same pattern in dead_code.rs, tests.rs and test_coverage.rs sat next to a real bug. `crates/al-analysis/tests/node_kind_literals.rs` carries an `ALLOWED_ABSENT` entry for this pair; delete the entry with the fix.
- fix: drop the `|| ... == "attribute_list"` on both lines.
- status: open (the file belongs to a concurrent fix branch)

## Review complete

`test_coverage`'s "conservative direct pass" matches four tree-sitter node kinds
(`method_call`, `function_call`, `invocation_expression`, `call_expression`) that
do not exist in tree-sitter-al, so roughly 110 lines never execute and all
coverage comes from the graph pass the module presents as a supplement.

`FileIndex::object_infos`, the map that made multi-object files work, has no
reader outside its own crate. Every query still reads the first declaration, so
`al source` returns the wrong id with the whole file's text, `test_coverage`
labels procedures with the wrong object, and a permission set declared second in
a file is parsed as whatever the first object is and then counted as usage of the
objects it grants.

The permission audit fails outright on a `system` grant, which AL allows and BC
permission sets use, and its right-level check inverts its own safety promise:
because the write-site scan visits only the first declaration of each repeated
trigger name, a write inside a second `OnValidate` or `OnAction` is invisible and
the audit recommends dropping a permission the code needs.

`suggest_event`'s "ready-to-paste" subscriber attribute writes
`Table::"Sales Header"` where AL requires `Database::"Sales Header"`, so the main
output of the `{type:'table'}` query does not compile. Every fixture in this repo
spells it the right way.

Two request-path costs stand out. `code_lens` runs an uncached `decl_loc`, and
therefore a full go-to-definition, for every call site in the workspace on every
`textDocument/codeLens`, while `references` and `rename` both memoize the same
lookups. Signature help answers `Cust.Modify(` with a same-named procedure from
the current file because the unqualified scan runs before the receiver is
resolved.
