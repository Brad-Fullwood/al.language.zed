# CLI & TUI (`al-explorer`)

**Crate:** `crates/al-explorer` · **Status:** ✅ shipped (Linux, macOS, Windows)

`al-explorer` is the terminal companion to `al-lsp`. It has two modes selected at startup
(`main.rs`): with a subcommand it is a **JSON-RPC CLI** client of the daemon (with a global `--json`
flag for scripts/CI); with no subcommand it opens an interactive **TUI**. Its local daemon IPC uses
Unix-domain sockets on Linux/macOS and Windows named pipes, with the same JSON-RPC protocol on every
platform.

The CLI auto-starts the daemon if it isn't running (`DaemonClient::connect`), so most commands "just
work" from a project directory.

## CLI command surface

A complete list lives in the [CLI command reference](../reference/cli-commands.md). By category:

- **Setup/diagnostics:** `version`, `setup`, `doctor`, `diag`, `clear-cache`, `init-debug`.
- **Symbols/objects:** `search`, `object`, `by-id`, `composed`, `packages`, `deps`, `deps-graph`,
  `events`, `subscribers`, `event-source`, `builtins`, `rules`, `error-codes`,
  `generate-completions`.
- **LSP-style queries:** `hover`, `definition`, `references`, `signature`, `completions`, `symbols`,
  `folding`, `tokens`, `parse`, `rename`, `hints`.
- **Build/toolchain:** `compile`, `package`, `pack-native`, `download-symbols`, `authenticate`.
- **Insight/analysis:** `trace`, `intercept`, `entrypoints`, `graph`, `insight-stats`, `impact`,
  `suggest-event`, `metrics`, `dead-code`, `sql-scan`, `duplicates`, `arch-lint`, `breaking`,
  `upgrade`, `obsolete`, `audit-data`, `permission-audit`, `profiler-hints`.
- **Format/refactor/codegen:** `format`, `lint`, `fix`, `permissions`, `generate`, `new`,
  `add-application-area`, `add-tooltips`, `add-data-classification`, `sort-members`, `organize-files`.
- **Debug/profiling:** `debug {start|breakpoint|state|eval|continue|step|history|stop}`,
  `snapshot {start|list|download}`, `profile {start|stop|analyze}`.
- **Tests:** `tests`, `test-run`, `test-run-all`, `test-coverage`, `test-mutate`, `test-affected`,
  `test-classify`, `test-snapshot {validate|diff}`, `test-results`.
- **Translation:** `xlf {generate|refresh|untranslated|suggest}`.

### `--json` mode

Every command accepts the global `--json` flag. Human mode prints tables/indented text (status to
stderr); JSON mode prints structured results to stdout, with errors as `{ "error": "…" }`. This is the
contract that makes the whole toolchain CI- and agent-friendly. Example error when the daemon is
unreachable: `{ "error": "Cannot connect to al-lsp daemon… Hint: al-lsp daemon --project ." }`.

## TUI views

The TUI (ratatui + crossterm) has **five switchable views** (F1–F5, with Alt+1–Alt+5 fallbacks for
terminals that swallow function keys), a mode bar, mouse support, and **lazy hydration** — the initial
symbol dump is "slim" (no member arrays) and full member data is fetched on demand when you select an
object. Single-line inputs are capped at 4096 bytes.

| View | Key | Shows |
| --- | --- | --- |
| **Object Browser** | F1 | 4-pane: search (global/package toggle) + packages list, object-kind tabs + objects list, and a details pane (fields, keys, methods, controls, enums, properties). Enter/double-click opens the object — or a selected member — in Zed. |
| **Event Chain** | F2 | Search-as-you-type event suggestions (index-backed, instant) then a full multi-hop subscriber chain (`trace`, depth 10) with colored edge types. |
| **Call Graph / Impact** | F3 | Enter a symbol (e.g. `Customer."Credit Limit"`) → impact results grouped by reference type with package tags. |
| **Profiler** | F4 | Load an `.alcpuprofile`; hotspot table (procedure / object / self ms / total ms / hits), synthetic nodes skipped, capped at 500k nodes. |
| **Test Runner** | F5 | Hierarchical codeunit → method tree with status icons (○/✓/✗/⊘); `r` runs the selected codeunit, `R` runs all, error detail pane on the right. |

Navigation is consistent: arrows or `j`/`k`, Tab/Enter to move between panes, `Esc` to back out. The
TUI renders immediately with a "Loading workspace…" status while symbols load on a background thread.

## Zed task mapping

`languages/al/tasks.json` exposes ~55 tasks that shell out to these commands, using `$ZED_FILE`,
`$ZED_SYMBOL`, and `$ZED_ROW` substitutions. The *AL: Open Object Explorer* task launches the TUI in a
new terminal. The generated task definitions are in [languages/al/tasks.json](../../languages/al/tasks.json),
with their underlying commands documented in the [CLI reference](../reference/cli-commands.md).

## Microsoft comparison

There is no terminal companion in Microsoft's AL tooling — everything is VS Code UI. `al-explorer`
makes the *entire* engine scriptable (a JSON-RPC CLI for every analysis and LSP query) and adds an
interactive symbol/event/impact/profiler/test TUI that runs over SSH and in any terminal.

## Why this approach

A long-lived daemon plus a thin CLI means the expensive work (indexing packages, building graphs) is
paid once and reused across many fast commands — ideal for both interactive terminal use and CI loops.
Sharing the daemon dispatcher with MCP and Zed tasks guarantees one set of answers everywhere. MCP's
`al_call` exposes that complete dispatcher, so named aliases improve discovery without defining a
smaller agent-only feature set. The TUI exists because some questions (browsing objects, following
event chains) are inherently interactive and benefit from a fast, keyboard-driven UI that doesn't
need the editor open.

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
native Windows host, where they exercise the named-pipe implementation rather than merely
cross-compiling it. See [Testing guide — daemon IPC](../testing-guide.md#daemon-ipc-on-linux-macos-and-windows).

## Limitations & roadmap

- Windows uses a per-user named pipe; Linux and macOS use owner-only Unix-domain sockets.
- Some CLI workflows aren't yet Zed tasks (affected tests, snapshot diff/validation, `deps-graph`, XLIFF
  refresh/untranslated/suggest, table impact) — see `ROADMAP.md` (Zed UX).
- Event-subscriber/call-site coverage in the CLI/TUI is workspace-source-only (package `.app` symbols
  have no method bodies).
