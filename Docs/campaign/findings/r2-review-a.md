# R2 review A: al-analysis, al-insight, al-syntax, al-source, al-symbols, al-runtime, al-test, al-emit

Adversarial review of `git diff dev..campaign/2026-09-21` for the eight crates above.
Reads the finding, the commit, the current code and the test for each sampled item.
Everything below is verified against the tree at `campaign/2026-09-21` unless tagged
`[UNVERIFIED]`.

## Coverage

Fix verification, high severity (finding -> commit -> code -> test):
- [ ] r1 with_elimination: dropped semicolon (cc9f231b)
- [ ] r1 with_elimination: deletes text before `with` (cc9f231b)
- [ ] r1 make_local: attribute line (0108b968)
- [ ] r1 bulk_fix: `Specifies` prefix (c375402b)
- [ ] r1b rename: quoted identifier no-op (eae29618)
- [ ] r1b rename: EventSubscriber references (9148954c)
- [ ] r1b resolution: `parse_field_line` parenthesis (be039a6c)
- [ ] r1b resolution: symbol package docs per keystroke (7fab7e36)
- [ ] r1b xliff: empty object type/id/name (95e80294)
- [ ] r1b xliff: multi-object file (95e80294)
- [ ] r1b obsolescence: object-level ObsoleteState (15d7b1c0)
- [ ] r1b obsolescence: obsolete fields (15d7b1c0)
- [ ] r1b obsolete_usage: double snapshot (15d7b1c0)
- [ ] r1c source.rs: multi-object id and code (4180a321)
- [ ] r1c audit: `system` permission grant (401e5022)
- [ ] r1c audit: over-granted rights, repeated trigger name (5996424f)
- [ ] r1c suggest_event: `Database::` scope (3302ac49)
- [ ] r1c test_coverage: dead direct pass (334afb5c)
- [ ] r1c workspace_sources: `object_infos` had no consumer (74acc262)
- [ ] r1c signature: receiver ignored (52b75eda)
- [ ] r1c code_lens: uncached decl_loc (eebb9c98)
- [ ] r1 syntax sort_members: multi-object corruption (24ffeeea)
- [ ] r1 runtime: Option/Enum typed zero (84144137)
- [ ] r1 runtime: SetFilter Date/Option placeholders (ef9c11e5)
- [ ] r1 runtime: Round midpoint (182c5c93)
- [ ] r1 runtime: temporary record store (abbb8e07)
- [ ] r1 runtime: fall-off-the-end return value (c1ec0077)
- [ ] r1 runtime: Text[N]/Code[N] enforcement (d32104f8)
- [ ] r1b test: `--filter` trailing pattern (40a64ccf)
- [ ] r1 emit: control add-in path containment (16731e97)
- [ ] r1 emit: resourceExposurePolicy key (7dfd746e)

Fix verification, spread of medium and low:
- [ ] r1 make_local PERF lowercase scan (c4a9839f)
- [ ] r1 events.rs open-buffer table edit (ed298cbe)
- [ ] r1 impact/analysis conditional TableRelation (88a5fd98)
- [ ] r1 permissions: one unparseable file (9d02c3b4)
- [ ] r1 inlay_hints source_line (387df64c)
- [ ] r1c test_diagnostics: line 0 sentinel (ec3cb172)
- [ ] r1 syntax: clean_identifier_text call sites (bce5ec64)
- [ ] r1 syntax: ts_range_to_syntax / LineIndex (caa16426)
- [ ] r1 symbols: file_index owner per (name, kind) (a8ead4ab)
- [ ] r1 symbols: source_index cache eviction (ddaad70f)
- [ ] r1b runtime: junit control characters (443ef7cc)
- [ ] r1b runtime: cobertura double count (ffb0807e)

Merge damage:
- [ ] `resolve_object_path` / `FileIndex::object_path_where`
- [ ] `source.rs` object ranges and `member_signature`
- [ ] `LineIndex` / `SourceLines`
- [ ] split `al-syntax/src/formatting/`
- [ ] split `al-syntax/src/symbols/`
- [ ] split `al-symbols/src/index/`
- [ ] split `al-symbols/src/oauth/`
- [ ] duplicated helpers across the eight crates

Flagged behaviour changes, every consumer:
- [ ] `PermissionCollection { entries, skipped }`
- [ ] `dispatch_generate` refusing ids at or below 50000
- [ ] `create_project` refusing to overwrite
- [ ] `Round` semantics
- [ ] temporary record isolation
- [ ] `Text[N]` enforcement
- [ ] `TestDiagnostic.line` optional
- [ ] `PermissionAuditReport.parseIssues`
- [ ] `EdgeKind::TriggerInvocation` removed
- [ ] `WorkspaceSource::objects`

Quality of the new code:
- [ ] comments restating code, defensive branches, speculative parameters, one-caller wrappers
- [ ] tests that assert nothing meaningful
- [ ] duplicated helpers across crates
- [ ] panics reachable from user input
- [ ] over-long functions added during the campaign
- [ ] naming against the surrounding code, needless `pub`

Multi-object open item:
- [ ] the nine workspace queries that still walk from the file root
- [ ] whether any campaign fix assumed otherwise

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

## Verified fixes
