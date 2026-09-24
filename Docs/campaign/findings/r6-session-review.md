# R6 review: 2026-09-24 session commits

Scope: `git diff 21e2220b..HEAD` (fixes to test-affected, signature help,
CLI completions/hints/symbols/folding, impact classification and dedupe, lint
ranges, graph --scope, suggest-event gaps, test-classify, RunTrigger table
events, file-index re-indexing, breaking baseline warning, projection
--fields, xlf notes, scaffold analyzers, obsolete package rows, table-impact
locals, search removal, intercept scope). The two trailing refactor commits
(test-module moves) were checked for test counts only. Read-only review;
findings A-1 and IMP-1 were confirmed with a scratch probe outside the repo.

Checked and not reported: file-index `replace_file_entries` (the
single-writer window is closed; concurrent writers of one path could leak a
name before and after the change alike), RunTrigger semantics (BC raises
OnBefore/OnAfter{Insert,Modify,Delete}Event whatever RunTrigger is, so this is
correct), lint `span_range` columns (bytes, converted to UTF-16 by
`ts_range_to_lsp`; `mask_line` keeps byte offsets), the graph slice cost, and
the camelCase rename of hints/folding/symbols (no other consumer in the repo
reads the old snake_case names).

## Findings

### [R6-TA-1] test-affected and table-impact skip temporary records, arrays and triggers
- where: crates/al-insight/src/analysis.rs:328 (`workspace_procedures_using_table`), :368, :561 (`is_record_of`); also :224 (`add_workspace_local_record_variables`)
- severity: medium
- scenario: `is_record_of("Record Customer temporary", "Customer")` is false: the type text keeps the `temporary` keyword, so the name compared is `Customer temporary`. `array[5] of Record Customer` also fails, on its `Array` prefix. The walk also collects `procedure_declaration` nodes only, so an `OnRun`, page action or table trigger that holds a `Record Customer` is never a seed. A scratch probe over a codeunit with `UsesTemp` (a `TempCust: Record Customer temporary` local), `UsesPlain` and `trigger OnRun` (a `C: Record Customer` local) returned only `UsesPlain`. Changing a Customer table extension therefore leaves out tests that reach the field through a temporary buffer or through `Codeunit.Run`. The code comments call exactly this kind of false negative the one to avoid. `impact --table` misses the same temporary locals.
- fix: in `is_record_of`, strip a trailing `temporary` and accept `array[..] of Record X`. Collect `trigger_declaration` (and `event_procedure_declaration`) as procedures too, keyed as the call graph keys triggers.
- status: fixed (`is_record_of` reads `Record X temporary` and `array[..] of Record X`; triggers and subscribers are seeds, looked up under their own node kinds; the test-affected test covers a temporary buffer and an `OnRun` reached through `Codeunit.Run`)

### [R6-TA-2] test-affected re-walks every workspace tree once per changed table
- where: crates/al-analysis/src/queries/tests.rs:331-340, crates/al-insight/src/analysis.rs:328
- severity: low
- scenario: `changed_tables` gives one table per changed table or table extension, and each one calls `workspace_procedures_using_table`. That call sorts every path in the file index and walks every node of every cached tree. For a diff touching 30 table extensions in a workspace of 3,000 files, that is 30 full walks of the workspace ASTs on each `tests.affected` call. `impact --table` pays one more walk per request through `add_workspace_local_record_variables`.
- fix: walk once and match each variable's type against the set of changed tables, e.g. a `workspace_procedures_using_tables(&files, &HashSet<String>)`.
- status: fixed (`workspace_procedures_using_tables` walks the workspace once for every changed table)

### [R6-IMP-1] impact calls `Rec.Validate(Field, Value)` a low-confidence read
- where: crates/al-analysis/src/queries/impact.rs:692 (`use_of`), :617-639
- severity: medium
- scenario: in `Cust.Validate("Loyalty Tier", Tier);` the field name is an argument, so `receiver_before` finds no receiver and the text after it is `, Tier)`. The row comes out `type: read, confidence: low, note: "name match only"`. Probe output: `{"n":"Loyalty Mgt","type":"read","confidence":"low",...}`. In Business Central, Validate is the usual way to write a field, and the commit's point was to tell writes from reads. `SetRange`/`SetFilter`/`TestField` uses are also `read`, even though the enum has `Filter`.
- fix: when the reference is the first argument of a `.Validate(`/`.SetRange(`/`.SetFilter(`/`.TestField(`/`.CalcFields(` member call, bind it through that call's receiver, and classify Validate as `Write` and SetRange/SetFilter as `Filter`.
- status: open

### [R6-IMP-2] the impact tests would pass without the declaration exclusions
- where: crates/al-analysis/src/queries/impact.rs:990-1000 (`impact_says_whether_a_consumer_calls_writes_or_reads`)
- severity: low
- scenario: `Loyalty Mgt` is expected as `[Call, Declares]`. `Promote` calls `SetTier`, so the test passes even if `use_of` counted the `procedure SetTier(` declaration as a `Call` (the name is followed by `(`). For the field, an unexcluded `field(50100; "Loyalty Tier"; ...)` would add an object-level `read` row to `Cust Ext`, and `dedupe` drops it because the object already has a `declares` row. Neither `is_field_declaration_name` nor the `name`-field check is exercised.
- fix: add a case where the declaring object does not use its own member (expect `[Declares]` only), and unit-test `use_of`/`is_field_declaration_name` directly.
- status: open

### [R6-IMP-3] a member query no longer lists any package page or extension of the table
- where: crates/al-analysis/src/queries/impact.rs:117
- severity: low
- scenario: `impact --scope all 'Customer."Credit Limit"'` used to list the package pages bound to Customer (`display`) and the package extensions of Customer (`extends`). These now sit behind `member_part.is_none()`. Package code has no source scan, so the member query reports nothing for packages except event subscribers, and nothing signals that it did not look. The old rows over-approximated. A package field or a dependent app is now reported as having no page consumers.
- fix: for package entries, keep the SourceTable/extends rows under a member query as `confidence: low` with a note ("uses the table; the member use is not checked without source"). Leave the workspace rows as they are.
- status: open

### [R6-IMP-4] the new `write` impact type is not in the impact skill
- where: plugin/skills/bc-impact-check/SKILL.md:40-44
- severity: low
- scenario: the skill lists `declares`, `display`, `read`, `call`, `filter`, `extends` and `subscribe`. `impact` now also returns `"type":"write"`, and `call` for the declaring object's own internal uses beside its `declares` row. An agent following the skill has no meaning for `write`. `graph` in Docs/reference/cli-commands.md still lists `--format` only, not `--scope`.
- fix: document `write` (field assigned) and the declaring-object internal-use row, and add `--scope` to the `graph` row.
- status: open

### [R6-PROJ-1] `--fields` now refuses an optional field that no row happens to carry
- where: crates/al-lsp/src/server/daemon/projection.rs:160 (`check_fields`)
- severity: medium
- scenario: many row types drop optional keys when they are empty (`skip_serializing_if`). Impact rows omit `proc`, `field`, `package` and `note`. SymbolEntry omits `methods`, `fields`, `extends`, `implements`, `properties` and others. `--fields n,type,proc impact X` fails with INVALID_PARAMS when no row has a procedure. `--scope workspace --fields n,package impact X` always fails, because workspace impact rows never carry `package`. `--fields kind,name,extends search Foo` fails when no hit extends anything. Through MCP (default scope workspace, default limit) an agent that names a documented field gets an error instead of rows. Before this commit those calls returned rows without the key.
- fix: validate against the row type's declared field names, not the keys present in this result. Failing that, refuse only when the name is absent from every row and is not a known optional key.
- status: open

### [R6-PROJ-2] MCP `al_call` for completions, documentSymbols, foldingRanges and inlayHints now returns an envelope capped at 50
- where: crates/al-lsp/src/server/daemon/projection.rs:58-61, crates/al-lsp/src/server/mcp.rs:1301-1307 (`apply_agent_defaults`, now mcp/mod.rs in the working tree)
- severity: low
- scenario: `apply_agent_defaults` adds `limit: 50` to every method that has a list target. Adding the four LSP methods to `LIST_TARGETS` changes their MCP result from a bare array to `{items,total,returned,offset,truncated}`, and cuts completions to 50 items. inlayHints still returns bare `null` when there are none, so its shape now varies. For `documentSymbols` the root array is the file's objects (usually one), so `--limit` and `--offset` do not page procedures in either the CLI or MCP, although the commit says symbols are now paged.
- fix: note the envelope in the MCP docs, or leave these methods out of the MCP default limit. For documentSymbols, page the flattened symbol list or leave it out of `LIST_TARGETS`.
- status: open

### [R6-SIG-1] signature help still reads the receiver from the untruncated prefix
- where: crates/al-analysis/src/queries/signature.rs:191 and :268 (`has_receiver`), :415 (`resolve_receiver_signature`)
- severity: low
- scenario: `find_call_context` now cuts the prefix at an unclosed quote. `has_receiver(prefix)` and `resolve_receiver_signature` still `rfind('(')` on the whole line. For a local `Notify(Msg: Text)` typed as `Notify('See p. 3 (`, `has_receiver` finds the `(` inside the string and takes `3` as a receiver. The same-file and implicit-Rec lookups are skipped, and help is found only if the symbol index or builtins happen to have `Notify`.
- fix: pass the truncated prefix (return it, or the paren offset, from `find_call_context`) to `has_receiver` and `resolve_receiver_signature`.
- status: open

### [R6-GRAPH-1] `graph --scope workspace` treats a file's second object as package code
- where: crates/al-lsp/src/server/daemon/scope.rs:93 (`workspace_object_names`), used by crates/al-lsp/src/server/daemon/insight_dispatch.rs:181 (`scoped_graph_nodes`)
- severity: low
- scenario: the name set is built from `file_index.object_info`, which holds only the first object declared in each file. In a file declaring `table 50100 X` and then `page 50100 "X List"`, the page and its procedures fall out of the workspace slice unless they are one edge from X. Under `--scope packages` they are listed as package nodes and counted in `outOfScopeCount`. The helper was already used by entrypoints/eventMap scoping, and graph export now depends on it too.
- fix: build the set from `object_infos` (every object in the file).
- status: open

### [R6-OBS-1] package obsolete procedures get a caller count by bare name
- where: crates/al-analysis/src/queries/obsolescence.rs:150
- severity: medium
- scenario: `call_site_counts` counts every call site by name alone, member calls included (`al_syntax::collect_call_sites`). Base Application often obsoletes one overload and keeps another of the same name, and some obsolete procedures have common names (`Initialize`, `Code`, `GetDefaultDimID`). A workspace that calls only the replacement overload, or a same-named procedure on another object, now shows `callerCount: N > 0` on the obsolete package procedure. That reads as "the workspace uses this obsolete API". Before this commit package rows reported 0.
- fix: count package callers from resolved call-graph edges to that object's method (or `obsoleteUsages`, which resolves the receiver). Until then, report package `callerCount` as unknown (omit it) rather than a by-name total.
- status: open

### [R6-XLF-1] refresh never brings a developer `Comment` into an existing translation unit
- where: crates/al-analysis/src/xliff/refresh.rs:36-49, crates/al-analysis/src/xliff/format.rs:168
- severity: medium
- scenario: `refresh_xliff` rebuilds an existing unit from `..lang_unit.clone()`, and a unit whose source is unchanged is pushed as `lang_unit.clone()`. Both keep the language file's `developer_note` and `note`. Language files made before this change have no Developer note, and the regenerated `.g.xlf` now carries the `Comment`. Refresh drops it for every unit whose source text did not change, so translators never see the comment the commit set out to preserve. An edited `Comment` never propagates either. Separately, a `<note from="Developer">` that spans lines goes through the multi-line path into `current_note`, so the following Xliff Generator note overwrites it.
- fix: in refresh, take `note` and `developer_note` from the generated unit and keep only target and state from the language unit. In the multi-line path, remember which note (`from=`) is being accumulated.
- status: open

### [R6-NEW-1] the analyzer settings `new` writes are ignored by the `.gitignore` it writes
- where: crates/al-project/src/scaffold.rs:233-238 and :855
- severity: low
- scenario: the AppSource template's analyzers (`AppSourceCop`, `PerTenantExtensionCop`, `UICop`) moved from app.json to `.vscode/settings.json`, and `generate_gitignore` lists `.vscode/settings.json`. The analyzer choice therefore never reaches the repository. A teammate's clone or a CI checkout builds without AppSourceCop and UICop, while the author's machine runs them.
- fix: stop ignoring `.vscode/settings.json` in the generated `.gitignore` (the file now holds project policy), or write the analyzers to a committed settings file and say so in the `new` output.
- status: open

### [R6-SE-1] `depthCut` fires for leaves and for nodes later reached at a shallower depth
- where: crates/al-analysis/src/queries/suggest_event.rs:526-530
- severity: low
- scenario: `trace_from_node` sets `depth_cut` as soon as any node is reached at depth 10, before checking whether that node has callees. That node is dropped even if it is itself an event. In a diamond or cycle, a node first reached at depth 10 and then again at depth 2 is fully traced, but `depthCut` stays true. The CLI then prints "the trace stops 10 calls deep, so events further down are not listed. Start from a deeper --procedure" when nothing is missing. The commit's aim was to replace a misleading "partial" with an accurate reason.
- fix: record the cut nodes and clear them when the same node is later visited at a shallower depth. Set `depthCut` only when a cut node that was never fully visited has callees, or is an event not otherwise recorded.
- status: open

## Review complete
