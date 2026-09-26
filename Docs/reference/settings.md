# Settings Reference

This is the canonical settings reference. A ready-to-copy template is
[`examples/zed-settings.jsonc`](../../examples/zed-settings.jsonc).

Settings live under `lsp.al-lsp.settings` in Zed's `settings.json` (or `.zed/settings.json`). Keys may
be flat, dotted (`"al.enableCodeAnalysis"`, recommended), or nested under an `"al"` object — all three
are accepted. The machine-readable schema is [`schemas/settings.json`](../../schemas/settings.json),
kept in lockstep with the keys the server reads by a repo test.

Status: ✅ honored · 🟡 honored, partial · ⛔ parsed but inert.

🔒 marks a setting that needs project trust when it comes from the repository's own
`.vscode/settings.json` or `.zed/settings.json`. Written in your user settings it applies as
it always has; written in the clone it is dropped, with one message naming it, until you run
`al-explorer trust` on that project. See [project trust](../features/project-trust.md).

## Semantic analysis

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableCodeAnalysis` | boolean | `true` | ✅ gates the .NET CodeAnalysis bridge |
| `al.backgroundCodeAnalysis` | boolean | `true` | ✅ |
| `al.diagnosticsScope` | `project`\|`openFiles` | `project` | ✅ |
| `al.diagnosticsTrigger` | `continuous`\|`onSave` | `continuous` | ✅ |
| `al.codeAnalyzers` | string[] | `["CodeCop","AppSourceCop","UICop","PerTenantCop"]` | ✅ (incl. 3rd-party DLL paths), 🔒 entries that are not built-in tokens |
| `al.enableExternalRulesets` | boolean | `false` | ✅ official `alc` backend; not the in-process bridge |
| `al.ruleSetPath` | string\|null | `null` | ✅ official `alc` backend; not the in-process bridge, 🔒 outside the project |
| `al.assemblyProbingPaths` | string[] | `[]` | ✅ official `alc` backend; not the in-process bridge, 🔒 |
| `al.outputAnalyzerStatistics` | boolean | `false` | ✅ official `alc` backend; not the in-process bridge |

## Editor features

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableCodeActions` | boolean | `true` | ✅ |
| `al.inlayHints.parameterNames` | boolean | `true` | ✅ |
| `al.inlayHints.returnTypes` | boolean | `false` | ✅ |

## Formatting

These apply to LSP document and range formatting in the editor, on top of the editor's `tabSize`
and `insertSpaces`. `al-explorer format` reads `.alformat.json` in the project root instead, which
takes the same four keys plus `tabSize`, `insertSpaces` and `keywordCasing`.

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.formatting.blankLinesBetweenProcedures` | `preserve`\|`one`\|`two` | `preserve` | ✅ |
| `al.formatting.maxLineLength` | integer | `0` (no wrapping) | ✅ |
| `al.formatting.braceStyle` | `sameLine`\|`nextLine` | `nextLine` | ✅ |
| `al.formatting.sortProperties` | boolean | `false` | ✅ sorts contiguous object-level property runs |

## Native lint

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableNativeLint` | boolean | `true` | ✅ wired (file, project-semantic, transaction, obsolete, and architecture rules) |
| `al.nativeLintRules` | object | `{}` | ✅ wired (per-rule override, e.g. `{ "AL-NL005": false }`) |

## Symbols & packages

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.packageCachePath` | string\|null | `null` (→ `<project>/.alpackages/`) | ✅, 🔒 outside the project |
| `al.appLocalFolderPaths` | string[] | `[]` | ✅, 🔒 outside the project |
| `al.nugetFeeds` | object[] `{name,url}` | `[]` | ✅, 🔒 |
| `al.useOnlyCustomFeeds` | boolean | `false` | ✅, 🔒 |
| `al.symbolsCountryRegion` | string\|null | `null` | ✅ |

## Compiler

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.compilationOptions` | string[] | `[]` | ✅ official `alc` backend only, 🔒 |
| `al.incrementalBuild` | boolean | `false` | ✅ official `alc` backend only |
| `al.useOfficialCompiler` | boolean | `false` | ✅ escape hatch → `dotnet alc` |
| `al.dotnetPath` | string\|null | `null` | ✅ extension-side executable override for all spawned .NET/`alc` processes; replaces environment-only `AL_DOTNET_PATH` configuration, 🔒 when it names a program inside the project |

## Resource limits & escape hatches

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.maxDocumentSizeBytes` | integer\|null | `null` | ✅ |
| `al.useOfficialLsp` | boolean | `false` | ✅ delegate to Microsoft AL LSP (ALTool v17+) |
| `al.useOfficialDap` | boolean | `false` | ✅ use Microsoft `EditorServices.Host` (extension-side toggle) |

> `al.useOfficialLsp` / `al.useOfficialDap` are extension-side launch toggles (resolved in
> `src/settings.rs`), stripped before settings are forwarded to the server.

## Environment variables

For CI, custom templates, and troubleshooting: `AL_TOOL_PATH`, `AL_DOTNET_PATH`,
`AL_TEMPLATES_DIR`,
`AL_COMPILE_TIMEOUT_SECS`, `AL_LOG_FILE_LEVEL`, `AL_LSP_ALLOW_HTTP_FEED`, `AL_EDITOR_SERVICES_PATH`,
`AL_BRIDGE_DIR`, `AL_ERROR_CODES_LIVE`, `AL_DAP_CAPTURE`, `AL_OAUTH_DISABLE_KEYRING`,
`AL_EXPLORER_PATH` (the `al-explorer` that `experimental/runnables` answers name, default:
beside `al-lsp`).

Daemon and client:

- `AL_REQUEST_TIMEOUT_MS`: per-request deadline for `al-explorer` and other daemon clients
  (default 30000). `--timeout-ms` overrides it for one command.
- `AL_DAEMON_IDLE_SECS`: how long an idle daemon stays up (default 1800, `0` keeps it running).
- `AL_ALLOW_MISMATCHED_DAEMON=1`: keep using a running daemon built from other code instead of
  replacing it.
- `AL_ALLOW_INSECURE_BC_HTTP=1`: allow credentials to a non-loopback Business Central server over
  `http`. See [project trust](../features/project-trust.md).

BC credentials (secrets, prefer OAuth/keyring): `BC_CLIENT_ID`, canonical `BC_ACCESS_TOKEN`
(`BC_TOKEN` is a compatibility alias), and `BC_USERNAME`/`BC_PASSWORD`. When both bearer-token
variables are set they must match; a blank, non-UTF-8, or conflicting override is rejected before
network access.

## Project-file schemas

Associate `schemas/{app,ruleset,alarch,appsourcecop,migration}.json` with Zed's bundled JSON LS via
`json.schemas` for autocomplete/validation on every channel today; see
[language-assets](../features/language-assets.md).
