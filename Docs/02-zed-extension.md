# 02 — Zed Extension Integration

This page documents the WASM extension (`zed-al`, root `src/`) that integrates the engine into Zed:
the extension manifest, how the `al-lsp` binary is resolved/downloaded, language-server and DAP
wiring, the MCP context server, settings handling, and the JSON schemas for project files.

## Extension manifest (`extension.toml`)

The manifest registers everything Zed needs:

```toml
id = "al"
name = "AL Language (Business Central)"
version = "0.2.2"
languages = ["languages/al"]
snippets  = ["snippets/al.json", "snippets/json.json"]
themes    = ["themes/bc-themes.json"]

[lib]                         # the Rust WASM extension
kind = "Rust"
version = "0.7.0"

[language_servers.al-lsp]     # the language server
name = "AL Language Server"
languages = ["AL"]

[context_servers.al-tools]    # the MCP server (spawns `al-lsp mcp`)
name = "AL Tools"

[debug_adapters.al]           # the debug adapter
[debug_locators.al]

[grammars.al]                 # the tree-sitter grammar revision Zed uses
repository = "https://github.com/Brad-Fullwood/AL-Tree-Sitter"
rev = "65e53be453b695eebf1be42000095729e24d4d88"
```

> **Release invariant.** The `[grammars.al].rev` must match the `tree-sitter-al` submodule commit
> recorded in the superproject for a release commit. A leading `+` in `git submodule status` is a
> release blocker. Zed uses this rev for highlighting while native parsing uses the bundled
> submodule; divergence shows up as parse-tree differences for the same `.al` file.

## Binary resolution (4-step chain)

The extension (`src/lib.rs`) resolves the `al-lsp` binary in a strict priority order. This is the
single most common new-user touchpoint, so it is built to degrade gracefully and to surface
actionable errors.

1. **User-configured path** — `lsp.al-lsp.binary.path` in Zed settings. If present and readable,
   used immediately.
2. **Previously downloaded binary** — `al-lsp-<VERSION>/<binary>` in the extension work directory,
   where `<VERSION>` is the latest GitHub release tag.
3. **`PATH` lookup** — `worktree.which("al-lsp")`. Covers dev builds, `cargo install`, `make install`,
   and system installs.
4. **GitHub release download → cache** — fetches the latest release of
   `Brad-Fullwood/al.language.zed` and downloads the asset for the current platform, then marks it
   executable and caches it (cleaning up old `al-lsp-*` versions).

Asset naming (kept in lockstep with `.github/workflows/release.yml` by a unit test and
`scripts/check-repo-consistency.sh`):

| Platform | Asset |
| --- | --- |
| Linux x86_64 | `al-linux-x86_64.tar.gz` |
| Linux aarch64 | `al-linux-aarch64.tar.gz` |
| macOS x86_64 | `al-macos-x86_64.tar.gz` |
| macOS aarch64 | `al-macos-aarch64.tar.gz` |
| Windows x86_64 | `al-windows-x86_64.zip` |

Unix archives ship `al-lsp`, `al-explorer`, and the semantic bridge files. The Windows archive
ships `al-lsp.exe` and the bridge but **not** `al-explorer` (it is Unix-only). Download progress is
shown in Zed's language-server panel when a `LanguageServerId` is available (the DAP path has none,
so no spinner there).

Error handling is deliberately helpful: a missing release, a missing asset, or a failed download
each produce an actionable message with a link to the releases page and a copy-paste settings
snippet for a manual binary override.

## Language server wiring

`language_server_command` (`src/lib.rs`) decides the arguments:

1. If the user set `lsp.al-lsp.binary.arguments`, use them verbatim (power-user escape hatch).
2. Else if `al.useOfficialLsp` is `true` (accepted flat, dotted, or nested under `"al"`), launch
   `al-lsp --official-lsp` to delegate to Microsoft's AL Language Server (requires ALTool v17+).
3. Else launch the native `al-lsp --stdio` (the default).

`language_server_initialization_options` sends the workspace path plus merged user settings under an
`"al"` key. User settings may be written flat, dotted (`"al.enableCodeAnalysis"`), or nested; the
extension normalizes all three, strips the `al.` prefix, caps key nesting depth at 64, and removes
the extension-only launch toggles (`useOfficialLsp`, `useOfficialDap`) before forwarding to the
server. `language_server_workspace_configuration` mirrors this for `workspace/configuration`
requests.

### Settings schema autocomplete (Zed 0.8+)

`build.rs` inspects the resolved `zed_extension_api` version in `Cargo.lock` and sets a
`zed_api_0_8` cfg on 0.8.0+. When that cfg is on, the extension implements
`language_server_workspace_configuration_schema` / `language_server_initialization_options_schema`
to return `schemas/settings.json`, so AL settings autocomplete and validate as you type. On the
committed/released 0.7 API those methods compile out, and settings still apply — just without
in-editor autocomplete.

> **Why pin a released API?** A `git`/main pin of `zed_extension_api` is an *unreleased* API: it
> loads only on Dev/Nightly Zed and silently fails on Stable. The committed state must always pin a
> released crates.io version (currently 0.7.0). A guard test (`committed_api_target_is_released`)
> enforces this; `scripts/use-api.sh dev|stable` toggles it for local experiments.

## Debug adapter wiring

`get_dap_binary` reuses the 4-step binary resolution. `src/dap.rs` builds the
`DebugAdapterBinary`:

- Default backend flag `--dap` (native Rust BC debug adapter); `al.useOfficialDap: true` selects
  `--dap-legacy` (the Microsoft `EditorServices.Host` proxy).
- Arguments passed to `al-lsp`: the backend flag, `/projectRoot:<path>`, optional `/server:<url>`,
  optional `/browser:<name>`.
- A malformed `launch.json`/`debug.json` produces a structured error rather than silently defaulting
  to `{}`.
- For `launch` requests, a build step (`al-explorer compile`) is emitted; `attach` skips the build
  (it connects to a running session). Debug configurations are authored via the bundled snippets and
  validated against `debug_adapter_schemas/al.json`.

See [debugging-dap](./features/debugging-dap.md) for what the adapter actually does.

## MCP context server

`context_server_command` launches `al-lsp mcp`. Unlike the LSP/DAP paths, the MCP context server
**requires `al-lsp` on `PATH`** (the released extension API offers no binary-download path at the
context-server scope), so the extension returns an actionable "run `make install`" error if it is
missing. See [ai-mcp](./features/ai-mcp.md).

## Zed tasks

`languages/al/tasks.json` defines ~55 editor tasks that shell out to `al-explorer`, and
`.zed/tasks.json` defines extension-development tasks. They cover build/package, debug start/stop,
symbol download/search/inspect, lint/format/fix/sort/organize, the full analysis suite (dead code,
impact, entrypoints, event tracing, SQL scan, complexity, duplicates, arch lint, breaking/upgrade/
obsolete reports, audits, profiler hints), translation (XLIFF generate), code generation (new
project, permission set, application-area/tooltip/data-classification fixups), and the test suite
(discover, run all, coverage, classify, results history, mutation). They use Zed variable
substitutions: `$ZED_FILE` (current file), `$ZED_SYMBOL` (word under cursor), `$ZED_ROW` (1-based
line). A full table is in [cli-and-tui](./features/cli-and-tui.md).

Because tasks require `al-explorer`, they are effectively **Unix-only**.

## Project-file JSON schemas

The extension ships JSON Schemas for the AL project files you hand-edit. Associate them with Zed's
bundled JSON language server (via the `json.schemas` block shown in
[`examples/zed-settings.jsonc`](../examples/zed-settings.jsonc)) to get autocomplete and validation
on **every Zed channel today**:

| Schema | Validates |
| --- | --- |
| `schemas/app.json` | the AL app manifest (`app.json`): id/name/publisher/version, runtime, target, dependencies, features, idRanges, resourceExposurePolicy, launch |
| `schemas/ruleset.json` | `*.ruleset.json` diagnostic severity overrides |
| `schemas/appsourcecop.json` | `AppSourceCop.json` analyzer configuration |
| `schemas/settings.json` | the `al.*` LSP settings (also used for in-editor autocomplete on 0.8+) |
| `schemas/migration.json` | `migration.json` data-upgrade manifest |

See [language-assets](./features/language-assets.md) for details and the
[settings reference](./reference/settings.md) for every setting.

## Platform support summary

| Surface | Linux | macOS | Windows |
| --- | --- | --- | --- |
| LSP (`al-lsp --stdio`) | ✅ | ✅ | ✅ |
| DAP (native / legacy) | ✅ | ✅ | ✅ |
| MCP context server | ✅ | ✅ | ⛔ (not shipped on released API) |
| `al-explorer` CLI/TUI + Zed tasks | ✅ | ✅ | ⛔ (Unix sockets; stub binary) |
| Auto-download binary | ✅ | ✅ | ✅ |

Continue to the feature pages from the [documentation index](./README.md), or read the
[Microsoft comparison](./microsoft-comparison.md).
