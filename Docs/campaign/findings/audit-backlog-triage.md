# Audit backlog triage

Source: `AUDIT-BACKLOG.md` (227 findings, written 2026-07-31, before the campaign started).
Checked against the detached worktree `.claude/worktrees/agent-audit-triage` at commit
2b7bce37 (campaign branch tip) plus `git log -S`/`git log --oneline` in that worktree, and
cross-checked against `Docs/campaign/findings/*.md` and `Docs/campaign/STATE.md` in the main
checkout.

Verdicts: `fixed` (commit or code shown, file:line), `open` (still reproducible, file:line),
`not a defect` (explained), `moot` (named code is gone or rewritten), `unclear` (what would
settle it).

## Start here — highest-impact items

These 10 items each summarize several findings from the later sections. Decided on the evidence
recorded there, not re-investigated separately.

| Item | Verdict | Evidence |
|---|---|---|
| 1. Object names invisible to Zed (Syntax & Grammar, first four findings) | fixed | see Syntax & Grammar: `object_declaration` name field, outline.scm, highlights.scm @title, locals.scm all fixed |
| 2. File-corrupting code actions (add_parens, if_to_case, with_elimination, Move ToolTip, promoted-action) | fixed | see Analysis & Insight: all five code actions fixed, plus a shared apply-edit-and-reparse test harness now covers seven of them |
| 3. Interpreter record semantics (shared filter/cursor/buffer, inverted @ case, += bug, Get/FindFirst silent pass) | fixed | see Runtime & DAP: per-record-variable state via `TableRef` keying, filter case-sensitivity, compound assignment, and statement-position Get/Find are all fixed |
| 4. LSP responsiveness bugs (global debounce slot, generation read-lock across bridge calls) | fixed | see LSP & Protocol: per-URI debounce and the generation-lock-across-await pattern (extended to 5 more handlers plus al.compile) are both fixed |
| 5. Symbol index keyed by display name only, plus one corrupt app aborting workspace init | fixed | see Symbols & Project: `package_identity_key` now uses app GUID, and workspace init loads packages leniently |
| 6. XLIFF pipeline (id collisions/hash mismatch, nondeterministic refresh order, non-atomic overwrite) | fixed | see Emit, BC & Explorer: ids are now FNV-hash based, refresh output is sorted, and the write is temp-file + persist |
| 7. DAP correctness (breakpoints before launch dropped, off-by-one stack frames, every stop "breakpoint") | fixed | see Runtime & DAP: the pending-breakpoint queue, stack-frame line offset, and Break stop-reason mapping are all fixed |
| 8. Credential handling (cleartext http://, tenant header, argv-visible password, case-sensitive redaction) | fixed | see Emit, BC & Explorer: `is_safe_http_server`/port default, `?tenant=` query param, env-var password fallback, and case-insensitive redaction are all fixed |
| 9. Extension startup offline (cached al-lsp only reused after a successful GitHub lookup) | fixed | see Extension, CI & Docs: cache reuse is now version-stamped and does not require a network call |
| 10. CLI exit codes (debug/snapshot/profile subcommands exit 0 on failure) | fixed | see Emit, BC & Explorer: `debug stop` and siblings now exit non-zero on failure |

## Cross-cutting themes (each is a sweep-sized work item)

| Item | Verdict | Evidence |
|---|---|---|
| AL case-insensitivity sweep (attribute names, true/false, date/time literal suffixes) | fixed | attribute-name comparisons now use `eq_ignore_ascii_case` in al-symbols/events.rs, al-analysis/impact.rs and al-insight/graph.rs. `true`/`false` highlighting and `0d`/`0D` literal suffixes are both case-insensitive in the grammar (see Syntax & Grammar, LSP & Protocol is unaffected) |
| One-bad-file-fails-everything (workspace init, whole-workspace snapshots, daemon/MCP startup, dependency source indexing) | fixed | all four now degrade per-file/per-package: workspace init (Symbols & Project), `workspace_sources::snapshot` skip-and-report (Analysis & Insight), `initialize_daemon_workspace` per-file (LSP & Protocol), dependency source index per-package (Symbols & Project) |
| Edit-application test harness (no code-action test applies its edit and re-parses) | fixed | `assert_action_applies_cleanly_to` now covers add_parens, if_to_case, with_elimination, implement_interface, promoted, make_local, doc_region, events.rs and namespace.rs, keyed by URI for multi-file actions |
| Atomic writes for user files (xlf refresh and other writers) | fixed | `dispatch_xlf_refresh` now writes via temp-file + persist. `al rename` (al-explorer) does the same |
| Non-ASCII / UTF-16 coverage (no al-lsp position test, byte-vs-UTF-16 column confusion) | fixed | al-lsp gained UTF-16 position tests (tests/lsp_integration.rs, tests/lsp_transport.rs). `is_builtin_record_member`'s byte-to-UTF-16 column conversion is fixed (al-syntax/tokens.rs) |
| Unbounded caches (virtual-file cache, symbol disk cache) | fixed | both now have GC: `virtual_file::gc_cache_once` (30-day sweep) and `cache.rs::gc(max_age, max_total_bytes)` |
| Blocking work on async executors (audit all dispatch paths) | fixed | al.compile/al.findReferences/workspace-symbol now use spawn_blocking. All 9 insight dispatchers are wrapped in the `offload` helper. Did_change's parse/reindex moved to spawn_blocking |
| Panic hardening (~2,400 `.unwrap()` calls concentrated in al-analysis/al-lsp/al-symbols) | unclear | raw `.unwrap()` counts are now *higher* than the audit's baseline (al-analysis 781, al-lsp 601, al-symbols 387 vs the original 635/352/315), almost certainly from added test code. Settling this needs a count restricted to non-test, input-reachable call sites, which this pass did not do |
| Query & asset drift (languages/al vs tree-sitter-al/queries, single corpus test file) | fixed | the two query directories are byte-identical (verified with `diff`). The corpus now has 10 files (was 1) |

## Larger feature gaps worth scheduling (from limitations/comparison docs + audit)

| Item | Verdict | Evidence |
|---|---|---|
| LSP incremental text sync (machinery existed, capability advertised FULL only) | fixed | server now advertises `TextDocumentSyncKind::INCREMENTAL` (see LSP & Protocol) |
| Interpreter builtin catalog (Abs, Round, StrPos, CalcDate, Evaluate, WorkDate, GetLastErrorText/ClearLastError, asserterror capture, Date/DateTime Format, DateTime/Duration arithmetic, Text/Dictionary instance methods) | fixed | global builtin catalog, `Assert.ExpectedError`, Date-aware `Format`, DateTime/Duration arithmetic, and Text instance methods are all fixed (see Runtime & DAP). Dictionary instance methods are also dispatched (`records::supports_dict_method` covers add/get/set/containskey/remove/count/keys/values, records.rs:2516-2521) |
| Transitive dependency resolution (.app manifests' own dependencies parsed but never downloaded) | fixed | `next_transitive_dependencies` now resolves the transitive closure (see Symbols & Project) |
| al-bc endpoints (install/uninstall/status documented but absent, AAD device-code flow, NTLM or an honest error) | fixed | the module doc no longer overclaims these endpoints or a device-code flow. The Windows-auth path now warns honestly instead of implying NTLM might work, which is the alternative the audit itself named as acceptable (bc_client.rs:417-432) |
| #region/#endregion folding (both surfaces) and in-source #define/#undef evaluation | fixed | folds.scm now has `#region`/`#endregion` directive-based folding on both surfaces. The scanner's defines table now updates on in-source `#define`/`#undef` (see Syntax & Grammar) |
| Multiple objects per .al file in the source file index (only the first was indexed) | fixed | `FileIndex` now collects every object declaration, and the remaining single-object consumers (dead_code, duplicates, sql_patterns, arch_lint, impact, native_check) were fixed to iterate per-object nodes (al-analysis, fixed ae5f69c3) |
| Machine-translation provider hook for XLIFF suggestions (explicitly unbundled today) | open | `Docs/features/xliff-translation.md:99` still states the suggestion engine "does not call a machine-translation service or an external translation-memory database". No provider hook exists in the worktree |

## Syntax & Grammar (al-syntax, tree-sitter-al)

| Item | Verdict | Evidence |
|---|---|---|
| object_declaration name field never populated (grammar.js:260-292) | fixed | grammar.js:288 now `optional(prec.dynamic(1, field('name', ...)))`. R1-syntax-grammar.md confirms |
| outline.scm name: pattern dead (outline.scm:5-7) | fixed | outline.scm:4-6 name: field now populated, pattern matches |
| highlights.scm @title dead (highlights.scm:230-231) | fixed | highlights.scm:232-233 @title patterns now match populated name field |
| locals.scm object name local def dead (locals.scm:36-40) | fixed | locals.scm:34-37 object_declaration name: pattern now matches |
| scanner.c #define/#undef not interpreted (scanner.c:544-577) | fixed | scanner.c:637-647 updates defines table, per r1-syntax-grammar.md |
| date/time/datetime literal uppercase-only (grammar.js:1167-1169) | fixed | lowercase `0d`/`20240131d` now parse as date_literal, verified in r1 review |
| string/quoted_identifier swallow newlines (grammar.js:1172-1176) | fixed | quoted_identifier now `[^"\r\n]`. Unterminated quote confines ERROR to one line |
| op_and/op_or dead external tokens (grammar.js:188-195, complexity.rs) | fixed | op_and/op_or/op_div/op_mod/op_xor/op_is/op_as removed from grammar.js entirely. Complexity.rs matches operator_word text |
| highlights.scm op_and..op_xor dead captures (highlights.scm:70-77) | fixed | those capture patterns removed from current highlights.scm |
| tokens.rs classify_node directive returns None (tokens.rs:315-323) | fixed | directive now classified as PREPROCESSOR_KEYWORD |
| highlights.scm missing directive/inactive_code/time/datetime captures | fixed | highlights.scm:10-15 now captures time_literal, datetime_literal, directive, inactive_code |
| highlights.scm true/false case-sensitive (highlights.scm:15-16) | fixed | match is now case-insensitive per r1-syntax-grammar.md |
| tokens.rs classify_name_like_node dead field_declaration branch | fixed | TABLE_FIELD reachable via classify_table_field_name per r1 review |
| is_builtin_record_member byte vs UTF-16 column (tokens.rs:576-579) | fixed | code now explicitly converts byte column to UTF-16 before resolving |
| resolve_type/variables_at O(tokens x file) (tokens.rs, type_resolver.rs) | fixed | TypeResolver has line_starts table and per-scope memo per r1 review. Also caa16426 |
| lint.rs loop frame pushed without pop (lint.rs:198-244) | fixed | LoopFrame enum (Repeat/Body/AwaitingBody) + drain_awaiting_bodies. Test `statement_after_single_statement_do_loop_is_not_in_loop` |
| collect_label_symbols_from_text no comment strip (symbols.rs:1155-1206) | fixed | moved to symbols/labels.rs, masks comments via mask_non_code_keep_quoted_identifiers |
| sort.rs is_member_keyword single-line attribute only (sort.rs:277) | fixed | multi-line `[...]` attribute tracking added. Test `multi_line_attribute_stays_attached_to_its_procedure` |
| sort.rs protected var / internal local procedure not member-start | fixed | is_var_start handles "protected var". Tests for protected var and internal local procedure present |
| format_al unconditional blank-line collapse (formatting.rs:158-162) | fixed | formatting/indent.rs gates collapse_blank_runs on BlankLinesBetweenProcedures::Preserve |
| collect_global_vars merges globals across objects (type_resolver.rs) | fixed | globals/Rec now scoped to the enclosing object_declaration per r1 review |
| node_text_clean no unescape (lib.rs:95-103) | fixed | clean_identifier_text unescapes doubled quotes (bce5ec64). ~20 downstream call sites fixed too (9997aedf/30502a20) |
| brackets.scm missing {} pair, dead @open (brackets.scm:4-6) | fixed | languages/al/brackets.scm now has `("{" @open "}" @close)`, no orphan @open |
| outline.scm/brackets.scm drift tree-sitter-al vs languages/al | fixed | the two copies are byte-identical (diff confirms), including event_procedure_declaration/key_declaration/enum_value_declaration |
| indents.scm missing var_section indent / kw_else outdent | fixed | indents.scm now includes var_section/object_var_section in @indent and `(if_statement (kw_else) @outdent)` |
| folds.scm missing key/enum/event folds, no #region folding | fixed | folds.scm now includes all three plus #region/#endregion directive-based folding |
| highlights.scm property_keyword as @operator (highlights.scm:219) | fixed | now `(property_keyword) @property` |
| tree-sitter-al/test/corpus only 1 file | fixed | now 10 corpus files, 59 corpus cases per r1-syntax-grammar.md |
| grammar.js option_member dead rule (grammar.js:670-676) | fixed | option_member and its conflicts entries removed entirely from grammar.js |
| grammar.js comment token treats # as comment (grammar.js:1181) | fixed | comment rule now matches only `//` and `/* */`, with a comment explaining AL has no `#` comments |
| Docs parsing-and-syntax.md omits AL-NL010 (line 24,141) | fixed | doc now lists AL-NL010 at lines 24 and 144 |
| Docs claims and/or counted in complexity, code didn't (line 81) | fixed | complexity.rs now actually counts and/or via operator_word matching, so doc and code agree |
| Docs language-assets.md claims folds.scm folds attribute lists (line 25) | open | folds.scm:languages/al and tree-sitter-al/queries fold list has no `(attribute)` entry despite an `attribute` node existing in the grammar. Doc still overstates |

## Analysis & Insight (al-analysis, al-insight)

| Item | Verdict | Evidence |
|---|---|---|
| add_parens bare-call heuristic hits keyword statements (add_parens.rs:21-41) | fixed | r1-analysis-insight.md confirms `end();`/`break();` corruption no longer reproduces |
| if_to_case deletes trailing code + loses `;` (if_to_case.rs:71-83) | fixed | r1-analysis-insight.md confirms trailing-text/semicolon loss fixed |
| if_to_case flattens branch indentation (if_to_case.rs:57-68) | fixed | r1-analysis-insight.md confirms branch indentation preserved |
| with_elimination rewrites inside string literals (with_elimination.rs:297-354) | fixed | r1-analysis-insight.md confirms string-literal qualification fixed |
| with_elimination qualifies own-object procedure calls (with_elimination.rs:266-281) | fixed | r1-analysis-insight.md confirms own-procedure qualification fixed |
| with_elimination O(len^2 x fields) substitution (with_elimination.rs:315-318) | fixed | r1-analysis-insight.md confirms substitution perf fixed |
| with_elimination ignores tableextension fields (with_elimination.rs:22-30) | fixed | r1-analysis-insight.md confirms tableextension fields now resolved |
| implement_interface inserts stubs before single-line object (implement_interface.rs:218-220) | fixed | r1-analysis-insight.md confirms single-line insertion point fixed |
| implement_interface ignores interface inheritance (implement_interface.rs:43-64) | fixed | r1-analysis-insight.md confirms base-interface methods now stubbed |
| Move ToolTip deletes without moving (events.rs:72-91) | fixed | r1-analysis-insight.md confirms tooltip delete-only bug fixed. Events.rs now writes the table-side edit (ed298cbe fixed a related buffer-vs-disk issue on the same path) |
| in_page_field false-positives on action blocks (events.rs:34-64) | fixed | r1-analysis-insight.md confirms page-action false positive fixed |
| promoted.rs unquoted actionref/duplicate area/Caption mistranslation (promoted.rs:220-225) | fixed | r1-analysis-insight.md confirms all four sub-bugs fixed |
| make_local "local " string-contains check (make_local.rs:84-94) | fixed | r1-analysis-insight.md confirms contains("local ") check replaced |
| namespace.rs quoted-identifier mispairing (namespace.rs:147-197) | fixed | r1-analysis-insight.md confirms quoted-identifier pairing and comment/literal suppression fixed |
| Docs code-actions.md:30 vs doc_region emits //region not #region | fixed | r1-analysis-insight.md confirms #region emission fixed |
| Docs code-actions.md:25 overstates event-subscriber conversion | fixed | Docs/features/code-actions.md:25 now states the narrow scope exactly ("Scoped to a single-line attribute within +/-2 lines of the cursor") |
| Docs promoted.rs actions missing from table (promoted.rs:295,429) | fixed | Docs/features/code-actions.md now lists both "Add application area" (line 32) and "Fix report layout" (line 34) |
| sql_patterns.rs loop-tracking never pops single-statement loops (sql_patterns.rs:119-137) | fixed | r1-analysis-insight.md confirms loop state machine fixed |
| sql_patterns.rs misses bare FindFirst/FindLast (sql_patterns.rs:146-156) | fixed | r1-analysis-insight.md confirms bare Find forms now matched |
| sql_patterns.rs has_filter_before_findset one flag per procedure (sql_patterns.rs:141-189) | fixed | r1-analysis-insight.md confirms per-record filter tracking fixed |
| dead_code.rs orphaned-subscriber ignores _target_event (dead_code.rs:392-422) | fixed | r1-analysis-insight.md confirms orphaned-subscriber check now uses target event |
| Docs analysis-and-insight.md claims dead code parallel, code is serial | fixed | Docs/features/analysis-and-insight.md:66 now says "serially" with the rayon-hang rationale, matching dead_code.rs |
| workspace_sources.rs snapshot() fails whole query on one bad file | fixed | r1-analysis-insight.md confirms skip-instead-of-fail landed |
| duplicates.rs O(P^2) bigram rebuild (duplicates.rs:77-106,219-243) | fixed | r1-analysis-insight.md confirms bigram precomputation fixed |
| extract_table_relation_table misses conditional form (al-insight/analysis.rs:197-225) | fixed | r1-analysis-insight.md confirms conditional TableRelation parsing fixed |
| impact.rs member-scoped fallback has no receiver filter (impact.rs:119-123,327-348) | fixed | impact.rs now has declared_receivers/receiver_of/receiver_before binding logic gating member-scoped matches |
| impact.rs/graph.rs case-sensitive attribute comparisons (impact.rs:225, graph.rs:417-429) | fixed | r1-analysis-insight.md confirms attribute case-sensitivity fixed |
| graph.rs ensure_node drops second package's node (graph.rs:213-222) | fixed | r1-analysis-insight.md confirms per-package node identity fixed |
| graph.rs remove_edges_from stale EdgeIndex (graph.rs:349-359) | fixed | r1-analysis-insight.md confirms stale-EdgeIndex bug fixed |
| find_entry_points filters on unused Calls edges (search.rs:436-450, insight_dispatch.rs:54-61) | fixed | r1-analysis-insight.md confirms call-graph argument fixed |
| parse_run_trigger_arg wrong no-arg default (calls.rs:656-700) | fixed | r1-analysis-insight.md confirms trigger-arg default fixed |
| trace_event fabricates hops, shared visited set (search.rs:148-171) | fixed | r1-analysis-insight.md confirms fabricated-hops bug fixed |
| Docs overclaim trace_event_chain diamond/cycle fidelity (search.rs:320-339) | fixed | search.rs doc comment now states explicitly "cycle means already expanded, not back-edge" and explains the diamond fan-in case |
| collect_permissions re-parses every file, holds lock during parse (permissions.rs:51-74) | fixed | r1-analysis-insight.md confirms cached-parse reuse and lock scope fixed |
| rename/references full binder run per occurrence, no cache (rename.rs:107-162, references.rs:37-87) | fixed | both now construct a `binding::DeclLocCache` (rename.rs:157, references.rs:37) |
| No code-action test applies edit + re-parses (if_to_case.rs test module) | fixed | shared `assert_action_applies_cleanly_to` harness (test_support.rs) now covers if_to_case, with_elimination, promoted, events.rs, namespace.rs |
| remove_edges_from_clears_outgoing tests only one edge (graph.rs:1278-1305) | fixed | graph.rs now also has `remove_edges_from_clears_every_outgoing_edge`, a multi-edge test |
| annotation_edit only searches 16 lines for brace (mod.rs:417-421) | fixed | r1-analysis-insight.md confirms annotation_edit structural search fixed |

## LSP & Protocol (al-lsp, al-protocol)

| Item | Verdict | Evidence |
|---|---|---|
| Single global debounce slot cancels other files' diagnostics (lsp.rs:162) | fixed | per-URI `diag_tasks` map (lsp.rs:165) with tests at lsp/tests.rs `diagnostics_debounce_tests` |
| hover holds generation_lock read guard across bridge call (lsp.rs:1477) | fixed | hover/completion/inlayHint use `snapshot_after_ready` to drop the guard. R1-lsp-protocol.md found the same class in 5 more handlers (references, implementation, documentSymbol, semanticTokens, workspaceSymbol) and `al.compile`, also fixed (92da8dd9) |
| Request.id is u64, breaks string ids/notifications (jsonrpc.rs:18) | fixed | `RequestId` now handles string/null ids and the notification rule (jsonrpc.rs:22-125, daemon/mod.rs:463-522) |
| Stale buffered response after read timeout desyncs client (client.rs:434) | fixed | client drains abandoned response ids (client.rs:296,501). A related partial-frame desync bug found and fixed too (620956ba) |
| last_activity bumped after dispatch, idle reaper races long requests (daemon/mod.rs:424) | fixed | idle reaper uses an `InFlightGuard` (daemon/mod.rs:385) |
| initialize_daemon_workspace aborts wholesale on one oversized file (daemon/mod.rs:1002) | fixed | per-file degradation in `initialize_daemon_workspace` (daemon/mod.rs:1211) |
| require_project_root uses try_read, spuriously fails under transient lock (daemon/mod.rs:811) | fixed | `project_state_with_wait` replaces `try_read` (daemon/mod.rs:1000) |
| dispatch_breaking_changes calls block_in_place unguarded (build_dispatch/mod.rs:277) | fixed | guarded `block_in_place` confirmed at build_dispatch/mod.rs:268. A sibling unguarded call in `dispatch_format` was found and fixed too (00667357) |
| validate_schema_value never checks minItems (mcp.rs:293) | fixed | `minItems` now enforced (mcp.rs:386) |
| "id": null handled inconsistently between notification/request paths (mcp.rs:1014) | fixed | both MCP paths now treat `"id": null` as a request (mcp.rs:1029, 1200) |
| MCP stdio read_line has no byte cap (mcp.rs:1304) | fixed | bounded MCP stdio reads (mcp.rs:1351) |
| Full-document TextEdit end position off by one (formatting.rs:66) | fixed | `full_document_end` fixed (formatting.rs:76) |
| Server advertises FULL sync only despite incremental support (lsp.rs:891) | fixed | server now advertises `TextDocumentSyncKind::INCREMENTAL` (lsp.rs:991) |
| insert_text_format never set for snippet completions (completions.rs:69) | fixed | `insert_text_format` set for snippets (completions.rs:81) |
| handle_code_action ignores CodeActionContext.only (handlers.rs:60) | fixed | `CodeActionContext.only` filtering implemented (handlers.rs:64) |
| experimental/runnables ignores position filter (lsp.rs:2084) | fixed | runnables position filtering implemented (lsp.rs:2274) |
| MCP has no cancellation or concurrency (mcp.rs:1227) | fixed | concurrent MCP `tools/call` plus `notifications/cancelled` handling (mcp.rs:1376-1418) |
| Connection limit drops silently, per-connection dispatch sequential (daemon/mod.rs:227) | fixed | connection-limit rejection frame (daemon/mod.rs:311) and per-connection concurrent dispatch (fixed f4fe24a4) |
| al.findReferences runs workspace walk on async executor, not spawn_blocking (commands.rs:536) | fixed | `spawn_blocking` now used for `al.findReferences` and `workspace/symbol` (commands.rs:538, lsp.rs:2020) |
| trace/entrypoints/graphExport/impact/suggestEvent build call graph sync on async task (insight_dispatch.rs:46) | fixed | all five wrapped in the `offload` helper. 4 more dispatchers found with the same issue, also fixed (00667357) |
| Every codeAction request formats the entire document (handlers.rs:109) | fixed | cheap `document_needs_formatting` probe replaces the full format (handlers.rs:191) |
| Project-scope debounce recomputes whole workspace per keystroke burst (lsp.rs:737) | fixed | bounded retry (`MAX_STAGING_ATTEMPTS`) plus a longer `WORKSPACE_DIAGNOSTICS_DEBOUNCE` stop the unbounded per-keystroke respin (diagnostics.rs:309-370) |
| Docs omit textDocument/implementation and experimental/runnables (lsp-commands.md:8) | fixed | Docs/reference/lsp-commands.md now lists `implementation` (line 8) and documents `runnables` with `position` filtering (line 16) |
| Docs reference nonexistent server/conversions.rs (language-server.md:26) | fixed | doc now correctly names `al_analysis::lsp::flatten_document_symbols` |
| lsp_integration.rs never drives a real transport (tests/lsp_integration.rs:1) | fixed | black-box transport tests added (tests/lsp_transport.rs) |
| No non-ASCII/UTF-16 LSP position test | fixed | UTF-16 tests added (tests/lsp_integration.rs:1669-1732, tests/lsp_transport.rs:144) |
| Debounce machinery (schedule_diagnostics) has no test (lsp.rs:689) | fixed | `diagnostics_debounce_tests` module in lsp/tests.rs covers the cross-file cancellation case directly |

## Symbols & Project (al-symbols, al-semantic, al-types, al-source, al-project, al-workspace)

| Item | Verdict | Evidence |
|---|---|---|
| Index keyed by display name only, same-name apps evict each other (index.rs:543) | fixed | `package_identity_key(app_id, name)` (index/mod.rs:41-48) keys by GUID when present, name only as fallback. Test `distinct_apps_sharing_a_name_are_both_kept_in_order` |
| Event discovery attribute match case-sensitive (events.rs:58) | fixed | `attr.name.eq_ignore_ascii_case(...)` (events.rs:88-100). Test `attribute_matching_is_case_insensitive` |
| Synthetic Option-enums keyed wrong, per-namespace suppression (model.rs:947-976) | fixed | `existing_enum_names` now accumulated across all namespace levels (model.rs:882-889) |
| 256 KiB header window can truncate an unquoted name (source_index.rs:124-134) | fixed | explicit boundary-straddle test present (source_index.rs ~1299-1323) |
| Corrupt .app aborts whole workspace init (al-workspace/lib.rs:778-781) | fixed | Docs/features/symbol-and-package-engine.md: "a corrupt or truncated .app...is skipped with a per-package warning instead of aborting initialization" |
| One bad embedded .al rejects the whole dependency source generation (al-workspace/lib.rs:418-429) | fixed | per-file skip already existed. The remaining per-package all-or-nothing was found and fixed in the same round (58645d20) |
| select_version doesn't enforce >= requested minimum (nuget.rs:452-462) | fixed | `parse_version(version) >= minimum` filter with explicit comment (nuget.rs:498-505). Test `version_selection_rejects_candidates_below_requested_minimum` |
| find_project aborts on an unrelated ancestor's malformed app.json (project.rs:207-213) | fixed | ancestor failures are now warn-only (project.rs:226-238). Tests `find_project_ignores_malformed_ancestor_manifest_when_nothing_is_found` etc. |
| search_dir_recursive picks filesystem-order-dependent alc.dll, not newest (toolchain.rs:333-374) | fixed | candidates collected and picked by newest version. Test `search_dir_recursive_selects_newest_version_deterministically` |
| render_outline always prints numeric id, even for name-scoped kinds (virtual_file.rs:469-477) | fixed | `entry.kind.requires_numeric_id()` gates the id part (virtual_file.rs:611-617) |
| find_object_range fails for ID-less embedded kinds (virtual_file.rs:216-247) | fixed | prefix search now branches on `requires_numeric_id()`. Test `find_object_range_matches_idless_declarations` |
| format_name hardcoded quoting character set (virtual_file.rs:407-420) | fixed | now quotes anything that isn't a plain ASCII identifier, not a fixed allowlist (virtual_file.rs:542-561) |
| get_composed picks first same-kind entry, no workspace preference (composition.rs:33-34) | fixed | candidates now sorted workspace-first, then deterministic package/id order (composition.rs:37-52) |
| NavxManifest.dependencies parsed but never used for transitive resolution (manifest.rs:36) | fixed | `next_transitive_dependencies` resolves the transitive closure (symbols_auth.rs:243-559) |
| strip_json_comments has no block-comment support (al-types/jsonc.rs:9-44) | fixed | r1-symbols-project.md confirms block-comment support added to al-types::jsonc |
| index_from_result records only the first object per file (file_index.rs:493-519) | fixed | file_index now collects every object declaration (`collect_object_declarations`). Ownership-keying bug fixed separately (a8ead4ab) |
| scan_package_folders accepts any symlink with .app extension (project.rs:376-382) | fixed | test `scan_package_folders_skips_dangling_and_directory_symlinks` |
| Package-name comparison inconsistent: Unicode lowercase vs ASCII-only fold (index.rs:391,844) | fixed | single `fold_name` (Unicode `to_lowercase`) used consistently across entries.rs/loading.rs/caches.rs/query.rs/removal.rs |
| get_events clones all_entries + deep-clones every match (events.rs:50-106) | fixed | now reads from a cached `index.event_catalog()` instead of a fresh full scan |
| replace_with deep-clones every SymbolEntry (index.rs:195-233) | fixed | comment and code confirm Arc payloads are shared, not deep-cloned (index/loading.rs:153-161) |
| find_zip_offset fallback pays full ZipArchive::new per PK signature (app_reader.rs:172-178) | fixed | attempt count capped at 64 (earlier fix). R1-symbols-project.md hardened further (8095dcd7) since each attempt was still costly |
| search/search_in_package O(total symbols) per query (index.rs:781-790,836-853) | fixed | `search_in_package` uses the `by_package` bucket. Substring stage capped at a 20,000-name scan budget (75b96eac) |
| Virtual-file cache filename never deleted, grows unbounded (virtual_file.rs:320-367) | fixed | `gc_cache_once` sweeps stale entries. Test `gc_cache_removes_stale_entries_and_empty_package_dirs` |
| Symbol disk cache has no GC besides clear() (cache.rs:284-289) | fixed | `cache.rs::gc(max_age, max_total_bytes)` with dedicated tests |
| search_dotnet_tool_store lexicographic sort picks 9.x over 17.x (toolchain.rs:307-308) | fixed | package dirs now sorted with numeric digit-run comparison (toolchain.rs:307-310). Test asserts "17.x package must beat the 9.x package" |
| Docs claim deterministic package replace despite name-only keying (symbol-and-package-engine.md:37-38) | fixed | doc now correctly describes GUID-based identity keying with name fallback, matching the code |
| Docs claim outline always renders valid AL (symbol-and-package-engine.md:177) | fixed | now true given the id-less-kind and format_name fixes above |
| No test for two packages same display name, different GUID (index.rs) | fixed | `distinct_apps_sharing_a_name_are_both_kept_in_order` |
| No test for non-canonical attribute casing (events.rs) | fixed | `attribute_matching_is_case_insensitive` |
| No test for header-boundary-straddle truncation (source_index.rs) | fixed | boundary-straddle test present near source_index.rs:1299 |
| No test for below-minimum-only version selection (nuget.rs) | fixed | `version_selection_rejects_candidates_below_requested_minimum` |

## Runtime & DAP (al-runtime, al-test, al-test-harness, al-dap)

| Item | Verdict | Evidence |
|---|---|---|
| String-literal unescaping strips all leading/trailing quotes (eval_expr.rs:100) | fixed | r1-runtime-dap.md confirms fixed, not re-reported |
| `Rec.Amount += 5` stores raw RHS, not Amount+5 (eval_expr.rs:769) | fixed | r1-runtime-dap.md confirms compound record-field assignment fixed |
| No DateTime/Duration arithmetic (eval_expr.rs:1175) | fixed | r1-runtime-dap.md confirms DateTime/Duration arithmetic fixed |
| coerce_into_slot no i32 range check on BigInteger (value.rs:315) | fixed | r1-runtime-dap.md confirms i32 narrowing check fixed |
| CASE label runtime error silently swallowed (eval_stmt.rs:558) | fixed | r1-runtime-dap.md confirms CASE label error propagation fixed |
| Assignment to unbound name auto-binds a new variable (eval_stmt.rs:641) | fixed | r1-runtime-dap.md confirms fixed |
| Global builtin catalog covers only ~15 functions (dispatch.rs:335-465) | fixed | r1-runtime-dap.md confirms the missing builtin catalog fixed |
| asserterror swallows ErrorInfo, no GetLastErrorText support (eval_stmt.rs:694) | fixed | r1-runtime-dap.md confirms asserterror error capture fixed. `Assert.ExpectedError` also wired in (036b8227) |
| builtin_format ignores Length/FormatStr arguments (dispatch.rs:1119) | fixed | r1-runtime-dap.md confirms Format's length/format arguments fixed |
| render_value renders Date/Time/DateTime as raw integers (dispatch.rs:1301-1303) | fixed | test `format_renders_dates_not_raw_carriers` (dispatch/render.rs:342) |
| substitute_placeholders re-scans already-substituted text (dispatch.rs:1317) | fixed | r1-runtime-dap.md confirms placeholder re-scanning fixed |
| CopyStr raises instead of truncating past the end (dispatch.rs:1171) | fixed | r1-runtime-dap.md confirms CopyStr-past-the-end fixed |
| Record variables of the same table share filter/cursor/buffer (records.rs:66) | fixed | stores now keyed by `TableRef` (table name plus owning variable's handle). R1-runtime-dap.md's "Record X temporary" fix (abbb8e07) confirms per-variable views |
| Get/FindFirst miss silently passes in statement position (records.rs:626-629) | fixed | test `statement_position_get_miss_errors` (records_tests.rs:1298) asserts "does not exist" |
| SetFilter substitutes %1..%10 in ascending order, corrupting %10 (records.rs:667-673) | fixed | r1-runtime-dap.md confirms SetFilter placeholder ordering fixed |
| Unset fields read back as Empty, not typed zero (mock/record.rs:188) | fixed | r1-runtime-dap.md confirms unset-field typed zeros fixed (`Value::default_for` Option/Enum arm, 84144137) |
| Primary-key Ord is case-sensitive Code and Integer != Decimal (mock/record.rs:150,235) | fixed | `normalize_key_value` uppercases Code and converts Integer to Decimal (mock/record.rs:226-232). Test `decimal_primary_key_equality_is_consistent` |
| FlowField Boolean CONST parses as Text, matches zero rows (mock/record.rs:600-611) | fixed | r1-runtime-dap.md confirms FlowField Boolean constants fixed |
| BC filter case-sensitivity inverted (mock/filter.rs:211-233) | fixed | r1-runtime-dap.md confirms filter case-sensitivity fixed. Tests `test_at_prefix_is_case_insensitive`, `code_cell_is_caseless_even_without_at_prefix` |
| Quoted empty string filter token fails to parse (mock/filter.rs:275) | fixed | r1-runtime-dap.md confirms quoted empty filter tokens fixed |
| Set-literal re-parses a synthetic codeunit per member per evaluation (eval_expr.rs:416-467) | fixed | test `set_literal_in_loop_uses_cached_fragment_parses` |
| dispatch_workspace_procedure re-walks the AST and re-collects params per call (dispatch.rs:512-562) | open | crates/al-runtime/src/interpreter/dispatch/workspace_procedure.rs:65-121: parse is now cached (`ctx.source.get_cached_parse`), but the procedure-node walk and `collect_params`/`collect_return` still re-run on every call with no memoization by procedure name |
| DeleteAll is O(n^2 log n) (records.rs:757-773) | fixed | r1-runtime-dap.md confirms DeleteAll complexity fixed |
| Bare global calls outside PLATFORM_GLOBALS wrongly routed to Interp (al-test/router.rs:917-949) | fixed | r1-runtime-dap.md confirms the router's bare-global safe-list fixed |
| test_equality_text locks in inverted case-insensitive default (mock/filter.rs:434-439) | fixed | replaced by `test_at_prefix_is_case_insensitive`, `code_cell_is_caseless_even_without_at_prefix`, `at_prefix_lower_matches_any_case` |
| Breakpoints set before launch permanently dropped (native_dap.rs:272) | fixed | r1-runtime-dap.md confirms the initialized/pending-breakpoint queue fixed |
| Stack frames off by one, StatementSpan.From ignored (native_dap.rs:1880-1890) | fixed | r1-runtime-dap.md confirms line off-by-one and StatementSpan.From fixed |
| supportsDelayedStackTraceLoading advertised but startFrame/levels ignored (native_dap.rs:265,999) | fixed | r1-runtime-dap.md confirms startFrame/levels paging fixed |
| Every Break event reported as stop reason "breakpoint" (bc_debug/events.rs:152-154) | fixed | r1-runtime-dap.md confirms Break stop reasons fixed |
| handle_configuration_done doesn't retry per BC's OnAttachedToConnection timing (native_dap.rs:285-291) | fixed | r1-runtime-dap.md confirms the configurationDone retry fixed. A follow-up unbounded-retry bug found and fixed too (61b20718) |
| Docs vs code disagree on attach breakOnNext default (bc_debug/session.rs:633 vs al.json:88) | fixed | session/mod.rs:598-601 now defaults to `"WebServiceClient"`, matching the schema. Regression test comments confirm the fix |
| Docs claim case-sensitive filters, actual default is inverted (native-test-runtime.md:64) | fixed | doc now states "unprefixed Text patterns match case-sensitively, the @ prefix makes a pattern case-insensitive, and Code cells always compare caselessly" |
| No test for statement-position record semantics / Format args / Date rendering (dispatch.rs tests, regression_tests.rs) | fixed | `statement_position_get_miss_errors`, `format_renders_dates_not_raw_carriers`, and format/date-specific tests now present |
| Text/Dictionary instance methods have no dispatch path (eval_stmt.rs:770-851) | fixed | test `text_instance_methods_execute_locally` (Replace/Trim/Contains/Substring execute locally) |

## Emit, BC & Explorer (al-emit, al-compile, al-bc, al-publish, al-snapshot, al-explorer)

| Item | Verdict | Evidence |
|---|---|---|
| make_translation_id emits duplicate ids for page-layout fields (xliff.rs:352-368) | fixed | ids are now FNV `name_hash`-based. Page controls get distinct ids, test `page_controls_get_distinct_translation_ids` |
| extract_from_file never resets field_id/current_field (xliff.rs:130-181) | fixed | extractor rewritten around a brace-depth stack (xliff/extract.rs:56-96) that re-anchors per member, not a stateful field_id carried across lines |
| XLIFF extractor ignores Locked = true (xliff.rs:303-325) | fixed | tests `locked_strings_are_excluded`, `locked_captions_and_tooltips_are_excluded` |
| detect_object_header only scans first 10 lines (xliff.rs:215) | fixed | extract_from_file now iterates `text.lines()` over the whole file with no line cap (xliff/extract.rs:63) |
| Docs claim Microsoft-compatible trans-unit ids (xliff-translation.md:7-16) | fixed | ids are now FNV name_hash based, matching alc's format |
| xlf refresh output order nondeterministic (xliff.rs dispatch:146-151) | fixed | obsolete units sorted before append |
| xlf refresh overwrites original attribute with locale stem (xliff.rs dispatch:153-158) | fixed | app name now read from the file's own `original` attribute |
| dispatch_xlf_refresh overwrites the user's .xlf non-atomically (xliff.rs dispatch:165) | fixed | write is now a temp-file + persist |
| xlf refresh fallback picks first *.g.xlf nondeterministically (xliff.rs dispatch:87-106) | fixed | an ambiguous `*.g.xlf` set is now a hard error instead of an arbitrary pick |
| Docs claim refresh removes obsolete units, it does not (xliff-translation.md:23-24) | fixed | doc now honestly states obsolete units are kept with state `final`, not deleted |
| AuthMethod::Windows is Basic, doc claims NTLM (bc_client.rs:357-371) | fixed | module doc now states plainly "also plain HTTP Basic... not a real NTLM/Negotiate handshake" |
| build_base_url prepends http://, no port default, no allowlist (bc_client.rs:441-459) | fixed | `build_base_url` enforces `is_safe_http_server` and the 7049 default |
| publish sends tenant as X-Tenant header, not query param (bc_client.rs:382-387) | fixed | tenant is now a `?tenant=` query param, with a mock test |
| bc_client.rs documents install/uninstall/status endpoints that are absent (bc_client.rs:4-8) | fixed | module doc now explicitly states these are not implemented and why, instead of implying they exist |
| bc_client.rs claims AAD device-code flow that does not exist (bc_client.rs:15,373-377) | fixed | module doc now explicitly states no interactive device-code flow exists and publishes fail fast without a pre-provisioned token |
| sanitize_error_body redaction case-sensitive (bc_client.rs:52-59) | fixed | redaction confirmed case-insensitive. A related Basic-credential redaction gap found and fixed too (49458927) |
| snapshot.rs re-wraps errors losing HTTP status (snapshot.rs:113-118,155-160) | fixed | snapshot errors now preserve the real HTTP status |
| --password argv-only, no env fallback (subcommands.rs:120-182) | fixed | `bc_server_params` now falls back to `BC_USERNAME`/`BC_PASSWORD` |
| snapshot/profile --company defaults to empty string (subcommands.rs:114-116) | fixed | `--company` is now `required = true`, no silent empty default |
| lint joins file args with spaces (cli/mod.rs:98-108) | fixed | `resolve_lint_targets` now handles multiple file arguments |
| test-results --method missing requires=codeunit (args.rs:533-535) | fixed | clap `requires` enforced. Test `test_results_method_without_codeunit_is_rejected` |
| format has no conflicts_with between file/--stdin/--all (language.rs:101-110) | fixed | `conflicts_with_all` now declared. Tests `format_stdin_and_all_conflict`, `format_file_and_stdin_conflict` |
| validate_with_alc copies to a predictable shared tmp path (build.rs:291-317) | fixed | now uses `tempfile::tempdir()` |
| debug/snapshot/profile subcommands exit 0 regardless of status (debug.rs:223-245) | fixed | `debug stop` (and siblings) now exit non-zero when the operation failed |
| collect_al_files follows symlinks with no cycle detection (al-emit/project.rs:517-539) | fixed | now iterative with a canonicalized visited set |
| control_addin_bundle emits duplicate archive paths (assemble.rs:797-808) | fixed | de-duplicates outer `addin/src/` entries |
| analyze_profile_file has no size cap (profiling.rs:460-468) | fixed | now capped at 500 MB, matching every other BC input path |
| object_browser.rs mouse hit-testing uses stale rectangles after resize (object_browser.rs:143-151) | fixed | hit-testing now uses the last rendered area |
| Docs claim generate-completions exports symbol data (scaffolding-and-codegen.md:52-55) | fixed | doc now matches the actual shell-completion-script behavior |
| rad_publish has zero test coverage (bc_client.rs:330-350) | fixed | wiremock coverage added in both al-bc and al-publish |
| XLIFF extraction tests cover only a table fixture (xliff.rs:1330-1368) | fixed | tests now cover page/pageextension extraction, Locked exclusion, and duplicate-id handling (`page_extension_controls_are_extracted`, `locked_captions_and_tooltips_are_excluded`, `workspace_extraction_drops_duplicate_ids`) |
| extract_single_quoted drops `Caption = '';` (xliff.rs:327-350) | fixed | empty captions are now emitted, per r1-emit-bc-explorer.md's backlog re-verification pass |

## Extension, CI & Docs (src/, themes, schemas, scripts, workflows)

| Item | Verdict | Evidence |
|---|---|---|
| Cached al-lsp only reused after a successful GitHub release lookup (src/lib.rs:134-141) | fixed | now version-stamped so a cached download is trusted without a network call on offline start. A follow-up "never upgrades" regression was found and fixed too (4c377a73) |
| cached_binary_path leaks a user-configured path across LSP/DAP/MCP surfaces (src/lib.rs:108-111) | fixed | src/lib.rs:466 comment: "Deliberately NOT cached into self.cached_binary_path: this path is [surface-specific]" |
| context_server_command only reads dotnet path from LSP-populated cache (src/lib.rs:360-379) | fixed | now falls back to reading the context server's own project settings directly (src/lib.rs:759-770) |
| build_dap_binary appends unparsed /server:/browser: args (src/dap.rs:49-54) | fixed | dead flags removed entirely. Comment explains they were "pure dead plumbing" |
| dap_config_to_scenario emits keys absent from the schema, omits required ones (src/dap.rs:100-121) | fixed | now emits schema-required `adapter`/`label`. Dead `projectDir` removed. Tests assert both |
| set_nested_value_inner silently drops a setting on type collision (src/settings.rs:80-88) | fixed | now coerces the intermediate value into an object instead of dropping (src/settings.rs:82-91) |
| Doc comment cites nonexistent nested setting examples (src/settings.rs:14-15) | fixed | doc now cites `al.formatting.maxLineLength` and correctly notes `al.inlayHints.parameterNames` has no `.enabled` leaf |
| settings_test.rs pins a bogus nested key (src/settings_test.rs:13-25,54-65) | fixed | test now uses the real `al.compilationOptions` array setting |
| src/dap.rs has zero unit tests (src/dap.rs) | fixed | src/dap.rs now has 9 tests covering scenario building and schema validation |
| Multi-shape setting precedence is untested and order-dependent (src/settings.rs:27-45) | fixed | test `multi_shape_precedence_is_alphabetical_last_write_wins` pins the order |
| Light theme reuses dark theme's hover/selection colors (themes/bc-themes.json:178-182) | fixed | accessibility fixes moved into the generator (84ce743d, grammar d8c4cc7) |
| Light theme syntax.attribute is placeholder red (themes/bc-themes.json:196) | fixed | current file: light theme attribute color is `#AF00DB`, not `#FF0000` |
| Theme family has no $schema, one players entry (themes/bc-themes.json:1) | fixed | current file declares `$schema: https://zed.dev/schema/themes/v0.2.0.json` and 8 players entries per theme |
| cargo clippy excludes zed-al everywhere, extension never linted (ci.yml:79,143,160-203) | fixed | a dedicated `cargo clippy -p zed-al --all-targets` job now exists (ci.yml:226-230) |
| cargo-deny excludes zed-al (ci.yml:158) | fixed | cargo-deny now runs `--workspace` with a comment explaining zed-al is included |
| CI triggers only on main/dev, tagging elsewhere hangs release for the full timeout (ci.yml:3-7, release.yml:41) | fixed | `CI_DISCOVERY_TIMEOUT_SECONDS` (default 120s) now bounds the "wait for a first run" phase separately, failing fast with an actionable message instead of hanging the full CI_TIMEOUT_SECONDS |
| checksums.txt omits extension.wasm/extension.toml (release.yml:263-278) | fixed | checksum generation now globs `*.tar.gz`, `*.zip`, `extension.wasm`, and `extension.toml` |
| check-doc-paths.sh uses mapfile, breaks on macOS's bash 3.2 (scripts/check-doc-paths.sh:38,53) | fixed | mapfile replaced with a portable read loop, comment cites the exact bash 3.2 incompatibility |
| check-release-hygiene.sh uses mapfile, same incompatibility (scripts/check-release-hygiene.sh:337) | fixed | same fix applied, comment references bash 3.2 |
| dev-watch.sh empty array expansion fails under set -u (scripts/dev-watch.sh:16,50,72) | fixed | now uses `${BRIDGE_BUILD_ARGS[@]+"${BRIDGE_BUILD_ARGS[@]}"}`. `set -euo pipefail` is present |
| dev-watch.sh doesn't watch schemas/settings.json (scripts/dev-watch.sh:67) | fixed | watch list now includes `-w schemas` |
| .zed/tasks.json references nonexistent validate-real-projects.sh (.zed/tasks.json:28) | fixed | that task entry is no longer present in .zed/tasks.json |
| make clean fails on a machine without dotnet (Makefile:367) | fixed | now checks `command -v dotnet` before invoking `dotnet clean`, skipping it outright when absent |
| ZED_EXT_DIR hardcoded to the Linux path (Makefile:24,74-81) | fixed | Makefile now branches between the macOS `Library/Application Support/Zed` path and the Linux `.local/share/zed` path |
| .cargo/mutants.toml excludes a nonexistent al-zed-test crate (.cargo/mutants.toml:11) | fixed | exclude_globs now lists only `**/build.rs` |
| README misstates the al-lsp resolution order (README.md:215-219) | fixed | README now accurately describes version-matched cache reuse and the offline fallback |
| README links to a broken anchor (README.md:422) | fixed | link now reads `#project-file-schemas`, matching the real heading |
| mcp-tools.md links to a broken anchor (Docs/reference/mcp-tools.md:46) | fixed | link now reads `#mcp-debug-control`, matching the real heading |
| examples/zed-settings.jsonc omits 5 settings while claiming completeness (examples/zed-settings.jsonc:12-13) | fixed | file now includes `al.dotnetPath` and all four `al.formatting.*` keys |
| Docs/README.md index omits benchmarks.md and comparison-zed-vs-vscode.md (Docs/README.md:7-49) | fixed | both are now listed in the index |
| BENCHMARKS.md names a kernel version that does not exist (BENCHMARKS.md:9) | fixed | doc now clarifies it is "a CachyOS custom build, not a mainline Linux release" |
| debug_adapter_schemas/al.json declares no $schema (debug_adapter_schemas/al.json:1) | fixed | file now declares `"$schema": "http://json-schema.org/draft-07/schema#"` |

## Queue for workstream A

Ranked by user impact, highest first. Only 3 items across the whole backlog are still open.

1. **dispatch_workspace_procedure re-walks the AST and re-collects params on every call** (`crates/al-runtime/src/interpreter/dispatch/workspace_procedure.rs:65-121`). Failure scenario: an interpreted test that calls the same helper procedure inside a loop (a common pattern for row-by-row assertions) pays a full tree walk plus parameter/return-type collection on every iteration, since the parse is cached but the procedure lookup is not memoized by name. On a large object with many procedures, or a loop with many iterations, this measurably slows local test runs.
2. **No machine-translation provider hook for XLIFF suggestions** (`Docs/features/xliff-translation.md:99`, `crates/al-analysis/src/xliff` suggestion engine). Failure scenario: a translator working through `xlf.suggest` gets only translation-memory and workspace-name matches. There is no fallback to an external MT service for a string with no prior translation, so untranslated strings with no historical match get no suggestion at all.
3. **folds.scm doc claims "attribute lists" are foldable, they are not** (`Docs/features/language-assets.md:25`, `tree-sitter-al/queries/folds.scm` and `languages/al/folds.scm`). Failure scenario: a developer reads the doc, expects to fold a long `[EventSubscriber(...)]` attribute block, and finds no fold point over the `attribute` node — a documentation-only mismatch with no functional breakage elsewhere.

## Unclear

- **Panic hardening (~2,400 `.unwrap()` calls in the 2026-07-31 audit, concentrated in al-analysis/al-lsp/al-symbols).** Raw counts today are higher (al-analysis 781, al-lsp 601, al-symbols 387), which is expected if most of the growth is test code, but this pass did not separate test from production call sites. What would settle it: re-run the count restricted to non-test files, then narrow further to call sites reachable from client/network/package input (the audit's own stated priority), and compare that filtered number against a baseline taken the same way.

## Triage complete

253 items decided: 249 fixed, 3 open, 1 unclear, 0 moot, 0 not a defect.

- The 227 numbered findings across the 7 subsystem sections: 225 fixed, 2 open (folds.scm attribute-list docs in Syntax & Grammar, and the interpreter's per-call procedure-lookup memoization in Runtime & DAP), 0 unclear.
- The 26 framing items across Start here (10), Cross-cutting themes (9) and Larger feature gaps worth scheduling (7): 24 fixed, 1 open (the machine-translation provider hook), 1 unclear (panic-hardening counts).
- Sections done, in file order: Start here, Cross-cutting themes, Larger feature gaps worth scheduling, Syntax & Grammar, Analysis & Insight, LSP & Protocol, Symbols & Project, Runtime & DAP, Emit BC & Explorer, Extension CI & Docs.
