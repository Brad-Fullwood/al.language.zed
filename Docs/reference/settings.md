# Settings Reference

This is the canonical settings reference. A ready-to-copy template is
[`examples/zed-settings.jsonc`](../../examples/zed-settings.jsonc).

Settings live under `lsp.al-lsp.settings` in Zed's `settings.json` (or `.zed/settings.json`). Keys may
be flat, dotted (`"al.enableCodeAnalysis"`, recommended), or nested under an `"al"` object — all three
are accepted. The machine-readable schema is [`schemas/settings.json`](../../schemas/settings.json),
kept in lockstep with the keys the server reads by a repo test.

Status: ✅ honored · 🟡 honored, partial · ⛔ parsed but inert.

## Semantic analysis

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableCodeAnalysis` | boolean | `true` | ✅ gates the .NET CodeAnalysis bridge |
| `al.backgroundCodeAnalysis` | boolean | `true` | ✅ |
| `al.diagnosticsScope` | `project`\|`openFiles` | `project` | ✅ |
| `al.diagnosticsTrigger` | `continuous`\|`onSave` | `continuous` | ✅ |
| `al.codeAnalyzers` | string[] | `["CodeCop","AppSourceCop","UICop","PerTenantCop"]` | ✅ (incl. 3rd-party DLL paths) |
| `al.enableExternalRulesets` | boolean | `false` | ✅ official `alc` backend; not the in-process bridge |
| `al.ruleSetPath` | string\|null | `null` | ✅ official `alc` backend; not the in-process bridge |
| `al.assemblyProbingPaths` | string[] | `[]` | ✅ official `alc` backend; not the in-process bridge |
| `al.outputAnalyzerStatistics` | boolean | `false` | ✅ official `alc` backend; not the in-process bridge |

## Editor features

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableCodeActions` | boolean | `true` | ✅ |
| `al.inlayHints.parameterNames` | boolean | `true` | ✅ |
| `al.inlayHints.returnTypes` | boolean | `false` | ✅ |

## Native lint

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableNativeLint` | boolean | `true` | ✅ wired (file, project-semantic, and transaction-stack rules) |
| `al.nativeLintRules` | object | `{}` | ✅ wired (per-rule override) |

## Symbols & packages

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.packageCachePath` | string\|null | `null` (→ `<project>/.alpackages/`) | ✅ |
| `al.appLocalFolderPaths` | string[] | `[]` | ✅ |
| `al.nugetFeeds` | object[] `{name,url}` | `[]` | ✅ |
| `al.useOnlyCustomFeeds` | boolean | `false` | ✅ |
| `al.symbolsCountryRegion` | string\|null | `null` | ✅ |

## Compiler

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.compilationOptions` | string[] | `[]` | ✅ official `alc` backend only |
| `al.incrementalBuild` | boolean | `false` | ✅ official `alc` backend only |
| `al.useOfficialCompiler` | boolean | `false` | ✅ escape hatch → `dotnet alc` |

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
`AL_BRIDGE_DIR`, `AL_ERROR_CODES_LIVE`, `AL_DAP_CAPTURE`, `AL_OAUTH_DISABLE_KEYRING`. BC credentials
(secrets, prefer OAuth/keyring): `BC_CLIENT_ID`, `BC_TOKEN`/`BC_ACCESS_TOKEN`,
`BC_USERNAME`/`BC_PASSWORD`, `BC_TENANT`.

## Project-file schemas

Associate `schemas/{app,ruleset,appsourcecop,migration}.json` with Zed's bundled JSON LS via
`json.schemas` for autocomplete/validation on every channel today; see
[language-assets](../features/language-assets.md).
