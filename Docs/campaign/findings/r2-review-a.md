# R2 review A: al-analysis, al-insight, al-syntax, al-source, al-symbols, al-runtime, al-test, al-emit

Adversarial review of `git diff dev..campaign/2026-09-21` for the eight crates above.
Reads the finding, the commit, the current code and the test for each sampled item.
Everything below is verified against the tree at `campaign/2026-09-21` unless tagged
`[UNVERIFIED]`.

## Coverage

Fix verification, high severity (finding -> commit -> code -> test):
- [x] r1 with_elimination: dropped semicolon (cc9f231b)
- [x] r1 with_elimination: deletes text before `with` (cc9f231b)
- [x] r1 make_local: attribute line (0108b968)
- [x] r1 bulk_fix: `Specifies` prefix (c375402b)
- [x] r1b rename: quoted identifier no-op (eae29618)
- [x] r1b rename: EventSubscriber references (9148954c)
- [x] r1b resolution: `parse_field_line` parenthesis (be039a6c)
- [x] r1b resolution: symbol package docs per keystroke (7fab7e36)
- [x] r1b xliff: empty object type/id/name (95e80294)
- [x] r1b xliff: multi-object file (95e80294)
- [x] r1b obsolescence: object-level ObsoleteState (15d7b1c0)
- [x] r1b obsolescence: obsolete fields (15d7b1c0)
- [x] r1b obsolete_usage: double snapshot (15d7b1c0)
- [x] r1c source.rs: multi-object id and code (4180a321)
- [x] r1c audit: `system` permission grant (401e5022)
- [x] r1c audit: over-granted rights, repeated trigger name (5996424f)
- [x] r1c suggest_event: `Database::` scope (3302ac49)
- [x] r1c test_coverage: dead direct pass (334afb5c)
- [x] r1c workspace_sources: `object_infos` had no consumer (74acc262)
- [x] r1c signature: receiver ignored (52b75eda)
- [x] r1c code_lens: uncached decl_loc (eebb9c98)
- [x] r1 syntax sort_members: multi-object corruption (24ffeeea)
- [x] r1 runtime: Option/Enum typed zero (84144137)
- [x] r1 runtime: SetFilter Date/Option placeholders (ef9c11e5)
- [x] r1 runtime: Round midpoint (182c5c93)
- [x] r1 runtime: temporary record store (abbb8e07)
- [x] r1 runtime: fall-off-the-end return value (c1ec0077)
- [x] r1 runtime: Text[N]/Code[N] enforcement (d32104f8)
- [x] r1b test: `--filter` trailing pattern (40a64ccf)
- [x] r1 emit: control add-in path containment (16731e97)
- [x] r1 emit: resourceExposurePolicy key (7dfd746e)

Fix verification, spread of medium and low:
- [x] r1 make_local PERF lowercase scan (c4a9839f)
- [x] r1 events.rs open-buffer table edit (ed298cbe)
- [x] r1 impact/analysis conditional TableRelation (88a5fd98)
- [x] r1 permissions: one unparseable file (9d02c3b4)
- [x] r1 inlay_hints source_line (387df64c)
- [x] r1c test_diagnostics: line 0 sentinel (ec3cb172)
- [x] r1 syntax: clean_identifier_text call sites (bce5ec64)
- [x] r1 syntax: ts_range_to_syntax / LineIndex (caa16426)
- [x] r1 symbols: file_index owner per (name, kind) (a8ead4ab)
- [x] r1 symbols: source_index cache eviction (ddaad70f)
- [x] r1b runtime: junit control characters (443ef7cc)
- [x] r1b runtime: cobertura double count (ffb0807e)

Merge damage:
- [x] `resolve_object_path` / `FileIndex::object_path_where`
- [x] `source.rs` object ranges and `member_signature`
- [x] `LineIndex` / `SourceLines`
- [x] split `al-syntax/src/formatting/`
- [x] split `al-syntax/src/symbols/`
- [x] split `al-symbols/src/index/`
- [x] split `al-symbols/src/oauth/`
- [x] duplicated helpers across the eight crates

Flagged behaviour changes, every consumer:
- [x] `PermissionCollection { entries, skipped }`
- [x] `dispatch_generate` refusing ids at or below 50000
- [x] `create_project` refusing to overwrite
- [x] `Round` semantics
- [x] temporary record isolation
- [x] `Text[N]` enforcement
- [x] `TestDiagnostic.line` optional
- [x] `PermissionAuditReport.parseIssues`
- [x] `EdgeKind::TriggerInvocation` removed
- [x] `WorkspaceSource::objects`

Quality of the new code:
- [x] comments restating code, defensive branches, speculative parameters, one-caller wrappers
- [x] tests that assert nothing meaningful
- [x] duplicated helpers across crates
- [x] panics reachable from user input
- [x] over-long functions added during the campaign
- [x] naming against the surrounding code, needless `pub`

Multi-object open item:
- [x] the nine workspace queries that still walk from the file root
- [x] whether any campaign fix assumed otherwise

## Findings

### [BUG] The `Specifies` prefix fix panics on a non-ASCII tooltip
- where: crates/al-analysis/src/queries/bulk_fix.rs:632-634, reached from
  crates/al-analysis/src/queries/bulk_fix.rs:711 (`inject_tooltips`) and
  crates/al-lsp/src/server/daemon/build_dispatch/fixes.rs:676-689 (`dispatch_fix_tooltips`)
- severity: medium
- scenario: `with_specifies_prefix` (added by c375402b for the double-`Specifies` finding) tests
  `value.len() > 9 && value[..9].eq_ignore_ascii_case("Specifies")`. `value[..9]` is a byte slice.
  The value is a table field's `ToolTip` property text, read verbatim out of the loaded symbol
  package at fixes.rs:680-684, so it is arbitrary user text. For the French base-app spelling
  `ToolTip = 'Spécifié le numéro du client.'` the string is `53 70 c3 a9 63 69 66 69 c3 a9 ...`,
  so byte 9 is the continuation byte of the second `é` and the slice panics with
  `byte index 9 is not a char boundary`. `al-explorer fix tooltips --from-table Customer` against
  a localized package kills the daemon request thread. The three new tests all use ASCII values,
  so the suite cannot see it.
- fix: compare without slicing, for example
  `value.strip_prefix(...)` on a case-folded first word, or
  `let mut c = value.chars(); PREFIX.chars().all(|p| c.next().is_some_and(|v| v.eq_ignore_ascii_case(&p))) && c.next().is_some_and(char::is_whitespace)`.
  Add a case with `Spécifié`.
- status: open

### [MERGE] `FileIndex::best_owner` and `FileIndex::object_path_where` are the same function
- where: crates/al-source/src/file_index.rs:823-836 (`object_path_where`) and 838-849 (`best_owner`)
- severity: low
- scenario: the two agents' `resolve_object_path` changes were combined by adding
  `object_path_where` (LOG.md, 2026-09-21 21:50), but the earlier side's `best_owner` stayed.
  Both bodies are: resolve `from`'s app root, take `objects[name.to_lowercase()]`, filter by kind,
  `min_by_key(|e| self.owner_rank(e, from_app.as_ref()))`, map to the path. `best_owner(name,
  kinds, from)` is exactly `object_path_where(name, Some(from), |k| kinds.is_none_or(|ks|
  ks.iter().any(|want| k.eq_ignore_ascii_case(want))))`. A change to the ranking rule (the
  same-app-then-dependency preference the a8ead4ab finding asked for) now has to be made twice.
- fix: make `object_path_near` and `object_path_of_kind_near` call `object_path_where` and delete
  `best_owner`.
- status: open

### [SLOP] `FileIndex::object_path_of_kind_near` has no caller but its own test
- where: crates/al-source/src/file_index.rs:810-817, test at file_index.rs:1715
- severity: low
- scenario: a repo-wide grep for `object_path_of_kind_near` finds the definition, its doc links and
  one assertion inside `file_index.rs`'s own test module. `object_path_near` has two production
  callers (resolution.rs:1498, definition.rs:144) and `object_path_where` one (resolution.rs:1487);
  the kind-plus-app variant is public surface added by the merge that nothing asked for.
- fix: delete it, or use it at the call sites that pass a kind list and a referring file
  (resolution.rs:1316 `object_path_of_kind(enum_name, ...)` is one).
- status: open

### [MERGE] `SourceRange.f` is a bare file name from one exit and an absolute path from the other
- where: crates/al-analysis/src/queries/source.rs:398 (member exit, `file_path.file_name()`) and
  source.rs:443 (object exit, `file_path.to_string_lossy()`), against the field's doc at
  source.rs:56-57 ("Relative file path")
- severity: low
- scenario: 4180a321 (multi-object `al source`) and fd97ced3 (daemon projection) both edited
  `try_workspace_source`. The projection agent added `range` to the whole-object exit and spelled
  `f` as the absolute path; the member exit, which already existed, spells it as the basename.
  `al source "Shipment Line" --json` answers `"f": "/home/you/proj/src/Shipment.al"`, while
  `al source "Shipment Line" --proc Stamp --json` answers `"f": "Shipment.al"`. Neither is the
  relative path the field documents, and the object form puts the developer's absolute filesystem
  path into a response that MCP hands to an agent. The response contract
  (crates/al-explorer/src/cli/commands/response_contract.rs:154) only checks the type, so nothing
  catches the split.
- fix: pick one spelling, project-relative for both, and say so in the doc.
- status: open

### [REGRESSION] The permission-set `skipped` list never reaches the user
- where: crates/al-analysis/src/permissions.rs:34-37 (`PermissionCollection`),
  crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:75-85 (builds `skipped`),
  crates/al-explorer/src/cli/commands/lsp/project.rs:42-56 (`cmd_permissions`)
- severity: medium
- scenario: 9d02c3b4 changed `collect_permissions` from failing on the first unparseable `.al` to
  skipping it and reporting it in `skipped`. The daemon puts the list in the JSON result. The human
  CLI path reads only `content` and `objectCount` and prints
  `"\n{count} objects included"`. So `al-explorer permissions --name "My App Objects"` on a project
  with one work-in-progress file now writes a permission set that silently omits that object's
  permissions and says nothing, where before it refused and named the file. The response contract
  at response_contract.rs:278-286 does not list `skipped` either, so nothing forces a consumer to
  look at it.
- fix: print a line per skipped file to stderr from `cmd_permissions`, and add `skipped` to the
  `permissions` contract.
- status: open

### [REGRESSION] The permission audit's `parseIssues` never reaches the user either
- where: crates/al-analysis/src/queries/audit.rs:262 (`parse_issues`),
  crates/al-explorer/src/cli/commands/lsp/reports.rs:115-190 (`cmd_permission_audit`),
  crates/al-explorer/src/cli/commands/mod.rs:889-893 (contract)
- severity: medium
- scenario: the same shape as the finding above, from 401e5022. One unreadable `Permissions` clause
  used to abort the audit with an error the user saw. It now degrades on its own into
  `parse_issues`, and `cmd_permission_audit` reads `coverage`, `overBroad` and `overGrantedRights`
  and nothing else. A grep for `parseIssues` across `crates/`, `plugin/` and `Docs/` finds the
  field only at its declaration and in three of its own tests. A permission set whose clause the
  audit could not read is therefore excluded from every check and reported as clean. That is the
  direction the audit's own module doc (audit.rs:16-19) promises not to fail in.
- fix: print the parse issues and add `parseIssues` to the `permissions.audit` contract in
  commands/mod.rs:889.
- status: open

### [REGRESSION] `dispatch_generate` refuses object id 50000, which is the first legal customization id
- where: crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:306
  (`MICROSOFT_ID_RANGE_END: i32 = 50_000`) and 337-346, pinned by the test at codegen.rs:604-621
- severity: medium
- scenario: the guard is `if object_id <= MICROSOFT_ID_RANGE_END`, so `al-explorer generate page
  --id 50000 --table Customer` is refused with "Object ID 50000 is inside Microsoft's reserved
  range (1-50000)". Microsoft's own object-range page says the base-app range is 0-49,999 and
  "50,000-99,999 This range is for customizations", and PerTenantExtensionCop PTE0001 states the
  free range as 50,000-99,999. So 50000 is the first id a per-tenant extension may use, the error
  message quotes a range that contradicts both pages, and `(50_000, "reserved range")` in the test
  pins the wrong boundary. Related: the check ignores `app.json`'s `idRanges`, so a localization or
  RSP app whose assigned range sits below 50000 cannot scaffold at all.
- fix: `object_id < 50_000`, reword the message as `1-49999`, change the test case to `49_999`,
  and prefer the project's own `idRanges` when `app.json` declares one.
- status: open

### [SLOP] `builtin_round`'s doc comment still describes the rounding the fix removed
- where: crates/al-runtime/src/interpreter/dispatch.rs:1604-1609 against the code at 1649-1654
- severity: low
- scenario: 182c5c93 changed `'='` to `MidpointAwayFromZero`, `'<'` to `ToZero` and `'>'` to
  `AwayFromZero`, and wrote the reasoning in a comment at 1647-1648. The doc comment four lines
  above still reads "direction `'='` (round to nearest with banker's rounding — midpoints go to
  the even multiple). `'<'` rounds toward negative infinity, `'>'` toward positive infinity."
  Every one of those three statements is now the behaviour the finding said was wrong, and a
  reader checking `Round(-1234.56789, 0.001, '<')` against the doc gets `-1234.568` where the code
  answers `-1234.567`.
- fix: rewrite the doc to match the three strategies and cite the System.Round page once.
- status: open

### [BUG] The campaign findings record lists fixed items as open
- where: Docs/campaign/findings/r1-analysis-insight.md:201-248 (seven findings repeated from
  152-199) and Docs/campaign/findings/r1-emit-bc-explorer.md:246-251
- severity: medium
- scenario: r1-analysis-insight.md carries seven findings twice, once with `status: fixed <sha>`
  and once, lower down, with `status: open`: the `source_line` PERF and SIMPLIFY pair, the
  events.rs `to_lowercase` indices, the calls.rs identical match arms, the events.rs stale comment,
  the arch_lint `expect`s and the doc_region off-by-one. All seven are fixed in the tree:
  `fn source_line` exists nowhere in al-analysis (only `TypeResolver::source_line`),
  events.rs:336 and :353 use `to_ascii_lowercase`, the duplicate match arm is gone from
  calls.rs:630-656, `grep -c 'expect("validated' arch_lint.rs` is 0, and doc_region.rs:117 tests
  `range.end.character == 0`. Separately, r1-emit-bc-explorer.md:246 records "xlf generate drops
  every property declared on a one-line member block" as open "in al-analysis, which another agent
  holds on another branch", while r1b-analysis-insight.md:177-182 records the same bug fixed in
  95e80294 — and `xliff::tests::a_one_line_member_anchors_its_own_property`
  (crates/al-analysis/src/xliff.rs:1906) asserts the two one-line fields get distinct ids anchored
  to their own members. Eight items in the open list are already done, so whoever works the list
  next re-reads code that needs nothing.
- fix: delete the duplicated block in r1-analysis-insight.md and mark the r1-emit xliff item fixed
  in 95e80294, then reconcile STATE.md's open list against both.
- status: open

### [SIMPLIFY] `extract_table_relation_table` has no production caller
- where: crates/al-insight/src/analysis.rs:235-237, tests at analysis.rs:675-730
- severity: low
- scenario: 88a5fd98 moved `check_object_consumers` onto the plural
  `extract_table_relation_tables` (impact.rs:354, analysis.rs:147). A repo-wide grep for
  `extract_table_relation_table` finds the definition, one doc cross-reference and eight of its own
  test assertions. It is public API that returns the alphabetically first branch of a conditional
  `TableRelation`, which is the answer the finding called wrong, and nothing calls it.
- fix: delete it and its tests, or state at the declaration what it is kept for.
- status: open

### [GAP] The multi-object open item names nine queries; three of them do not have the problem
- where: Docs/campaign/findings/r1c-analysis-insight.md:300-305 against
  crates/al-analysis/src/queries/{complexity.rs, obsolescence.rs, obsolete_usage.rs}
- severity: low
- scenario: the open item lists nine readers of `WorkspaceSource` that "take `source.object` for
  the object name and walk `source.tree` from the root". Six do: dead_code.rs:84,
  duplicates.rs:70, sql_patterns.rs:40, arch_lint.rs:264/270/282, impact.rs:383/422/448 and
  native_check.rs:225-230. The other three do not attribute anything to `source.object`:
  `workspace_complexity` (complexity.rs:35-60) emits a per-file `FileComplexity` and never names
  an object; `obsolete_usages` (obsolete_usage.rs:33-54) keys its definitions on the procedure name
  alone and `ObsoleteUsageFinding` (obsolete_usage.rs:19-23) carries only file, range and message;
  and `obsolescence.rs:186-191` re-reads the object name from each `object_declaration` node as the
  walk descends, which 15d7b1c0 added on purpose. Of the six that remain, `native_check` is the
  one with a concrete wrong answer beyond a label: it builds one object record per file from
  `source.object.info` and then calls `extract_member_ids(&source.tree, ...)` over the whole tree,
  so the second object's members are recorded under the first object's kind, id and name.
  No campaign fix assumed otherwise: the four passes 74acc262 claims to have converted really do
  iterate (`audit.rs:83/312/367`, `test_coverage.rs:140/367`, `profiler_hints.rs:398`).
- fix: correct the list to the six, and lead with `native_check`.
- status: open

### [SLOP] `PermissionAuditReport`'s doc comment lost its subject in a merge
- where: crates/al-analysis/src/queries/audit.rs:247-249
- severity: low
- scenario: the comment reads "Full result of the permission-set audit: per-object coverage plus
  over-broad (unused) grants. added the `over_broad` (object-level) and `over_granted_rights`
  (right-level / RIMDX) sections; `coverage` is unchanged." The second sentence has no subject —
  a commit reference or an agent's name was edited out of the front of it — and it describes the
  struct as a changelog entry rather than saying what the four fields are.
- fix: one sentence naming the four sections; the change history belongs in the git log.
- status: open

### [SIMPLIFY] `build_reference_counts` is a 364-line function with five nested helpers
- where: crates/al-analysis/src/queries/code_lens.rs:387-750
- severity: low
- scenario: eebb9c98 added 62 lines to a function that was already 302 (measured against
  `git show dev:crates/al-analysis/src/queries/code_lens.rs`). It now holds `record_member_bindings`,
  `record_file`, `record_event_subscriber_reference`, `is_call_site` and
  `is_bare_statement_expression` as nested `fn`s, three of which are pure predicates over a
  tree-sitter node and none of which closes over the enclosing scope. They are unreachable from a
  test without going through the whole workspace walk, which is why `is_call_site`'s bare-call gap
  (93c0f49d) had to be found by reading rather than by a unit test.
- fix: move the file to `code_lens/` with the three node predicates in their own module and a
  direct test each.
- status: open

### [BUG][pre-existing] `open_browser` hands a URL full of `&` to `cmd /c start` on Windows
- where: crates/al-symbols/src/oauth/redirect.rs:177-180, called from
  crates/al-symbols/src/oauth/flows.rs:176 with the URL built at flows.rs:163-173
- severity: medium
- scenario: not campaign damage — 4f84b50e only moved this code out of `oauth.rs`, and `dev` has
  the same body at oauth.rs:728. Recording it because the round 2 security pass read `oauth.rs`
  and did not reach it. `suppress_stdio_and_spawn("cmd", &["/c", "start", "", url])` passes the
  authorization URL as an argument with no spaces, so Rust quotes nothing and `cmd.exe` re-parses
  the line. `cmd` treats `&` as a command separator, and the URL is
  `...authorize?client_id=X&response_type=code&redirect_uri=...`, so Windows opens the browser on
  the truncated URL and then tries to run `response_type=code` as a command. Interactive BC
  sign-in cannot work on Windows as written, and the same hop would execute anything a URL source
  could put after an `&` (the device-code path at flows.rs:257 and :263 passes a
  `verification_uri` straight from the server's JSON).
- fix: call `ShellExecuteW` on Windows, or quote the URL for `cmd` (`start "" "<url>"`) and refuse
  a URL that is not `https://` with no `"` in it.
- status: open

## Verified fixes

Each was read as finding -> commit -> current code -> the test that covers the failure scenario,
and the test names the input the finding named.

- r1 with_elimination dropped semicolon and deleted prefix (cc9f231b). The edit spans the node's
  own columns (with_elimination.rs:178-196). `with_elimination_keeps_the_terminating_semicolon_of_a_single_statement_body`
  and `with_elimination_keeps_code_before_the_with_on_the_same_line` (with_elimination.rs:1007,
  1039) assert the whole file after the action, on the finding's two inputs.
- r1 make_local attribute line (0108b968). `signature_keywords` (make_local.rs:84-119) reads the
  `kw_procedure` child. Three tests at make_local.rs:468, 498, 525 cover the
  `'OnAfterPostProcedure'` attribute, `[NonDebuggable]` and an already-local attributed procedure.
- r1 make_local whole-workspace lowercasing (c4a9839f). `contains_identifier_ignore_ascii_case`
  (make_local.rs:42) scans bytes with no allocation, with an identifier-boundary test at :378.
- r1 events.rs table edit against the on-disk snapshot (ed298cbe). events.rs:66 prefers
  `documents.get_text_arc(&table_uri)`; the lowercased-index bug is gone (`to_ascii_lowercase` at
  events.rs:336 and :353).
- r1 conditional `TableRelation` (88a5fd98). impact.rs:354 calls the plural helper.
- r1 permissions one bad file (9d02c3b4). Routed through `snapshot_with_skipped`; see the open
  regression above about the report never being printed.
- r1 inlay_hints `source_line` (387df64c). No `fn source_line` remains in al-analysis; the three
  modules take `&al_syntax::SourceLines` (inlay_hints.rs:27, diagnostics.rs:568,
  profiler_hints.rs:512).
- r1b rename of a quoted identifier, EventSubscriber strings, keyword names and control characters
  (eae29618, 9148954c). `parse_rename_name` / `RenameName::spelled_over` (rename.rs:460-520) derive
  the spelling from the identifier, so the unquoted placeholder round trip closes;
  `rename_quoted_field_through_the_client_round_trip` (rename.rs:1074) drives
  `prepare_rename` -> client edit -> `rename` and asserts both files;
  `rename_event_rewrites_the_subscriber_string` (rename.rs:1319) asserts the subscriber file byte
  for byte, and `rename_event_leaves_a_subscriber_to_another_publisher_alone` pins the guard.
  `rename_field_edits_the_referencing_codeunit` (rename.rs:1244) closes the cross-file TEST gap.
- r1b `parse_field_line` (be039a6c). Replaced by `parse_field_node` (resolution.rs:1706-1734)
  reading the header's child nodes; `field(50; "Amount (LCY)"; Decimal)` and `field(52; "A;B";
  Text[10])` are in the tests at resolution.rs:2852 and 2883, and `parse_field_line` no longer
  exists anywhere.
- r1b xliff (95e80294). `every_object_in_a_multi_object_file_is_extracted` (xliff.rs:1798) asserts
  two units with different ids and the right object names;
  `a_one_line_member_anchors_its_own_property` (:1906) and
  `a_caption_parses_with_any_spacing_around_equals` (:1925) cover the other two shapes.
- r1b obsolescence (15d7b1c0). `finds_obsolete_object_properties` (:478),
  `finds_obsolete_field_key_and_enum_value` (:508), `removed_is_not_read_as_pending` (:626) and
  `caller_counts_come_from_one_pass_per_file` (:663) cover object properties, fields, the ordering
  bug and the per-symbol re-walk.
- r1c `al source` on a multi-object file (4180a321). `source_candidates` matches on name through
  `object_infos_in` (source.rs:260-266) and `try_workspace_source` re-reads with
  `find_object_declarations` and slices `code` to the object's byte range (:443).
  `source_returns_the_named_object_in_a_multi_object_file` (:1896) asserts the header's text does
  not contain the line table, and `source_finds_a_second_object_of_a_different_kind` (:1922) covers
  the table-then-page file.
- r1c audit (401e5022, 5996424f, 97c12124). `a_system_grant_is_accepted_and_leaves_the_audit_intact`
  (audit.rs:1184), `a_malformed_clause_is_reported_without_failing_the_audit` (:1165),
  `a_write_in_a_repeated_trigger_name_is_observed` (:1501),
  `a_write_in_a_repeated_field_trigger_is_observed` (:1566) and
  `a_write_through_an_unresolvable_receiver_keeps_the_right` (:1626). The scan walks declaration
  nodes through `al_insight::calls::collect_declaration_nodes` rather than de-duplicating names.
- r1c suggest_event (3302ac49). `subscriber_scope` maps the two arguments separately; the tests at
  suggest_event.rs:1242 and :1279 assert the emitted attribute text is
  `ObjectType::Table, Database::"Sales Header"`.
- r1c test_coverage direct pass (334afb5c). The pass matches `postfix_expression` + `call_suffix`,
  and `crates/al-analysis/tests/node_kind_literals.rs` extracts every kind literal in al-analysis
  and al-insight and checks it against `Language::node_kind_for_id`, with an empty
  `ALLOWED_ABSENT`. That guard is the durable part.
- r1c `test_diagnostics.line` (ec3cb172). `Option<u32>` with
  `skip_serializing_if`, `test_diag_to_lsp` returns `Option<Diagnostic>` (diagnostics.rs:766), and
  `publish_test_diagnostics` remembers the files it published to so an empty slice clears them.
- r1c signature help (52b75eda). `has_receiver` (signature.rs:268) gates the two paths;
  `a_receiver_qualified_call_does_not_answer_with_a_same_named_local_procedure` (:748) is the
  finding's `Cust.Modify(` case.
- r1c code_lens (eebb9c98, 93c0f49d). One `DeclLocCache` per request, and the counts are cached on
  `Workspace::code_lens_reference_counts` keyed to `generation_revision` plus the open document.
  `on_document_change` ends with `mark_generation_changed` (al-workspace/src/lib.rs:1376), so a
  keystroke invalidates it; `reference_counts_are_reused_until_the_generation_changes`
  (code_lens.rs:1060) asserts both directions by `Arc::ptr_eq`.
- r1 syntax `sort_members` on a multi-object file (24ffeeea). `object_body_spans` (sort.rs:73-101)
  takes each `object_declaration`'s `body` from the tree and refuses a file whose brace shares a
  line with code. `two_objects_each_keep_their_own_members` (:415) and
  `three_objects_are_sorted_independently` (:467) assert the full output. The implementation went
  further than the finding asked (sort each object rather than bail), and the refusal is strictly
  safer than the old first-`{`-to-last-`}` scan, which would have mis-segmented the same files
  silently.
- r1 syntax `clean_identifier_text` call sites (bce5ec64). No `trim_matches('"')` remains in
  al-syntax's navigation.rs, symbols/ or type_resolver.rs; the three surviving `trim_matches` calls
  there strip `'` from attribute arguments, which is the case the finding excluded.
- r1 syntax `ts_range_to_syntax` (caa16426) and the `LineIndex`/`SourceLines` merge. The conversion
  derives the line start from `byte_offset - column` (lib.rs, `utf16_col_at`) and needs no table at
  all; `LineIndex` is gone, `SourceLines` is the one type in
  crates/al-syntax/src/source_lines.rs, and `line_matches_the_scanning_lookup` checks it against
  `get_source_line` over six buffers including CRLF and invalid UTF-8. tokens.rs:184 and
  type_resolver.rs:237 both use it.
- r1 symbols file_index owner per (name, kind) (a8ead4ab). `remove_owned_object_mapping`
  (file_index.rs:673-681) retains by path, and `owner_rank` (:773-791) prefers the same app then a
  dependency, with the multi-app `Install` case at file_index.rs:1706-1742.
- r1 runtime Round (182c5c93). `MidpointAwayFromZero` / `ToZero` / `AwayFromZero` at
  dispatch.rs:1649-1654, `round_matches_the_documented_bc_directions` (:2889), plus
  crates/al-runtime/tests/property_round.rs against scaled-integer reference arithmetic. The doc
  comment is the open item above.
- r1 runtime temporary records (abbb8e07). `TableRef` (records.rs:144-165) keys a temporary store
  by the owning variable's handle, `fork_record_for_by_value` (:236-270) copies the whole store for
  a by-value pass, and `temporary_stores_are_keyed_per_variable` (:2011) asserts two temporaries
  and the persistent store are three stores. The router needed no change because
  `split_type_reference` already stripped the keyword (router.rs:1199).
- r1 runtime `Text[N]`/`Code[N]` (d32104f8). `check_string_capacity` (value.rs:286-297) counts
  chars, not bytes, and `coerce_into_slot` trims a `Code` value before measuring it, which is the
  BC rule.
- r1b test `--filter` (40a64ccf). `method_name_matches` (session.rs:68-97) is a backtracking
  matcher that records the last `*`; `("TestPostPost", "*Post")`, `("aaa", "*aa")` and the
  negative `("TestPostPosted", "*Post")` are asserted at session.rs:177-180.
- r1b test JUnit and Cobertura (443ef7cc, ffb0807e). `group_by_object` (cobertura.rs:59-78) folds
  repeated lines into one entry whose `hits` is the number of covering tests; the JUnit test at
  junit.rs:474 round-trips a message holding `\u{0}\u{1}\u{8}\u{b}\u{c}\u{e}\u{1b}\u{1f}`.
- r1 emit control add-in containment (16731e97). All three reads go through
  `read_project_resource` (assemble.rs:622, 818, 850), and `traversing_archive_entry_names_cannot_be_written`
  (:1594) proves `write_zip` refuses `addin/src/../../../../etc/passwd`, `/etc/passwd` and
  `C:/Windows/x.js` as a second line of defence.
- r1 emit `resourceExposurePolicy` (7dfd746e). All four documented keys are read
  (manifest.rs:162-165) and a test at manifest.rs:403-415 reads the key list out of
  `schemas/app.json` and asserts each one reaches the NavX XML, so a schema change cannot drift
  away from the emitter again.
- `EdgeKind::TriggerInvocation` (80cb7756, 6d5f4e96). The variant, its `Display` arm and all three
  consumer branches are gone; a repo-wide grep outside `Docs/campaign/` finds no reference.
- `create_project` refusing to overwrite. `refuse_existing_destinations` (scaffold.rs:305-318) runs
  on the built-in path (scaffold.rs:201) and on the custom-template path (scaffold.rs:577), both
  after everything is rendered and before anything is written.
- The four file splits (`formatting/`, `symbols/`, `index/`, `oauth/`) carry the campaign's changes:
  `apply_brace_style` runs as a pre-pass at formatting/indent.rs:23 (the test-depth F1 fix), the
  `format_range` clamp is at formatting/range.rs:41/105 with its test at :205, the substring scan
  budget is at index/query.rs:12-57 with its test at :245, and no function name is defined twice
  in any of the four directories outside `#[cfg(test)]` and `#[cfg(unix)]` pairs.

## Review complete

Thirty-one high-severity fixes and twelve medium ones were read end to end; every one has a test
that fails without the change, and none was fixed by editing the test instead of the code.

One new bug: `with_specifies_prefix` byte-slices at index 9, so `fix tooltips` panics on a
localized `ToolTip` such as `Spécifié le numéro`, and the three tests added with it are all ASCII.

Two "degrade instead of fail" fixes lost their report on the way out: the permission set's
`skipped` files and the audit's `parseIssues` are in the JSON and in no CLI surface or contract,
so a project with one unreadable file now gets a quietly incomplete answer.

`dispatch_generate` refuses object id 50000, which Microsoft's object-range page and
PerTenantExtensionCop PTE0001 both call the first legal customization id, and the test pins the
wrong boundary.

Merge damage is small: `FileIndex::best_owner` and `object_path_where` are the same function under
two names, `object_path_of_kind_near` has no caller, and `SourceRange.f` is a basename from one
exit and an absolute path from the other. The four file splits and the `LineIndex`/`SourceLines`
merge are clean and kept every campaign change.

The record itself is the worst of it: eight findings already fixed in the tree are still written
`status: open`, seven of them duplicated inside `r1-analysis-insight.md`, and the multi-object open
item names nine queries when three of them no longer have the problem.
