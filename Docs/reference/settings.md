# Settings Reference

Settings for the AL extension and `al-lsp`.
[`examples/zed-settings.jsonc`](../../examples/zed-settings.jsonc) is a template to copy.

Settings live under `lsp.al-lsp.settings` in Zed's `settings.json` (or `.zed/settings.json`). Keys may
be flat, dotted (`"al.enableCodeAnalysis"`, recommended), or nested under an `"al"` object. The
machine-readable schema is [`schemas/settings.json`](../../schemas/settings.json), and a repository
test checks that it lists the keys the server reads.

Status: ✅ honored. 🟡 honored, partial. ⛔ parsed but inert.

🔒 marks a setting that needs project trust when it comes from the repository's own
`.vscode/settings.json` or `.zed/settings.json`. Written in your user settings it applies
without trust. Written in the clone it is dropped, with one message naming it, until you run
`al-explorer trust` on that project. See [project trust](../features/project-trust.md).

## Semantic analysis

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableCodeAnalysis` | boolean | `true` | ✅ gates the .NET CodeAnalysis bridge |
| `al.backgroundCodeAnalysis` | boolean | `true` | ✅ |
| `al.diagnosticsScope` | `project`\|`openFiles` | `project` | ✅ |
| `al.diagnosticsTrigger` | `continuous`\|`onSave` | `continuous` | ✅ |
| `al.codeAnalyzers` | string[] | `["CodeCop","AppSourceCop","UICop","PerTenantCop"]` | ✅ (incl. 3rd-party DLL paths), 🔒 entries that are not built-in tokens |
| `al.enableExternalRulesets` | boolean | `false` | ✅ official `alc` backend only |
| `al.ruleSetPath` | string\|null | `null` | ✅ official `alc` backend only, 🔒 outside the project |
| `al.assemblyProbingPaths` | string[] | `[]` | ✅ official `alc` backend, and the custom analyzer search for both backends, 🔒 |
| `al.outputAnalyzerStatistics` | boolean | `false` | ⛔ read so a shared `.vscode/settings.json` still loads. A VS Code language server option, `alc` has no switch for it (it answers AL1009), so it changes nothing here |

## Editor features

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableCodeActions` | boolean | `true` | ✅ |
| `al.inlayHints.parameterNames` | boolean | `true` | ✅ |
| `al.inlayHints.returnTypes` | boolean | `false` | ✅ |

## Formatting

These apply to LSP document and range formatting in the editor, on top of the editor's `tabSize`
and `insertSpaces`. `al-explorer format` reads `.alformat.json` in the project root instead, which
takes the same four keys plus `tabSize`, `insertSpaces` and `keywordCasing` (`preserve`, `lower`, `upper`).
`keywordCasing` folds keyword tokens chosen by syntax tree node, so identifiers and type names such as
`Code`, `Date`, `Text` and `Record` stay as written, while `field`, `fields`, `begin` and `procedure` are
folded (audit finding `formatting-04-keyword-casing-rewrites-identifiers.md`). The editor has no
setting for it.

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.formatting.blankLinesBetweenProcedures` | `preserve`\|`one`\|`two` | `preserve` | ✅ |
| `al.formatting.maxLineLength` | integer | `0` (no wrapping) | ✅ wraps long single-line object properties such as `Permissions` at top-level commas only, not code, signatures or expressions (audit finding `formatting-02-max-line-length-properties-only.md`) |
| `al.formatting.braceStyle` | `sameLine`\|`nextLine` | `nextLine` | ✅ `nextLine` moves a `{` that ends a header line onto its own line, `sameLine` merges it up. Affects `{` only, never `begin` (audit finding `formatting-01-brace-style-nextline-is-noop.md`) |
| `al.formatting.sortProperties` | boolean | `false` | ✅ sorts contiguous object-level property runs |

## Native lint

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableNativeLint` | boolean | `true` | ✅ file, project-semantic, transaction, obsolete, and architecture rules |
| `al.nativeLintRules` | object | `{}` | ✅ per-rule override, for example `{ "AL-NL005": false }` |

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
| `al.compilationOptions` | string[] | `[]` | ✅ official `alc` backend only, 🔒. A repository entry that names a file alc loads from (`/analyzer:`, `/assemblyprobingpaths:`, `/ruleset:`, `/packagecachepath:`, an `@` response file) keeps the project from being trusted |
| `al.incrementalBuild` | boolean | `false` | ⛔ read so a shared `.vscode/settings.json` still loads. A VS Code language server option, `alc` has no incremental switch (it answers AL1009), so it changes nothing here |
| `al.useOfficialCompiler` | boolean | `false` | ✅ builds with Microsoft's `dotnet alc` |
| `al.dotnetPath` | string\|null | `null` | ✅ the `dotnet` executable for every .NET and `alc` process the toolchain spawns, the setting form of `AL_DOTNET_PATH`, 🔒. A program inside the project, or reached through a link written inside it, is checked again before each spawn |

## Resource limits & escape hatches

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.maxDocumentSizeBytes` | integer\|null | `null` | ✅ |
| `al.useOfficialLsp` | boolean | `false` | 🟡 run altool `launchlspserver` instead of al-lsp (ALTool v17+). Hover, definition, references, rename, formatting and completion only. See [language-server](../features/language-server.md#microsoft-comparison) |
| `al.useOfficialDap` | boolean | `false` | ✅ use Microsoft `EditorServices.Host` (extension-side toggle) |

> `al.useOfficialLsp` / `al.useOfficialDap` are extension-side launch toggles (resolved in
> `src/settings.rs`), stripped before settings are forwarded to the server.

## Environment variables

For CI, custom templates, and troubleshooting: `AL_TOOL_PATH`, `AL_DOTNET_PATH`,
`AL_TEMPLATES_DIR`, `AL_COMPILE_TIMEOUT_SECS`, `AL_LOG_FILE_LEVEL`, `AL_LSP_ALLOW_HTTP_FEED`,
`AL_EDITOR_SERVICES_PATH`, `AL_BRIDGE_DIR`, `AL_ERROR_CODES_LIVE`, `AL_DAP_CAPTURE`,
`AL_OAUTH_DISABLE_KEYRING`, and `AL_EXPLORER_PATH` (the `al-explorer` that
`experimental/runnables` answers name, by default the one beside `al-lsp`).

Daemon and client:

- `AL_REQUEST_TIMEOUT_MS`: per-request deadline for `al-explorer` and other daemon clients
  (default 30000). `--timeout-ms` overrides it for one command.
- `AL_DAEMON_IDLE_SECS`: how long an idle daemon stays up (default 1800, `0` keeps it running).
- `AL_ALLOW_MISMATCHED_DAEMON=1`: keep using a running daemon built from other code instead of
  replacing it.
- `AL_ALLOW_INSECURE_BC_HTTP=1`: allow credentials to a non-loopback Business Central server over
  `http`. See [project trust](../features/project-trust.md).

Sign-in client: the default client id is the AL extension's `41839ce3-4041-4bac-8c17-0941f25d7aaf`
(CloudHttpClientFactory.cs:228). A launch configuration's `entraIdAuthentication` block overrides
`endpoint` (https host only), `scope`, `clientId` and `redirectUri`. `redirectUri` is read and not sent,
because the interactive flow uses `http://localhost` and the device code flow has no redirect. `BC_CLIENT_ID`
overrides both. A tenant policy that blocks the AL client needs `BC_CLIENT_ID` or a launch override. Before an
interactive sign-in, a refresh token for the same client id and a GUID tenant is taken from the AL extension's
`~/.local/share/Microsoft/BusinessCentral/DevTools/TokenCacheUnprotected.dat` (read only, never written).
The protected `TokenCache.dat` is not read. Use `al-explorer authenticate login --device-code` on a headless host.

BC credentials (secrets, prefer OAuth/keyring): `BC_CLIENT_ID`, canonical `BC_ACCESS_TOKEN`
(`BC_TOKEN` is a compatibility alias), and `BC_USERNAME`/`BC_PASSWORD`. When both bearer-token
variables are set they must match. A blank, non-UTF-8, or conflicting override is rejected before
network access.

## Project-file schemas

Associate `schemas/{app,ruleset,alarch,appsourcecop,migration}.json` with Zed's bundled JSON LS via
`json.schemas` for completion and validation on every Zed release channel. See
[language-assets](../features/language-assets.md).
