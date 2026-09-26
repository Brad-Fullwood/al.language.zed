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
