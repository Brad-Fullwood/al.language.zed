# Docs review: user docs against the code (workstream H)

Branch `campaign/docs-review`, based on `campaign/2026-09-21` at 8dff1393.

Scope: `README.md`, `ROADMAP.md`, `Docs/` (except `Docs/campaign/` and
`Docs/features/project-trust.md`), `plugin/ROADMAP.md`, `plugin/skills/*/SKILL.md`,
`tree-sitter-al/README.md`. `plugin/README.md` does not exist; the root README's "Claude Code
Plugin" section covers the plugin.

Checked against: `al-explorer --help` and every subcommand's help from the release binaries built
at fa4fcf8f (no CLI argument changed between fa4fcf8f and 8dff1393), the `dispatch_table!` registry
in `crates/al-lsp/src/server/daemon/mod.rs` (95 methods), the MCP tool list in
`crates/al-lsp/src/server/mcp/mod.rs` (19 tools), `SUPPORTED_COMMANDS` in
`crates/al-lsp/src/server/lsp/mod.rs`, the settings reader in `crates/al-project/src/config.rs`,
`schemas/settings.json`, `src/lib.rs`, `plugin/.claude-plugin/`, `Makefile` targets and
`.github/workflows/*.yml`.

## Files

- `Docs/features/native-test-runtime.md`: changed: `Evaluate` and `CalcDate` run locally (removed
  from the live-BC list), added the other globals, instance methods, enum methods, arrays, text
  indexing, call chains, CalcSums, Ascending, ModifyAll, `Assert.ExpectedError`, per-variable
  temporary stores, the 512/2560/2560 depth caps on a 64 MiB stack, and the dynamic Cobertura
  `line-coverage="unavailable"` attribute.
- `README.md`: changed: extension binary resolution (latest-release lookup before a cached
  download, newest download only when the lookup fails), MCP resolution, CodeLens commands go
  through the execute-command dispatcher, the plugin's `SessionEnd` hook, `package-diff` in the CLI
  list, dependency-upgrade and ID-allocation workflows, `al-explorer` as a product version,
  interpreter builtins.
- `ROADMAP.md`: changed: the language package ships 55 tasks (the roadmap said installed tasks were
  absent on purpose), release sidecars come from `PATH` or the downloaded archive.
- `Docs/gaps-and-future-work.md`: changed: task row as above, the 23/9 fixture counts dated to
  `a4e7d5fe`. Fixture directories hold 25 valid and 10 invalid files, all 25 valid parse with no
  error node and all 10 invalid produce one (`tree-sitter parse --stat`).
- `Docs/architecture.md`: changed: removed the dashed `al-lsp -> tree-sitter-al` edge and its note
  (only `al-syntax` depends on the grammar crate). `architecture_doc` passes before and after
  (compiled standalone with `rustc --test`).
- `Docs/reference/cli-commands.md`: changed: `trust --yes --root`, `composed --name`,
  `suggest-event --kind`, `debug breakpoint <file> <line>`, `xlf refresh [--generated]`,
  `snapshot start --description`, `--scope` covers `intercept` and `graph`, exit 75 is `doctor`
  only, `subscribers` reads package source.
- `Docs/reference/daemon-methods.md`: changed: `ping` returns `"pong"`, `handshake` `proof`,
  `nativeCheck` and `diag` placement, `stepType` accepts `into`, the methods that wait for the
  source index, only `format`, `fix`, `sortMembers` refuse `text`.
- `Docs/reference/mcp-tools.md`: changed: `al_downloadsymbols` `source`/`config`,
  `al_symbolsearch` `summary`, `al_trace_event` depth max 50.
- `Docs/reference/lsp-commands.md`: changed: `workspace/didChangeWatchedFiles` and the `**/*.al`
  watcher registration.
- `Docs/reference/settings.md`: changed: added the four `al.formatting.*` keys (read by
  `config.rs`, in the schema and the example, missing here), `.alformat.json` for
  `al-explorer format`, removed `BC_TENANT` (no code reads it), added `AL_REQUEST_TIMEOUT_MS`,
  `AL_DAEMON_IDLE_SECS`, `AL_ALLOW_MISMATCHED_DAEMON`, `AL_ALLOW_INSECURE_BC_HTTP`,
  `AL_EXPLORER_PATH`.
- `Docs/features/cli-and-tui.md`: changed: the tasks ship (the page said the CLI was not exposed as
  tasks), missing commands added to the category list, exit 75, dead-code exits on any finding,
  package subscribers.
- `Docs/features/ai-mcp.md`: changed: MCP binary resolution, tool parameters from the schemas,
  MCP defaults (`limit` 50, `scope` workspace, `signatures`).
- `Docs/features/daemon-protocol.md`: changed: `-32002`, `build_dispatch/` file list, `nativeCheck`
  placement, the `dispatch_table!` capability list, the offloaded insight walks.
- `Docs/features/language-server.md`: changed: LSP formatting reads `al.formatting.*`,
  `al-explorer format` reads `.alformat.json`, the `**/*.al` watcher, module paths.
- `Docs/features/analysis-and-insight.md`: changed: record operations raise the table's
  OnBefore/OnAfter events whatever `RunTrigger` is, obsolete usage, package diff, free IDs and
  native check in the catalog.
- `Docs/features/symbol-and-package-engine.md`: changed: module paths, `location` and
  `--list-procedures`, removed `BC_TENANT`.
- `Docs/features/scaffolding-and-codegen.md`: changed: no LSP command scaffolds (the tasks do),
  `generate` takes the first free ID when `--id` is omitted.
- `Docs/features/xliff-translation.md`, `Docs/features/code-actions.md`: changed: module paths,
  `suggest_translations`, `refresh` finds the `.g.xlf`, task names.
- `Docs/features/language-assets.md`: changed: the build command is `al.compile`, 78 snippets.
- `Docs/features/parsing-and-syntax.md`: changed: module directories, the source of the Record
  method catalog.
- `Docs/features/semantic-bridge.md`: changed: the bridge has no compile method, analyzer lookup
  goes through `discover_custom_analyzer` and searches `al.assemblyProbingPaths` for both
  backends (the page said probing paths apply to `alc` only).
- `Docs/features/debugging-dap.md`: changed: module layout, 30 s step/continue wait, 4096-message
  event channel.
- `Docs/features/native-app-emitter.md`: changed: ALN2201 to ALN2501 codes listed, ALN1004 to
  ALN1006 removed, module owners.
- `Docs/current-limitations.md`, `Docs/testing-guide.md`, `Docs/benchmarks.md`, `Docs/index.md`:
  changed: embedded package source in the call graph, the stale-binary guard, nightly property
  tests, release stages 6 and 10, `impact --table`, links to project trust and the plugin roadmap.
- `plugin/skills/*/SKILL.md`, `plugin/agents/*.md`, `plugin/ROADMAP.md`: changed: every command
  and `jq` filter run against the release binaries and matched to the JSON they print
  (`--fields` on search, `free-ids` keys, `breaking` and `dead-code` row shapes, `test-run` takes
  the codeunit ID), and `object`/`by-id` fill workspace members only with `--wait-for-members`.
- `tree-sitter-al/README.md` (grammar repository, branch `campaign/docs-review`): changed: names
  `AL_EXTENSION_PATH`, one sentence per idea.

## Claims not verified, left as written

- Benchmark figures in `README.md`, `Docs/benchmarks.md` and `benchmarks/`: dated measurements
  with committed raw results. Not re-measured.
- The evidence in `Docs/gaps-and-future-work.md` (runs at `a4e7d5fe`, CI run links, corpus
  counts): a dated record of those runs. Not re-run. The 25/10 fixture counts were checked.
- Live Business Central behaviour in `Docs/features/debugging-dap.md` and the live test runner:
  no tenant was available.
- The Microsoft column of `Docs/microsoft-comparison.md`: not checked against Microsoft's current
  extension.
- `Docs/features/project-trust.md`: written by the security workstream and out of this review's
  scope. After the merge its ten credential methods, the `ADVISORY_KEYS`, `one_line`,
  `escapes_untrusted_project`, `server_with_scheme` and `discover_custom_analyzer` names, the
  six-`stat` fingerprint and the untrusted message were checked against the code and agree. Its
  wording was not edited.
- `plugin/TESTING.md`: a record of agent runs, left as recorded apart from one sentence split at
  a semicolon.

## Code comments and help text that disagree with the code (not changed, code is out of scope)

- `crates/al-runtime/src/interpreter/dispatch/mod.rs:13`: module doc says recursion depth is capped
  at 100. `MAX_RECURSION_DEPTH` is 512.
- `al-explorer free-ids --help` examples say `al free-ids` where the binary is `al-explorer`.

## Review complete

Checked: every file under Files against the code, the CLI help of the release binaries, the
dispatch table, the MCP tool list, the settings reader and the workflows, as recorded above.
After `campaign/2026-09-21` was merged (the security round 4 and blog fixes), the docs the merge
touched were read again against the new code: `Docs/features/project-trust.md`,
`Docs/features/daemon-protocol.md`, `Docs/features/xliff-translation.md`,
`Docs/current-limitations.md`, `Docs/reference/cli-commands.md`,
`Docs/reference/daemon-methods.md`, `README.md` and `plugin/skills/bc-symbol-lookup/SKILL.md`.
The merge was done before the wording pass so that pass also covers the merged text.
`Docs/features/native-test-runtime.md` lists `Evaluate` and `CalcDate` as local builtins, and both
are in `supports_global_builtin`.

Changed after the merge, for drift against the code:

- `object` and `byId` answer at once and fill workspace members only with `waitForMembers: true`
  (97d7d98d). The daemon-methods waiter list, `bc-symbol-lookup` and `plugin/ROADMAP.md` said
  they always wait.
- The request deadline also keeps waiting while the call graph builds, up to 600 s in all
  (a757fb13). The `--timeout-ms` row named the source index only.
- `trust --yes` needs `--digest` (ef0d4e27). The CLI reference showed `--yes --root` only.
- `al.assemblyProbingPaths` is also where the in-process bridge looks for a named analyzer DLL.
  `semantic-bridge.md` and `settings.md` said it applies to `alc` only.

Wording: every file under Files had an unsloppify pass. Em dashes are gone from `Docs/` (outside
`Docs/campaign/`), `README.md`, `ROADMAP.md` and `plugin/`, and so are semicolons in prose, except
one sentence in `Docs/features/project-trust.md` (see below). Em dashes in table cells meaning
"none" became empty cells or `n/a`, and UTF middle dots used as separators became commas (CLI
reference) or a list (the `Docs/microsoft-comparison.md` legend). Other edits: negation-then-reveal and history phrasing ("no
longer", "now", "used to", "as before") where the positive statement carries the fact, gravitas
and chat register ("Yes, this project includes", "honest", "first-class", "escape hatch",
"control plane"), headings such as "Why this approach" and "Honest limitations", and improvised
hyphen compounds. Technical claims were kept as they were.

Left as written, and why:

- Bold labels at the start of bullets in the feature pages: each names a stage, module or field
  the reader looks up.
- "gate" in `ROADMAP.md` and `Docs/gaps-and-future-work.md`: the project's name for its release
  checks.
- The `**Status:** ✅ shipped` headers of the feature pages: a shared badge format across the
  pages.
- `Docs/features/project-trust.md` wording, its one semicolon and the historical notes in it
  ("used to"): owned by the security workstream, and the history explains why each rule exists.
- `Docs/campaign/`, `AUDIT-BACKLOG.md`, `BENCHMARKS.md`, `benchmarks/*.md` and `.claude/skills/`:
  outside this review's scope.
- The two code comments under "Code comments and help text": code is outside this review's scope.

`grep -niE 'advania|customers/' Docs README.md ROADMAP.md plugin` finds only the line in
`Docs/campaign/README.md` that states this check. No doc names a customer.

## Re-check 2026-09-27 after the round 8 merges

Checked `git diff 2b7bce37..4c1b0ae6 -- crates` (72 files) and the `docs(campaign)` commit messages
against every doc listed below, for the local test interpreter, the test router, the call graph and
summary store, the native debug adapter, and the grammar's fix for an attribute on a member placed
after a global var section. Two of the interpreter fixes (`e48f3ab6`, `77fbf34b`, the rename key and
relation-cascade behavior) already updated `Docs/features/native-test-runtime.md` in the same
commit, so those claims were already correct and are not repeated here.

### Docs read, with a claim corrected

- `Docs/features/native-test-runtime.md:50-53`. Old claim: the Dispatch paragraph said nothing about
  what a codeunit's globals mean across calls. What is true now: a codeunit variable keeps its own
  globals between calls made on it, a `SingleInstance` codeunit has one instance for the test's
  whole lifecycle, an event subscriber's codeunit runs on a fresh instance, and a label is not
  state. Added a sentence covering this.
- `Docs/features/native-test-runtime.md:78-83`. Old claim: "assigning shares the object, and a token
  from `Get` changes its parent... take `SelectToken` paths (`$.a.b[0]`)", with no mention of
  `ReadFrom`'s effect on aliasing, no filter/wildcard/recursive-descent support, no
  `DefaultIfNotFound`, and no statement-position failure behavior. What is true now: `ReadFrom` gives
  the variable a new node and leaves the old one where it was (so an earlier alias keeps the old
  value), `SelectToken` follows `[?(...)]` filters, `..` and `*` and refuses slices, unions, regular
  expressions and grouped filters (a literal path with one of those routes the test to live BC), the
  `JsonObject` typed getters honour `DefaultIfNotFound`, and a failed `Get`, `ReadFrom`,
  `SelectToken`, `Add`, `Replace`, `Insert`, `Set` or `RemoveAt` raises in statement position and
  returns false where its result is read.
- `Docs/features/native-test-runtime.md:112-115`. Old claim: "single-pass DeleteAll, and ModifyAll
  (the value is coerced to the field's type, and primary-key fields and `RunTrigger` are refused)".
  What is true now: `RunTrigger` is not refused. `DeleteAll` and `ModifyAll` run each row through the
  same events as `Delete` or `Modify`, and the row's trigger when `RunTrigger` is true, whenever the
  table has a subscriber to that event or a trigger `RunTrigger` would run, and otherwise write every
  matching row in one pass as before. Only assigning a primary-key field is still refused.
- `Docs/features/native-test-runtime.md:133-137`. Old claim: "Insert, Modify, Delete and Rename raise
  the table's OnBefore/OnAfter events... whatever `RunTrigger` says", with `DeleteAll` and
  `ModifyAll` absent from the list. What is true now: `DeleteAll` and `ModifyAll` raise the same
  Delete or Modify events for every row they touch. Added a clause naming them.
- `Docs/features/native-test-runtime.md:148-151`. Old claim: the Routing bullet described the
  transitive call/trigger/interface/event graph with no mention of `Rename`, `DeleteAll`,
  `ModifyAll`, table-code record calls, or codeunit state. What is true now: those record operations
  and a bare `Rec`/`xRec`/method call in table code reach their table's event subscribers the same
  way a named call does, and a reachable `SingleInstance` codeunit with variable globals still routes
  to live BC unless it is the test's own codeunit, because its state outlives one test on BC while
  the local run starts each test afresh.
- `Docs/features/debugging-dap.md:113-115`. Old claim: "AL file paths resolve to (ObjectType,
  ObjectId) via the workspace index", which described resolving one object per file. What is true
  now: each breakpoint resolves to the object around its own line (the last one declared at or above
  it), so a file that declares more than one object sets every breakpoint on the right one, and a
  `setBreakpoints` call clears a file's previously tracked breakpoints even when none of its
  requested lines resolve to an object.

### Docs read, nothing to change

- `Docs/testing-guide.md`: process and verification commands, not affected by these interpreter,
  router, insight, or DAP behavior changes.
- `Docs/current-limitations.md`: its Native Test Runtime and Debugging sections stay at a summary
  level that the round 8 fixes do not contradict.
- `Docs/gaps-and-future-work.md`: a dated evidence ledger (`a4e7d5fe`), out of scope for a behavior
  re-check.
- `Docs/microsoft-comparison.md`: a capability table general enough that round 8 leaves every claim
  in it standing.
- `Docs/features/daemon-protocol.md`: the daemon's own `debug` `breakpoint` command already resolved
  its object per line before this round. The fixed bug was in the native `--dap` adapter
  (`crates/al-dap`), covered separately in `debugging-dap.md`.
- `Docs/features/language-server.md`: describes LSP query behavior, outside the crates this round
  changed.
- `Docs/reference/daemon-methods.md`: its `debug` entry already describes per-call breakpoint
  behavior generically and names no per-file resolution rule to correct.
- `Docs/reference/cli-commands.md`: the `debug breakpoint <file> <line>` row already reads as
  per-line and needed no change.
- `README.md`: its testing and debugging rows are summary-level and were not contradicted.
- `plugin/skills/bc-test-locally/SKILL.md`: covers running `test-classify`/`test-run-all` and reading
  their output. It stays at the CLI/JSON level.
- `plugin/skills/bc-impact-check/SKILL.md`: covers the `impact` call graph query. Its `ModifyAll`
  mention is one example of a field's `write` type on that command's output shape, unaffected by the
  transaction-lint per-object fix, which is not this skill's subject.
- No `crates/al-test/README.md` or other crate-level README exists in the repository.

### Claims not settled from the code

None. Every changed area named in the task had a matching commit and code path to confirm the claim
against.

## Re-check 2026-09-27, round 9

Branch `campaign/docs-recheck-3`, based on `campaign/2026-09-21` at 4c429ac5. Checked the round 9
runtime fixes (aaa4097d, 93bdfb9a, 1b10dafe, e6551978, 6280a560, 217af726), the List and Dictionary
commits (af3a36cd, 404f2fb4, b2577ceb), the round 8 last batch (merge 8f70b75e: 95f1905b, 985d68a6,
f2cec26c, a59621d8, 4143be7a), the security round 5 fixes (merge 68f0bbe8, every status in
`r5-security.md`) and grammar 142aba6 against the docs listed below, with the same method as the
round 8 re-check. The runtime claims were also run with the release `al-explorer` built at 12:53 on
a copy of `crates/al-test-harness/data/test_al_project` in `/tmp`: a probe table with globals, a
probe subscriber and ten probe tests passed locally (table globals kept and cleared by `Reset`,
`ModifyAll` rows sharing one copy, `Clear` on a codeunit, a record and a temporary record, a
subscriber on its own instance, a table publisher's `Sender`, `X:=-1`, a `-5..-3:` case label, a
List shared by assignment), and `test-classify` sent `GetRange(1, 1, Part)` to live BC with the
reason `calls List.GetRange with a var result list`.

### Docs read, with a claim corrected

- `Docs/features/native-test-runtime.md:50-52`. Old claim: dispatch runs "receiver-specific stubs
  → catalog stubs → built-in globals → real workspace procedures". What the code does
  (e6551978, `dispatch/routing.rs`): a call on an object tries that codeunit's stub, then workspace
  procedures. A bare call tries the builtins, then the running object's procedures, and a stub
  catalog answers only a call on its own codeunit. Fixed in 0738d54c.
- `Docs/features/native-test-runtime.md:54`. Old claim: "`var` scalar parameter write-back", with
  variables as the only target. What the code does (e6551978, `apply_var_writebacks`): a record
  field argument such as `Rec.Name` also takes the value back. Fixed in 0738d54c.
- `Docs/features/native-test-runtime.md:56-57`. Old claim: "an event subscriber's codeunit runs on
  a fresh instance each time it fires". What the code does (1b10dafe, `globals_for_call`): a fresh
  instance also while another instance of the codeunit is running, and a `SingleInstance`
  codeunit's subscriber joins its one instance. Added the `SingleInstance` exception. Fixed in
  0738d54c.
- `Docs/features/native-test-runtime.md:71-76`. Old claim: the builtin catalog left out `Clear`.
  What the code does (e6551978, `cleared`): `Clear` is a builtin that sets the variable to its
  type's default. A codeunit variable gets a new instance, a JSON variable a new empty node, and a
  record a new view with fields, filters and table globals reset. Database rows stay and a
  temporary record's rows go with the old view. Fixed in 0738d54c.
- `Docs/features/native-test-runtime.md:83-84`. Old claim: the List bullet listed `GetRange` with
  no word on its three-argument form. What the code does (b2577ceb, `router/ast.rs`
  `argument_count`): `GetRange(Index, Count, var Result)` routes the test to live BC. Fixed in
  0738d54c.
- `Docs/features/native-test-runtime.md:138-140`. Old claim: the DeleteAll and ModifyAll bullet
  said nothing about table globals. What the code does (aaa4097d, 93bdfb9a,
  `without_record_globals`): the rows' triggers share one copy of the globals that starts from the
  defaults, and the record's own globals come back afterwards. Fixed in 0738d54c.
- `Docs/features/native-test-runtime.md:157-159`. Old claim: the table code bullet said nothing
  about table globals. What the code does (aaa4097d, `run_table_code`, `reset_record_globals`): a
  record variable keeps its table's globals between table code calls, and `Reset` sets them back to
  their defaults. Fixed in 0738d54c.
- `Docs/features/native-test-runtime.md:162-164`. Old claim: the Events bullet did not mention
  `Sender`. What the code does (95f1905b, `raise_published_event`): a publisher whose
  `IncludeSender` argument is true passes its codeunit, or for a table procedure the record it runs
  on, as `Sender`, and a write to a `var Sender` changes that record. Fixed in 0738d54c.
- `Docs/features/analysis-and-insight.md:26-28`. Old claim: `Insert`, `Modify`, `Delete` and
  `Validate` produce table event edges, with the others left out. What the code does
  (`calls/edges.rs` `record_op_event_names` and the bare call branch, from 958d81a5 in round 8):
  `Rename`, `ModifyAll` and `DeleteAll` produce edges too, and so do `Rec`, `xRec` and a bare record
  method in table code. Fixed in 5103c5a8.
- `Docs/features/analysis-and-insight.md:31-32`. Old claim: none on a name declared more than once.
  What the code does (4143be7a, `populate_call_edges_in_object`): each field's `OnValidate` and each
  overload share one node, which gets the calls of every declaration. Fixed in 5103c5a8.
- `Docs/features/project-trust.md:59-60`. Old claim: the record lists linked package folders
  "from any settings file". What the code does (63083381, `linked_package_folders` over the
  configuration `read_repository` builds): the folders come from the project's settings files and
  `~/.config/al-lsp/settings.json`. Zed user settings are not read. Fixed in 1c56a732.
- `Docs/features/project-trust.md:140-142`. Old claim: the fingerprint stamps "the launch file",
  "six `stat` calls". What the code does (`inputs_fingerprint`, since b41668e1): it stamps both
  `.zed/debug.json` and `.vscode/launch.json`, up to seven `stat` calls. Fixed in 1c56a732.
- `Docs/features/semantic-bridge.md:125-128`. Old claim: the project's folders are searched only
  when the project is trusted, with nothing on the record check. What the code does (2411d08c,
  `CustomAnalyzerSearch::resolve`): in a trusted project a copy found there loads only when the
  trust record lists that file with its current hash. Fixed in c4ca2fd0.
- `Docs/reference/cli-commands.md:100`. Old claim: `pack-native --validate` refuses "a custom
  analyzer from an untrusted repository's own folders". What the code does (2411d08c,
  `validation_analyzer_entries` through `CustomAnalyzerSearch`): it also refuses a copy in a trusted
  project whose record does not list it, which covers a name passed with `--analyzers`. Fixed in
  c4ca2fd0.
- `Docs/reference/settings.md:74`. Old claim: `al.compilationOptions` is 🔒, which reads as
  "applies once trusted". What the code does (427f83ff, `is_path_option`, `grant_refusal`): a
  repository entry that names a file alc loads from makes an existing record stale and
  `al-explorer trust` refuses the project. Fixed in 201bb309.

### Docs read, nothing to change

- `Docs/features/project-trust.md`, apart from the two rows above: the round 5 merge rewrote it,
  and its claims on symbolic links and neighbour hashing (`with_project_contents`,
  `collect_files`), the 50,000 entry cap, the refused `compilationOptions` switches, project copies
  of analyzer names, the proxy's on-premises rule and `applicationFamily` check
  (`proxy_debug_config`, `launch_servers`) and the check before each `dotnet` spawn
  (`enforce_dotnet_path_before_spawn`, the `--official-lsp` start) match the code.
- `Docs/features/debugging-dap.md`: it names the `--dap-legacy` proxy and makes no claim about the
  proxy's trust judgement.
- `Docs/current-limitations.md`: its native test runtime and `dotnetPath` sections stay at a level
  these fixes do not contradict.
- `Docs/features/symbol-and-package-engine.md`: it does not describe the package source summary
  cache that 6280a560 fixed.
- `Docs/features/parsing-and-syntax.md` and `Docs/gaps-and-future-work.md`: no claim on operator
  lexing, and the corpus count 46,389/46,389 still holds at 142aba6.
- `Docs/reference/daemon-methods.md`, `Docs/reference/mcp-tools.md`, `Docs/features/cli-and-tui.md`:
  nothing on these areas beyond names and links.
- `README.md` and `ROADMAP.md`: summary level, not contradicted.
- `plugin/skills/bc-test-locally/SKILL.md`: CLI and JSON usage only. `plugin/README.md` does not
  exist.

### Left for the sibling branch

- `Docs/features/native-test-runtime.md:86`, the `Keys` order sentence, and `:87-88`, the bullet
  that List and Dictionary are references: a fix agent is changing both. The List bullet above them
  (`:83-84`) now ends with the `GetRange` routing sentence, so a merge touches the lines next to
  theirs.

### Code comments that disagree with the code (not changed)

- `crates/al-project/src/trust.rs:701`, the `inputs_fingerprint` doc comment: "This is six `stat`
  calls". The function stamps up to seven paths since both launch files were added.
