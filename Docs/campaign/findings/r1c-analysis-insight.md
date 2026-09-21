# R1c review: al-analysis and al-insight, the remainder

Adversarial read-only review of every file under `crates/al-analysis/src` and
`crates/al-insight/src` that neither `r1-analysis-insight.md` nor
`r1b-analysis-insight.md` ticked. `scaffold.rs` and `generators.rs` are out of
scope here (covered by `r1b-scaffold-generators.md`).

## Coverage

Explicitly named as unreviewed:
- [x] queries/source.rs
- [x] queries/audit.rs
- [ ] queries/test_diagnostics.rs
- [ ] queries/code_actions/test_support.rs
- [ ] queries/suggest_event.rs
- [ ] queries/profiler_hints.rs
- [ ] queries/test_coverage.rs

Listed by neither checklist:
- [ ] lsp.rs
- [ ] queries/mod.rs
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
- [ ] queries/code_lens.rs
- [ ] queries/hover.rs
- [ ] queries/completions.rs
- [ ] queries/signature.rs
- [ ] al-analysis/src/lib.rs, al-insight/src/lib.rs

## Findings

### [BUG] `al source` on any object but the first in a multi-object file returns the wrong id and the whole file
- where: crates/al-analysis/src/queries/source.rs:246-262 (`source_candidates`) and 327-349 + 388-400 (`try_workspace_source`)
- severity: high
- scenario: a file `Shipment.al` declaring `table 50100 "Shipment Header"` followed by `table 50101 "Shipment Line"`. `FileIndex::add_file_with_tree` (crates/al-source/src/file_index.rs:525-553) indexes *both* names into `objects`, so `object_paths("Shipment Line")` returns `Shipment.al`. But the query then reads `file_index.object_info` (the singular map, which file_index.rs:549-551 documents as "the first declaration in document order") and `al_syntax::find_object_declaration`, which also returns only the first. So `al source "Shipment Line"` reports `k: Table, id: 50100` — the *header's* id — under the name `Shipment Line`, and `code` is the text of the whole file including both tables. When the two objects have different kinds (`table` then `page`, the common setup-table-plus-card file), `declared_kind != kind` at line 338 fires and the query returns `InvalidWorkspaceDeclaration`; with an explicit `--kind page` the candidate is dropped at line 259 and the answer is `ObjectNotFound`. `file_index.object_infos` (the plural map, file_index.rs:167) already holds every declaration with its own kind and is never consulted here.
- fix: select the `CachedObjectInfo` from `object_infos` whose `name` matches the requested name, and slice `code` to that object's node range rather than returning `text.clone()`.
- status: open

### [BUG] The reported signature of an attributed procedure is the attribute, truncated
- where: crates/al-analysis/src/queries/source.rs:566-599 (`extract_signature_from_text`), reached from `find_member_node` at 550 and `extract_member_from_text` at 604
- severity: medium
- scenario: `tree-sitter-al/grammar.js:755-757` puts `repeat($.attribute)` inside `procedure_declaration`, so the node text of
  ```al
  [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
  procedure MyHandler(var SalesHeader: Record "Sales Header")
  ```
  starts with the attribute. `extract_signature_from_text` scans for the first paren-balanced `(...)`, which is the attribute's argument list, so it stops at the `)` before `]` and returns `[EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)` — the attribute with its closing bracket cut off, and no procedure signature at all. Every subscriber, every `[IntegrationEvent]` publisher and every `[Test]` procedure reports this as `sig` from `al source --proc`. The `range.l` returned alongside it is the attribute's line for the same reason.
  The eight `extract_signature_*` tests (source.rs:1156-1250) all feed bare `procedure ...` text, so none of them sees an attribute.
- fix: take the signature from the declaration's own child nodes (`kw_procedure`/`kw_function`, the `name` field, the `parameters` field and the optional `return_type` field) instead of re-scanning the node's text. If the text scan stays, skip leading `[...]` attribute blocks first.
- status: open

### [BUG] One `system` permission grant makes the whole permission audit fail
- where: crates/al-analysis/src/queries/audit.rs:768-777 (`parse_permission_clause`), propagated through `extract_permission_grants` (753-758) to `permission_set_audit` (277)
- severity: high
- scenario: AL's `Permissions` property accepts `system` as an object type alongside tabledata/table/report/codeunit/xmlport/page/query, and real permission sets use it: `Permissions = tabledata Customer = RIMD, system "Tools, Debugger" = X;`. `parse_permission_clause` matches the lowercased type against a seven-entry list with no `system` arm, so the `other` arm returns `unsupported permission object type 'system'`. `extract_permission_grants` turns that into `AuditError::InvalidSource`, and `permission_set_audit` returns `Err` on the `?` at line 277. One `system` grant anywhere in the project therefore takes down coverage, over-broad and over-granted-rights for the entire workspace, not just that one clause. The rights check makes it worse: `system` grants are written `= X`, which the non-tabledata branch would accept, so only the type list is in the way.
- fix: add `"system" => "System"` to the type match and skip system grants in the usage scans (they have no workspace object to reference). More generally, collect per-clause parse failures into the report instead of aborting the audit, the way `whole_workspace_audits_skip_malformed_source` (audit.rs:1116-1135) already does for unparsable files.
- status: open

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
- status: open

### [PERF] Every grant costs two full-workspace tree walks, and the second is a recomputation of the first
- where: crates/al-analysis/src/queries/audit.rs:446-451 (`count_object_refs`), called at 423 (`compute_over_broad`) and again at 506 (`compute_over_granted_rights`)
- severity: medium
- scenario: `count_object_refs` runs `al_syntax::find_variable_references` over every non-permissionset file, which is a full tree walk each. `compute_over_broad` calls it once per unique grant; `compute_over_granted_rights` then calls it again with the same argument for every `tabledata` grant, recomputing a number the first pass already had. A permission set with 300 tabledata grants over a 2000-file project is 300 x 2000 walks for the first check plus another 300 x 2000 for the second. `collect_observed_writes` (570-611) adds its own quadratic term: for each file it calls `extract_procedure_var_types` and `extract_call_sites` once per procedure name, and each of those re-walks that file's tree from the root to find the procedure.
- fix: collect every referenced identifier name per file once (`al_syntax::collect_call_site_names` has the same shape and its doc, navigation.rs:291-303, argues exactly this point) into one multiset, then look each grant up in it. Pass the counts from `compute_over_broad` into `compute_over_granted_rights` rather than recomputing them.
- status: open
