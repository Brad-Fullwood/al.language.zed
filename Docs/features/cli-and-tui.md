# CLI & TUI (`al-explorer`)

**Crate:** `crates/al-explorer`. **Status:** ✅ Linux, macOS, Windows

`al-explorer` is the terminal client for `al-lsp`. `main.rs` picks the mode at startup: with a
subcommand it is a **JSON-RPC CLI** client of the daemon, with a global `--json` flag for scripts
and CI. With no subcommand it opens an interactive **TUI**. It reaches the daemon over Unix-domain
sockets on Linux/macOS and named pipes on Windows, with the same JSON-RPC protocol on every
platform.

The CLI starts the daemon if it is not running (`DaemonClient::connect`), so most commands work
from a project directory with no setup.

## CLI command surface

The [CLI command reference](../reference/cli-commands.md) lists every command and flag. By category:

- **Setup/diagnostics:** `version`, `setup`, `doctor`, `diag`, `clear-cache`, `daemon-shutdown`,
  `init-debug`, `trust`.
- **Symbols/objects:** `search`, `object`, `by-id`, `source`, `location`, `composed`, `packages`,
  `deps`, `deps-graph`, `events`, `subscribers`, `event-source`, `builtins`, `rules`, `error-codes`,
  `generate-completions`.
- **LSP-style queries:** `hover`, `definition`, `references`, `signature`, `completions`, `symbols`,
  `folding`, `tokens`, `parse`, `rename`, `hints`.
- **Build/toolchain:** `compile`, `package`, `pack-native`, `publish`, `download-symbols`,
  `authenticate`.
- **Insight/analysis:** `trace`, `intercept`, `entrypoints`, `graph`, `insight-stats`, `impact`,
  `suggest-event`, `metrics`, `dead-code`, `sql-scan`, `duplicates`, `arch-lint`, `native-check`,
  `free-ids`, `breaking`, `upgrade`, `obsolete`, `package-diff`, `audit-data`, `permission-audit`,
  `profiler-hints`.
- **Format/refactor/codegen:** `format`, `lint`, `fix`, `permissions`, `generate`, `new`,
  `add-application-area`, `add-tooltips`, `add-data-classification`, `sort-members`, `organize-files`.
- **Debug/profiling:** `debug {start|breakpoint|state|eval|continue|step|history|stop}`,
  `snapshot {start|list|download}`, `profile {start|stop|analyze}`.
- **Tests:** `tests`, `test-run`, `test-run-all`, `test-coverage`, `test-mutate`, `test-affected`,
  `test-classify`, `test-snapshot {capture|validate|replay|diff}`, `test-results`.
- **Translation:** `xlf {generate|refresh|untranslated|suggest}`.

### `--json` mode

Every command accepts the global `--json` flag. Human mode prints tables/indented text (status to
stderr). JSON mode prints structured results to stdout, with errors as `{ "error": "…" }`, which is what
scripts, CI and agents read. Example error when the daemon is unreachable: `{ "error": "… Hint: Is the daemon running? Start it with: al-lsp daemon --project <dir>" }`.

Human and JSON modes also share exit codes. `0` means the requested check passed, `1` means an
error or blocking findings, and `75` is `doctor`'s answer while the daemon is still loading the
workspace. Complexity hotspots, any dead-code finding, SQL anti-patterns,
duplicate/architecture/native-check/breaking findings, unclassified data, and permission-audit
failures make the process exit non-zero in both output modes.

## TUI views

The TUI (ratatui + crossterm) has five views (F1–F5, with Alt+1–Alt+5 for terminals that swallow
function keys), a mode bar, and mouse support. The initial symbol dump has no member arrays, and an
object's members are fetched when you select it. Single-line inputs are capped at 4096 bytes.

| View | Key | Shows |
| --- | --- | --- |
| **Object Browser** | F1 | 4-pane: search (global/package toggle) + packages list, object-kind tabs + objects list, and a details pane (fields, keys, methods, controls, enums, properties). Enter or double-click opens the object, or the selected member, in Zed. |
| **Event Chain** | F2 | Event suggestions from the index as you type, then a full multi-hop subscriber chain (`trace`, depth 10) with colored edge types. |
| **Call Graph / Impact** | F3 | Enter a symbol (e.g. `Customer."Credit Limit"`) → impact results grouped by reference type with package tags. |
| **Profiler** | F4 | Load an `.alcpuprofile`. Hotspot table (procedure / object / self ms / total ms / hits), synthetic nodes skipped, capped at 500k nodes. |
| **Test Runner** | F5 | Hierarchical codeunit → method tree with status icons (○/✓/✗/⊘). `r` runs the selected codeunit, `R` runs all, error detail pane on the right. |

Every view uses the same keys: arrows or `j`/`k`, Tab/Enter to move between panes, `Esc` to back out. The
TUI renders immediately with a "Loading workspace…" status while symbols load on a background thread.

## Zed integration

The language package ships 55 tasks in `languages/al/tasks.json` and inline runnables in
`languages/al/runnables.scm`, and every one runs `al-explorer`. Stable Zed task JSON cannot address
the `al-explorer` binary inside the extension's download directory, so the tasks need `al-explorer`
on `PATH`. See [Language assets](language-assets.md#al-explorer-must-be-on-path) for the install step.

Without that install, the resolved `al-lsp` binary covers the same ground: LSP execute commands
provide editor actions, and the **AL Tools** MCP server exposes named operations and the complete
shared dispatcher through `al_call`. The commands are documented in the
[CLI reference](../reference/cli-commands.md).

## Microsoft comparison

Microsoft's AL tooling has no terminal client. Everything runs in the VS Code UI. `al-explorer`
makes its documented commands scriptable, and MCP `al_call` exposes the complete shared daemon
catalog. It also adds an interactive TUI for symbols, events, impact, profiles and tests that runs
over SSH and in any terminal.

## Design

A long-lived daemon behind a thin CLI indexes packages and builds graphs once, then reuses them for
every later command, in a terminal session or a CI loop. The CLI, MCP and the contributor tasks
share the daemon dispatcher, so they give the same answers. MCP's `al_call` exposes the complete
dispatcher, so the named aliases help an agent find operations without limiting which ones it can
call. The TUI is for questions that are easier to work through interactively, such as browsing
objects or following event chains, with the keyboard and without the editor open.

## How to use

```
cd my-al-project
al-explorer doctor              # check setup
al-explorer                     # open the TUI
al-explorer search Customer --json
al-explorer impact "Sales-Post.PostDocument"
al-explorer test-run-all --junit-out results.xml
```

## Platform verification

The shared protocol suite binds the host's real local transport, and the native harness verifies
that `al-explorer` auto-starts `al-lsp` and completes a JSON-RPC request. CI runs those checks on a
native Windows host, so they exercise the named-pipe code instead of only cross-compiling it. See
[Testing guide: daemon IPC](../testing-guide.md#daemon-ipc-on-linux-macos-and-windows).

## Compatibility boundaries

- Windows uses a per-user named pipe. Linux and macOS use owner-only Unix-domain sockets.
- The language package's tasks (`languages/al/tasks.json`) require `al-explorer` on `PATH`. Stable
  Zed task JSON cannot resolve a path inside the extension's directory, so a task cannot reach the
  copy of `al-explorer` the extension downloaded. See
  [Language assets](language-assets.md#al-explorer-must-be-on-path). The checkout's
  contributor-only `.zed/tasks.json` also has affected tests, snapshot diff/validation/live replay,
  `deps-graph`, XLIFF refresh/untranslated/suggestions, and table impact.
  `crates/al-test-harness/tests/extension_smoke.rs` runs every task in both files as
  `al-explorer <args> --help` with sample Zed variables, which checks that each names a real
  subcommand with valid arguments. The replay task reads the Business Central version it needs
  from `AL_BC_VERSION`.
- Call sites and event subscribers come from workspace source and from the AL source embedded in
  loaded packages. A package without embedded source contributes declarations and no bodies.
