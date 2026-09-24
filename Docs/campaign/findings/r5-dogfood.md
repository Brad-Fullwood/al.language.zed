# R5 dogfood: `al-explorer` on a Base Application 26 project

What was run: the `al-explorer` CLI (and, for `codeActions`, which has no CLI command, the daemon
socket directly) built from commit `fea0d721`, against a small per-tenant extension project:
`app.json` (runtime 15.0, target Cloud, idRanges 50100-50149), `src/` with a tableextension of
Customer adding field 50100 "Loyalty Tier", codeunit 50101 "Loyalty Mgt" (`SetLoyaltyTier` plus an
`[EventSubscriber]` on `Sales-Post.OnAfterPostSalesDoc`), a test codeunit and a Hello World codeunit.
`.alpackages` held Microsoft Base Application, System Application, Business Foundation, Application
and System 26; `old/` held Base Application 25. Extra probe files were added to `src/` per scenario
(each reproduction below gives its file). No source was changed and cargo was not run. Every finding
was reproduced at least twice; timings are warm-daemon wall-clock times. File:line references are to
`fea0d721`.

Notation: `X` is the `al-explorer` binary, run with the project directory as cwd.

## Findings

### [SYM-EXT-TARGETOBJECT] Table and enum extensions inside a package never attach to their base object
- severity: high
- repro:
  ```al
  // src/PdProbe.Codeunit.al
  codeunit 50105 "Pd Probe"
  {
      procedure Probe(): Code[20]
      var
          Item: Record Item;
      begin
          Item.Get('1000');
          exit(Item."Production BOM No.");
      end;
  }
  ```
  `X composed table Item` -> `Extensions: 0`, 190 fields.
  `X composed enum "Inventory Order Type"` -> `Extensions: 0`.
  `X hover src/PdProbe.Codeunit.al 8 20` -> `No symbol`; `X definition ...` -> `No results`;
  `X --json completions src/PdProbe.Codeunit.al 8 15` has no "Production BOM No." among 228 items.
  `X compile` -> `src/PdProbe.Codeunit.al:8:6: error ALN2404: Record 'item' has no field 'Production BOM No.'`
- expected: Base Application 26 moved manufacturing fields into table extensions in the same app
  (for example tableextension 99000750 "Mfg. Item" extends Item, which declares "Production BOM No.").
  The composed Item has those fields, the code compiles and hover/definition/completion work. Same for
  the 41 enum extensions in Base Application 26 (for example "Mfg. Inventory Order Type" adds
  `Production` to "Inventory Order Type").
- actual: none of the 80 package table extensions or 41 package enum extensions is attached to its
  target. 145 Base Application fields are invisible to hover, definition, completion and composition,
  and native compile reports a false error on correct code. `search "Mfg. Item"` finds the extension
  object, but `object tableextension "Mfg. Item"` has no `extends` value.
- likely cause: `crates/al-symbols/src/model.rs:655`, where `ObjectJson.extends` is read only from
  `ExtendsObjectName`. Real `SymbolReference.json` files name the target in `TargetObject`
  (`TableExtensions`, `PageExtensions`, `EnumExtensionTypes`) and `Target` (`ReportExtensions`).
  A cross-app target is written `#<appId without dashes>#<Name>`
  (e.g. `#63ca2fa44f034f2ba480172fef340d3f#Email Scenario`), so that prefix has to be removed as well.
  `SymbolIndex::by_extends` is empty for package extensions, so
  `composition::get_composed` (`crates/al-symbols/src/composition.rs:54`) finds none.
- status: fixed 5f08e9e5 (TargetObject/Target read, app-id prefix stripped; hover, composition and compile work on the repro)

### [EMIT-TABLE-PROC-ALN2209] Native compile rejects every statement-level call to a table procedure
- severity: high
- repro:
  ```al
  // src/TblMethod.Codeunit.al
  codeunit 50108 "Tbl Method"
  {
      procedure Probe()
      var
          Cust: Record Customer;
      begin
          Cust.SelectCustomer(Cust);
          Cust.HasAddress();
      end;
  }
  ```
  `X compile` -> `Compilation failed`,
  `src/TblMethod.Codeunit.al:7:14: error ALN2209: procedure 'Probe' calls unknown method 'SelectCustomer' on Record 'Cust'`
  and the same for `HasAddress` at 8:14. The same call inside an expression (`exit(Cust.HasAddress());`)
  compiles. The error also fires with `"target": "OnPrem"`.
- expected: both are public procedures of table 18 Customer (`X source Customer --kind table
  --list-procedures` lists them). The build succeeds.
- actual: the native build is refused. Calling a table's own procedures (`SalesHeader.TestStatusOpen()`,
  `Cust.CheckBlockedCustOnDocs(...)`) is everyday AL, so any real extension that does it cannot be
  compiled natively.
- likely cause: `crates/al-emit/src/verification.rs:585-597` (`verify_local_procedure_semantics`).
  A receiver typed `Record` is accepted only when the method is a built-in record method
  (`known_record_method`, :769). The table's declared procedures, and those of its extensions, are
  never consulted.
- status: fixed 25aed1dd (table and extension procedures collected from packages and workspace)

### [FMT-REPEAT-UNDER-IF] `format` breaks the indentation of `if ... then repeat ... until`
- severity: high
- repro: `X format --stdin <` this file (it is already correctly indented):
  ```al
  codeunit 50131 "Fmt Min"
  {
      procedure P(var R: Record Customer)
      begin
          if R.FindSet() then
              repeat
                  R.Mark(true);
                  R.Mark(false);
              until R.Next() = 0;
      end;
  }
  ```
- expected: unchanged output (`format --check` passes).
- actual: the second statement and `until` lose one indentation level:
  ```
              repeat
                  R.Mark(true);
              R.Mark(false);
          until R.Next() = 0;
  ```
  `format --check` reports "would reformat" on correct code, and `format`/`format --all` rewrite it.
  `if Rec.FindSet() then repeat ... until Rec.Next() = 0;` is the canonical AL loop, so almost every
  real codeunit hits this.
- likely cause: `crates/al-syntax/src/formatting/indent.rs:298-321`. `repeat` is a block opener that
  does not drain `single_stmt_depth`, but the first ordinary statement inside the repeat body does
  drain the pending `if ... then` indent. Everything after it, including `until`
  (`indent.rs:237`), moves out one level. The single-statement slot should be consumed by the whole
  `repeat ... until` statement, and drained after the `until` line.
- status: fixed 97fc7380 (block stack carries the pending single-statement indent; fixture test)

### [INSIGHT-NUMERIC-SUBSCRIBER] Subscribers that name their publisher by numeric ID are treated as orphans, and dead-code tells the user to delete them
- severity: high
- repro:
  ```al
  // src/CustSubs.Codeunit.al
  codeunit 50102 "Cust Subs"
  {
      [EventSubscriber(ObjectType::Codeunit, 80, 'OnBeforePostSalesDoc', '', false, false)]
      local procedure ByNumericId(var SalesHeader: Record "Sales Header")
      begin
      end;
  }
  ```
  `X dead-code` -> `DEAD CODE — high confidence (... safe to act on)`:
  `subscriber ByNumericId Cust Subs ... reason publisherRemoved`.
  `X --json --scope all intercept` lists it under `orphanSubscribers` with `targetObject: "80"`.
  `X trace OnBeforePostSalesDoc` shows no subscriber.
  With no workspace file at all, `intercept` reports 109 orphan subscribers, every one a Base
  Application subscriber (for example `Approvals Mgmt..DeleteApprovalEntries -> 1535::OnDeleteRecordInApprovalRequest (missing)`,
  though `X events OnDeleteRecordInApprovalRequest` finds that publisher on codeunit 1535 "Approvals
  Mgmt."). `X trace OnAfterModifyEvent` on Customer misses
  `Workflow Event Handling.RunWorkflowOnCustomerChanged`, which `X subscribers OnAfterModifyEvent`
  prints as `Table::18.OnAfterModifyEvent`.
- expected: `Codeunit, 80` is codeunit "Sales-Post", which publishes `OnBeforePostSalesDoc`. AL accepts
  an integer ID in that argument, and `SymbolReference.json` always writes subscriber targets as IDs
  (`{"Value": "Codeunit"}, {"Value": "12"}, ...`). The subscriber binds to its event, and nothing
  is reported as dead or orphaned.
- actual: a high-confidence "safe to act on" deletion suggestion for live code, 109 false orphans in
  the event map, and traces and subscriber counts that leave out every package subscriber. The
  `subscribers` footer also says symbol packages carry no subscriber metadata, while the list above
  it contains package subscribers.
- likely cause: `crates/al-insight/src/graph.rs:704-715` (`parse_subscriber_target_full`) keeps the
  raw `80`/`18`. Both the edge pass (`graph.rs:578` and `resolve_subscriber_edges`, :307) and
  `find_orphaned_subscribers` (`crates/al-analysis/src/queries/dead_code.rs:405-421`) look the target
  up by name, so an ID never matches. The ID has to be resolved to the object name of the given kind
  first. The synthesised table events (`OnAfterModifyEvent` on `18`) fail the same way.
- status: fixed f44d2696 (0 orphans on Base Application, was 109; dead-code resolves the ID too)

### [XLF-REFRESH-TARGET-LANG] `xlf refresh` overwrites `target-language` with the file-name stem
- severity: high
- repro: `X xlf generate`, then copy `Translations/Bench.g.xlf` to `Translations/Bench.fr-FR.xlf`
  with `target-language="fr-FR"` (the `<App>.<culture>.xlf` naming AL tooling uses). Add a
  `Label` to any codeunit, run `X xlf generate`, then `X xlf refresh Translations/Bench.fr-FR.xlf`.
- expected: the new unit is added and `target-language="fr-FR"` is kept.
- actual: the refreshed file has `target-language="Bench.fr-FR"`, which is not a culture name, so
  Business Central will not apply the translation. The existing translation is preserved. The command
  prints only `Refresh complete: +1 new ...`.
- likely cause: `crates/al-lsp/src/server/daemon/build_dispatch/xliff.rs:148-156`
  (`xlf_target_language`) takes the whole file stem, which gives `fr-FR` only for a file named
  `fr-FR.xlf`. It should read the existing `target-language` attribute, or at least use the last
  dotted segment.
- status: fixed f914c430 (declared target-language kept, else last dotted part of the name)

### [PKGDIFF-FALSE-FIELDREMOVED] `package-diff` reports every Base Application 25->26 `fieldRemoved` as breaking, and flags code that compiles
- severity: medium
- repro: `X --json package-diff --all "old/Microsoft_Base Application_25.0.23364.36035.app" ".alpackages/Microsoft_Base Application_26.0.30643.38226.app"`
  gives `1137 change(s), 1036 breaking`, including 415 `fieldRemoved`. With the `Pd Probe` file from
  [SYM-EXT-TARGETOBJECT], text mode prints `1 used by this workspace` and
  `[breaking] Table Item.Production BOM No.: Field 'Production BOM No.' was removed from 'Item'`.
- expected: a field that moved into a table extension in the same package (145 of the 415) is still
  reachable as `Item."Production BOM No."`, so it is not a removal. A field whose v25 `ObsoleteState`
  was already `Removed` (the other 270) could not be used in v25, so dropping it breaks nothing. Of 146
  `objectRemoved`, 128 were tables already `ObsoleteState = Removed` in v25. The report should list
  these as not breaking, or leave them out.
- actual: all 415 are `isBreaking: true`, and a workspace use of a moved field is reported as
  affected. This is the report an agent reads to decide what to fix before an upgrade.
- likely cause: the diff compares each table's own `Fields` only and ignores the obsolete state of the
  old member (`crates/al-analysis/src/queries/breaking_changes.rs`, reached through
  `crates/al-analysis/src/queries/package_diff.rs`). The moved-field half has the same root as
  [SYM-EXT-TARGETOBJECT]: the new package's table extensions are never composed into their target.
- status: fixed 21656c88 (same-package table/enum extensions folded in, already-Removed members not breaking: 430 breaking of 930, no field removals)

### [CLI-SCOPE-ENTRYPOINTS] `--scope` makes `entrypoints` fail
- severity: medium
- repro: `X --scope workspace entrypoints` (also with `packages` or `all`, and with `--json`).
- expected: the scoped list, as documented for `--scope`.
- actual: `Error: daemon response for 'entrypoints' must be an array`, exit 1.
  `X --scope workspace --limit 100 entrypoints` works.
- likely cause: `crates/al-lsp/src/server/daemon/scope.rs:188-192` wraps a scoped root-array result as
  `{items, scope, outOfScopeCount}` with no `total`. `list_rows`
  (`crates/al-explorer/src/cli/commands/mod.rs:529-533`) unwraps `items` only when `total` is present,
  so the envelope object reaches the array validation (`mod.rs:650`). With `--limit`, projection adds
  `total` and the envelope is unwrapped.
- status: fixed (list_rows unwraps the scope envelope too)

### [ENTRYPOINTS-UNRESOLVED-CALLS] `entrypoints` lists procedures that are called from their own object
- severity: medium
- repro:
  ```al
  // src/CallProbe.Codeunit.al
  codeunit 50106 "Call Probe"
  {
      trigger OnRun()
      begin
          FromTrigger();
      end;

      procedure Caller()
      begin
          FromProcedure();
      end;

      procedure FromTrigger()
      begin
      end;

      procedure FromProcedure()
      begin
      end;
  }
  ```
  `X --scope workspace --limit 50 entrypoints` lists `Call Probe::FromTrigger` and
  `Call Probe::FromProcedure`. The result is the same after `daemon-shutdown` and a fresh start.
- expected: only `OnRun` and `Caller` have no incoming calls.
- actual: every procedure called only from inside its own (low-fanout) object is reported as an
  entry point. A local procedure that nobody calls (`Sql Probe::Unused`) is listed as an entry
  point as well, while `dead-code` correctly calls it unused.
- likely cause: the call graph is filled lazily and in tiers. `populate_workspace_call_edges`
  (`crates/al-insight/src/calls/edges.rs:288-350`) resolves only Tier-1 (high-fanout) files eagerly.
  `dispatch_entrypoints` (`crates/al-lsp/src/server/daemon/insight_dispatch.rs:58-67`) then asks
  `find_entry_points` (`crates/al-insight/src/search.rs:493`) for `callers_of`, which is empty for
  callees of unresolved callers. `resolve_all_workspace_call_edges` (`edges.rs:368`) exists for exactly
  this case and is not called.
- status: fixed d47d57fb (Workspace::complete_workspace_call_edges before entrypoints/trace)

### [TRACE-TREE-BACKEDGE] `trace --tree` shows a fake cycle back to the event and never follows the subscriber's body
- severity: medium
- repro: with the original `LoyaltyMgt.Codeunit.al` (its subscriber calls `SetLoyaltyTier`, which calls
  `Cust.Modify(true)`) and a `Cust Subs` codeunit subscribing to Customer `OnAfterModifyEvent`:
  `X trace --tree OnAfterPostSalesDoc` prints
  ```
  [origin] event: Sales-Post::OnAfterPostSalesDoc
    [event_subscription] subscriber: Loyalty Mgt::OnAfterPostSalesDoc
      [event_subscription] event: Sales-Post::OnAfterPostSalesDoc (cycle)
  ```
- expected: the subscriber's children are what its body calls: `SetLoyaltyTier`, then
  `Customer::OnAfterModifyEvent`, then `Cust Subs::CustOnAfterModify`. No cycle.
- actual: the subscriber's own subscription edge is walked back to the event it listens to and shown
  as a cycle, and the real call chain is missing.
- likely cause: `recurse_subscriber` (`crates/al-insight/src/search.rs:396-445`) iterates every
  `callees_of(sub_id)` edge, including the `EventSubscription` edge that `build_from_insight`
  (`crates/al-insight/src/index.rs:186-199`) records from subscriber to event. It should skip
  `EdgeKind::EventSubscription` there. The missing body edges have the same lazy-resolution cause as
  [ENTRYPOINTS-UNRESOLVED-CALLS].
- status: fixed d47d57fb (subscription edge skipped; the subscriber body now shows)

### [SEARCH-PAGING] `search --limit N` reports `total: N` and `--offset` cannot page past it
- severity: medium
- repro: `X --json --limit 3 search Sales-Post` returns `total: 3, truncated: false`, but
  `X search Sales-Post` finds 8. `X --json --limit 3 --offset 3 search Sales-Post` returns
  `total: 3, returned: 0`. `X --json --limit 5 --offset 5 search Customer` gives `total: 5, returned: 0`.
- expected: `total` counts all matches, `truncated: true`, and `--offset 3` returns rows 4-6, as the
  projection contract in `daemon-methods.md` describes.
- actual: an agent paging a search is told it has everything after the first page.
- likely cause: `crates/al-lsp/src/server/daemon/lsp_dispatch.rs:329-359`. `dispatch_search` reads
  `limit` as the match cap (`workspace.symbols.search(query, limit)`), and projection then pages over
  that truncated list. The dispatcher should search `offset + limit + 1` (or everything) and leave the
  window to `projection.rs`.
- status: fixed e5f351e4 (a paged search collects every match; the window is serialized in full)

### [COMPLETION-UNQUOTED-FIELDS] Field completions after `Rec.` insert names that need quotes without them
- severity: medium
- repro:
  ```al
  codeunit 50120 "Scratch Cu"
  {
      procedure Probe()
      var
          Cust: Record Customer;
      begin
          Cust.
      end;
  }
  ```
  `X --json completions src/Scratch.Codeunit.al 7 14` gives items such as
  `{"label": "Loyalty Tier", "kind": 5, "insertText": null}` and `{"label": "No.", "insertText": null}`.
- expected: `insertText` is `"Loyalty Tier"` and `"No."` (quoted), as object-name completions already
  do (`crates/al-analysis/src/queries/completions.rs:174-180`).
- actual: an editor inserts the label, which gives `Cust.Loyalty Tier` or `Cust.No.`, and neither
  compiles.
- likely cause: `crates/al-analysis/src/resolution/completion.rs:236-245` (package and composed fields)
  and `:439-452` (`workspace_field_items`) set `insert_text: None`. The `needs_quoting` check from
  `completions.rs:467` is not applied to members.
- status: fixed e7c92b24 (quoted insert text unless the quote is already typed)

### [HOVER-LOCAL-CALL] Hover on an unqualified call to a sibling procedure returns nothing
- severity: medium
- repro: `X hover src/LoyaltyMgt.Codeunit.al 15 14`, on `SetLoyaltyTier(Cust, 'GOLD');` inside
  `OnAfterPostSalesDoc`.
- expected: the signature, as the qualified call `LoyaltyMgt.SetLoyaltyTier` in the test codeunit
  gives (`X hover src/LoyaltyTest.Codeunit.al 14 22`). `X definition` at the same position does work.
- actual: `No symbol at src/LoyaltyMgt.Codeunit.al:15:14`.
- likely cause: `crates/al-analysis/src/queries/hover.rs:179-181` compares the name only against the
  procedure that encloses the cursor (`find_procedure_at`). Other procedures of the same object are
  never looked up, and nothing later in `hover_native` resolves a bare call.
- status: fixed cd80727b

### [RENAME-COLLISION] `rename` accepts a name already used in the same scope
- severity: medium
- repro: `X rename --dry-run src/LoyaltyMgt.Codeunit.al 3 60 Cust` renames parameter `Tier` of
  `SetLoyaltyTier(var Cust: Record Customer; Tier: Code[10])` and exits 0. Applied, it produces
  `procedure SetLoyaltyTier(var Cust: Record Customer; Cust: Code[10])` with body
  `Cust."Loyalty Tier" := Cust;`. `X compile` then reports `ALN1106 ... declares parameter 'Cust' more
  than once`, at 1:1.
- expected: the rename is refused with a message naming the existing `Cust`.
- actual: silently broken code. The `WorkspaceEdit` is also returned to editors through LSP rename.
- likely cause: `crates/al-analysis/src/queries/rename.rs:30` (`rename`) never checks the new name
  against the declarations visible at each edited reference.
- status: fixed 1b5a363d (RenameError::Collision for parameters and locals)

### [INTERCEPT-SCOPE] `intercept --scope workspace` drops the workspace's own subscribers and keeps package orphans
- severity: medium
- repro: `X --json --scope workspace intercept` with the original project, where `Loyalty Mgt`
  subscribes to `Sales-Post.OnAfterPostSalesDoc`.
- expected: the event the workspace subscribes to, with its workspace subscriber, and no orphans from
  Base Application.
- actual: `events: []`, `outOfScopeCount: 22970`, `orphanSubscribers`: 109 (all Base Application
  codeunits, see [INSIGHT-NUMERIC-SUBSCRIBER]). The text header still says
  `Event interception map (22970 event(s))` over an empty list.
- likely cause: `crates/al-lsp/src/server/daemon/scope.rs:90-92` classifies an `eventMap` row by
  object name, which is the publisher (a package), so a workspace subscriber never makes its row
  "workspace". The scope filter is not applied to `orphanSubscribers` at all.
- status: fixed 994115d4 (the workspace's own subscribers and the events it raises are kept; package orphans dropped)

### [TABLE-IMPACT-LOCALS] `impact --table` leaves out objects that use the table only through local variables
- severity: medium
- repro: `X --scope workspace impact --table Customer`
- expected: Cust Ext (extends), Loyalty Mgt (parameter and local `Cust: Record Customer`), and
  Loyalty Test (two local `Cust: Record Customer`).
- actual: `Table impact for 'Customer' (803 site(s) across 2 object(s))`. Loyalty Test is missing, and
  Loyalty Mgt shows only the parameter. The site count in the header is the unscoped total, not the
  total of the listed objects.
- likely cause: `crates/al-insight/src/analysis.rs:161-168` reads only `entry.variables` (object-level
  globals) and method parameters. Procedure-local variables are not in `SymbolEntry`. The header uses
  `totalImpacts` computed before the scope filter.
- status: fixed e67e78b2 (procedure-local Record variables counted)

### [STALE-SEARCH-AFTER-DELETE] `search` keeps returning a deleted workspace object
- severity: medium
- repro: with `src/CallProbe.Codeunit.al` present, run any insight command (for example
  `X --scope workspace --limit 50 entrypoints`), delete the file, then run `X search "Call Probe"`
  twice.
- expected: `No results`, which is what `X by-id codeunit 50106` already says
  (`No Codeunit with id 50106`).
- actual: both searches return `Codeunit 50106 Call Probe workspace workspace source`. The entry
  disappears only after another insight command (`entrypoints`, `dead-code`) rebuilds the enriched
  graph.
- likely cause: workspace objects enter `workspace.symbols` through the call-graph enrichment (see the
  comment in `crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:452`), and the per-request file
  refresh does not remove them from the symbol index when a file disappears. `dispatch_search`
  (`lsp_dispatch.rs:342`) reads that index first.
- status: fixed dcf4cf00 (a vanished workspace object is dropped from the index)

### [OBSOLETE-INCOMPLETE] `obsolete` lists only `[Obsolete]` procedures, all as pending with zero callers
- severity: medium
- repro: `X --json obsolete` gives 1553 rows, all `kind: procedure`, all `state: pending`, all
  `callerCount: 0`. Add `Cust.LookupCustomer(Cust);` and `HP := Cust."Home Page";` to a codeunit:
  `X obsolete --used` reports the `LookupCustomer` call, but the `obsolete` row for
  `Customer.LookupCustomer` still has `callerCount: 0`, and the `"Home Page"` use (field
  `ObsoleteState = Pending`, "Field length will be increased to 255.") is reported nowhere.
- expected: Base Application 26 has 125 fields and 62 tables with `ObsoleteState`. They appear with
  their real state, and `callerCount` agrees with `--used`.
- actual: obsolete fields, tables and other objects are invisible to both `obsolete` and
  `obsolete --used`, and the caller counts for package procedures are hard-coded.
- likely cause: `crates/al-analysis/src/queries/obsolescence.rs:103-131` scans only
  `sym.methods[].attributes` for `Obsolete`, and sets `state: ObsoleteState::Pending` (:122) and
  `caller_count: 0` (:127) on every package entry. Field, object and `ObsoleteState` properties are
  not read.
- status: fixed 758715e7 (package objects and fields listed, package callers counted, overload-aware)

### [TEST-AFFECTED-FIELDS] `test-affected` ignores field use
- severity: medium
- repro: `X test-affected src/CustExt.TableExt.al`
- expected: both tests in Loyalty Test, which read and write `Cust."Loyalty Tier"` declared in that
  table extension.
- actual: `No tests touch the given files` (`{"affected": []}`).
  `X test-affected src/LoyaltyMgt.Codeunit.al` finds only `SetTierStoresTheTier`.
- likely cause: affected-test detection walks call edges only. Field and table-extension dependencies
  are not edges in the call graph (`crates/al-insight/src/calls/edges.rs`).
- status: fixed (a changed table or table extension seeds every procedure holding its records; both tests found)

### [GEN-PAGE-OBSOLETE] `generate page` emits a removed field and accepts any page type
- severity: low
- repro: `X generate page --table Customer --name "Loyalty Customers" --page-type List`, and
  `X generate page --table Customer --page-type Bogus`.
- expected: fields with `ObsoleteState = Removed` are left out (Customer "Coupled to CRM" in v26),
  and an unknown page type is an error.
- actual: the page contains `field(coupledToCRM; Rec."Coupled to CRM")`, which does not compile
  against v26, among 103 fields and without tooltips. `Bogus` silently gives `PageType = List`.
- likely cause: `crates/al-analysis/src/generators.rs:54` (`generate_page`) copies every field.
  `crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:485-487` uses `unwrap_or_default()` on
  the page-type parse.
- status: fixed 1ad4356b (Removed fields left out, unknown page type is an error)

### [COMPLETIONS-PROJECTION] `--limit`/`--fields` do nothing on `completions`, `hints`, `symbols` and `folding`, and their text mode prints raw JSON
- severity: low
- repro: `X --json --limit 3 completions src/LoyaltyMgt.Codeunit.al 5 14` returns 319 items.
  `X --fields label completions ...` returns full rows. `X hints src/LoyaltyMgt.Codeunit.al`
  (text mode) prints `{"position": ..., "label": {"String": "Cust:"}, "padding_left": null}`.
  `completions` and `signature` print `Result:` followed by JSON.
- expected: the global flags apply to every list-returning command, as `--help` says, or are refused.
  Text mode is human-readable, and JSON uses LSP field names (`paddingLeft`, a string `label`).
- actual: the flags are accepted and silently ignored. `hints` leaks Rust enum serialisation, and
  `folding` uses `start_line`, where the rest of the JSON is camelCase. Positions are 0-based here
  but 1-based on input.
- likely cause: these methods are missing from `LIST_TARGETS`
  (`crates/al-lsp/src/server/daemon/projection.rs:33-68`). The CLI printers for them serialise the
  internal structs directly.
- status: open

### [IMPACT-TYPES] `impact` labels calls and writes as `read` and repeats rows
- severity: low
- repro: `X impact SetLoyaltyTier` and `X impact 'Customer."Loyalty Tier"'`.
- expected: calls reported as `call`, and the assignment `Cust."Loyalty Tier" := Tier` distinguishable
  from a read. One row per consumer, or rows that say what differs.
- actual: every consumer is `read`. `ImpactType::Call` exists but is never produced
  (`crates/al-analysis/src/queries/impact.rs:18-35`, `:620-627`).
  `X --scope workspace impact Customer` lists `Cust Subs read`, `Loyalty Mgt read` and
  `Cust Ext read/extends` twice each, with no procedure or line to tell the rows apart. With a
  qualified `"Loyalty Mgt".SetLoyaltyTier`, the internal call in Loyalty Mgt collapses into its
  `declares` row.
- status: open

### [TEXT-LIMIT-COUNTS] Text output under `--limit` prints the page size as the total
- severity: low
- repro: `X --limit 3 entrypoints` prints `Entry points (3 found)` (JSON: `total: 27073`).
  `X --limit 2 obsolete` prints `2 obsolete symbol(s)` (total 1553). `X --limit 1 dead-code`
  prints `1 findings`.
- expected: `3 of 27073 (truncated)` or similar.
- actual: a human reader cannot tell the list was cut.
- status: fixed e819b42f (the footer names the page and the total)

### [SORT-MEMBERS-DRYRUN] `sort-members --dry-run` says "Members sorted."
- severity: low
- repro: `X sort-members --dry-run src/HelloWorld.Codeunit.al` (with a `var` section after the
  procedures). The file is unchanged, as it should be, but the output is `Members sorted.` and
  no preview.
- expected: "would reorder members" plus the preview (JSON already has `sorted`/`dryRun`).
- likely cause: `crates/al-explorer/src/cli/commands/lsp/refactor.rs:77-80` ignores `dry_run`.
- status: fixed 7348280e (the dry run lists the moves)

### [SOURCE-CANDIDATES] A wrong `source --procedure` lists the first eight declarations only
- severity: low
- repro: `X source "Sales-Post" --procedure OnAfterPostSalesDocc`
- expected: the near match (`OnAfterPostSalesDoc`), or the full list with a count.
- actual: `It declares: RunWithCheck, CopyToTempLines, ... PostItemLine`, which is eight names of
  several hundred, with no "and N more", and the one-letter typo is not suggested.
- likely cause: `crates/al-analysis/src/queries/source.rs:525-529` (`.take(8)` in declaration order).
- status: fixed 8ac320c1 (closest names first, and how many are declared)

### [LINT-RANGES] Lint diagnostics span the whole line and lowercase variable names
- severity: low
- repro: `X --json lint` on a codeunit with `if Cust.FindFirst() then;` inside a loop:
  `"column": 1, "endColumn": 42` and message `Record cust is read without a preceding SetLoadFields ...`.
- expected: the range of the call, and the variable as written (`Cust`).
- status: open

### [SIGNATURE-IN-STRING] No signature help while the cursor is inside a string argument
- severity: low
- repro: `X signature src/LoyaltyTest.Codeunit.al 14 45`, inside `'GOLD'` of
  `LoyaltyMgt.SetLoyaltyTier(Cust, 'GOLD');`.
- expected: `activeParameter: 1`, as at columns 42-43 and 49.
- actual: `No results`. While typing a text argument, the signature popup disappears.
- status: fixed (the call context is read up to the unclosed quote; `activeParameter: 1` at 14:45)

### [GRAPH-EXPORT-CAP] `graph` can never succeed on a project with Base Application
- severity: low
- repro: `X graph` or `X --scope workspace graph`.
- actual: `Graph too large to export in one response: 117018 nodes+edges exceeds cap of 50000`.
  `--scope` is silently ignored, so there is no way to export the workspace part.
- status: open

### [PERF-PACKAGE-DIFF] `package-diff` takes about 6 s every run
- severity: low
- repro: run `X package-diff old/…25….app .alpackages/…Base Application_26….app` twice. It takes
  5.95 s, then 5.89 s. `--timeout-ms 100` fails after 2 s with
  `Daemon did not respond within 0s`, a duration rounded down to zero.
- likely cause: `crates/al-lsp/src/server/daemon/build_dispatch/mod.rs:97-102` re-reads and re-parses
  both `.app` files on every call. There is no cache keyed on path and mtime.
- related: `X breaking --baseline-app old/…Base Application_25….app` takes 15.5 s and reports 7569
  breaking changes. The baseline is a different app (its appId differs from `app.json`), and nothing
  warns about it.
- status: partly fixed: the timeout message 3b20538c, the different-app warning e57b81f9; no cache (it would pin two Base Applications in memory)

### [DOCS-FLAG-NAMES] Help and docs name commands and flag combinations that do not exist
- severity: low
- repro: the global `--scope` help says "Applies to impact, table-impact, entrypoints and event-map",
  but `X table-impact` and `X event-map` are `unrecognized subcommand`. The CLI names are
  `impact --table` and `intercept`. `cli-commands.md` lists `suggest-event --event <x>`, but
  `X suggest-event --event OnAfterPostSalesDoc` requires `--object`. `X suggest-event --object Sales-Post
  --procedure PostItemLine` prints `Some call paths are still being analyzed` on every run, including
  warm ones.
- status: partly fixed e623104d (help names `impact --table` and `intercept`, docs name `--object`); the "still analyzed" message is open

### [FIELDS-UNKNOWN] `--fields` with unknown names returns empty rows silently
- severity: low
- repro: `X --fields bogus --limit 2 packages` returns `{"items": [{}, {}], ...}`, and `--fields
  kind,name` on `packages` silently drops `kind`.
- expected: an error, or a warning naming the fields the rows actually have.
- status: fixed fca54c75 (unknown names are an error that lists the fields rows have)

### [XLF-NOTES] Generated XLIFF drops the label's `Comment` and uses bare notes
- severity: low
- repro: a `GreetingMsg: Label 'Hello from Bench!', Comment = 'Shown on run';`, then `X xlf generate`.
- expected: the comment as `<note from="Developer" ...>Shown on run</note>`, and the object path as
  `<note from="Xliff Generator" ...>`, which is what translation tools key on. There is no `<target>`
  in the `.g.xlf`.
- actual: one bare `<note>` with the path, no developer note, and `<target state="new"/>` in the
  generated file.
- status: fixed 5f5b2e79 (Developer and Xliff Generator notes, no `<target>` in the .g.xlf)

### [MISC-SCAFFOLD] Small scaffold issues
- severity: low
- repro and actual:
  - `X new p --runtime 99.0` is accepted and writes `"application": "110.0.0.0"`.
  - `new` writes `"codeAnalyzers"` into `app.json`, which is an editor setting (`al.codeAnalyzers`)
    and not an app.json property.
  - `X permissions` puts the test codeunit ("Loyalty Test") into the assignable
    `Generated Permissions` set.
  - `X test-classify` gives as reasons "reachable procedure uses enum 'Database' ... 'ObjectType'".
    These come from `[EventSubscriber]` attribute arguments, not executed code, and the Customer reason
    is repeated.
  - `X test-coverage` credits the `OnAfterModifyEvent` subscriber only to the test that calls
    `Modify(true)`. Business Central raises the global table events whatever the `RunTrigger` value,
    so `TierCanBeCleared` (`Insert()`/`Modify()`) reaches it too. That behaviour is per the BC
    database-trigger-event documentation; confirm it before changing the rule.
- status: partly fixed: runtime and al.codeAnalyzers 04d8460d, test codeunits c7c20856; test-classify reasons and test-coverage are open

## Commands exercised that behaved correctly

- `version`, `doctor`/`setup` (a clear ALTool/.NET report with an install hint in `setup`), `packages`
  (text and JSON), `deps`, `deps-graph` (json and dot), `diag`, `insight-stats`, `rules`, `builtins`
  and `error-codes` (a clear "requires ALTool").
- `search` ranking (workspace objects rank first for "Cust"), `object`, `by-id`, `composed table
  Customer` (workspace extension attached), `source` (outline, workspace, `--procedure`,
  `--list-procedures`), `location`, `events`, `event-source` (resolves `Sales-Post`
  `OnAfterPostSalesDoc` with its full signature).
- `hover` on parameters, locals, fields (base and extension), object names, qualified procedure calls.
  `definition` on all of those, including the unqualified local call. `references` for the
  extension field (7, from the declaration and from a use), the procedure (3) and the Customer table (5).
  `signature` active-parameter tracking. `symbols`, `folding`, `tokens`, `parse`.
- `rename` of a procedure (3 edits in 2 files) and of a quoted extension field (7 edits in 3 files, the
  `Caption` string left alone), with quoting added for a name that needs it. `rename --dry-run`.
- `sql-scan` (FindSet without filters, Get and FindFirst in a loop) and `lint`/`lint --all` findings.
  `obsolete --used` for procedure calls, including overload judgement. `free-ids` (summary, `--kind`,
  `--count`, `--object` for a tableextension with the base table counted). `native-check`,
  `duplicates`, `metrics --all`, `arch-lint`, `audit-data`, `permission-audit`.
- `breaking`/`upgrade` without a baseline ("not evaluated", exit 1) and against the project's own
  earlier `.app` (renamed field and removed procedure reported, with migration hints).
- `package-diff` input validation: a missing file names the path, and a path outside the project is
  refused with the project root named.
- `dead-code` on a genuinely unused local procedure. `impact --table` package scope and projection.
  `trace` (non-tree) for a name-qualified subscriber. `subscribers`.
- `format --check`/`--all` on files without `repeat` under `if`, and `format --stdin`.
- `codeActions` (daemon): "Make procedure local" is not offered for a procedure called from another
  object.
- `tests`, `test-classify`, `test-run`/`test-run-all` (the missing-credentials error explains the
  routing and names `test-classify`), `test-affected` for a codeunit change.
- `generate test --subject`, `generate page` for an unknown table (clear error), `new` (refuses an
  existing project, lists the built-in templates for an unknown `--template`), `init-debug` (does not
  overwrite), `organize-files --dry-run`, `add-application-area`/`add-data-classification --dry-run`,
  `sort-members` (applied order correct), `permissions`.
- `xlf generate`, `xlf untranslated`, `xlf suggest`. `xlf refresh` keeps existing translations and adds
  new units; only the language attribute is wrong.
- Freshness: an edited file (new procedure, new label) is seen immediately by `symbols`,
  `references`, `impact`, `entrypoints` and `xlf generate`. A deleted file drops out of `by-id`,
  `object`, `free-ids`, `permissions` and `entrypoints` at once.
- `--timeout-ms` is honoured. `--compact`, and `--limit`/`--offset`/`--fields` on `packages`,
  `entrypoints`, `impact` and `impact --table`, with `total`/`truncated` correct.

## Review complete
