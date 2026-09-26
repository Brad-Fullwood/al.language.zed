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

## Claims not verified, left as written

## Code comments and help text that disagree with the code (not changed, code is out of scope)

- `crates/al-runtime/src/interpreter/dispatch/mod.rs:13`: module doc says recursion depth is capped
  at 100. `MAX_RECURSION_DEPTH` is 512.
- `al-explorer free-ids --help` examples say `al free-ids` where the binary is `al-explorer`.
