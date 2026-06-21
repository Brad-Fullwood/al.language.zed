# Settings Reference

The **canonical, fully-described** settings reference is [`docs/settings.md`](../../docs/settings.md)
(with types, defaults, descriptions, project-file schema associations, environment variables, and BC
credential variables), and a ready-to-copy commented template is
[`examples/zed-settings.jsonc`](../../examples/zed-settings.jsonc). This page is a status-tagged
summary; where it and `docs/settings.md` differ, `docs/settings.md` wins.

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
| `al.enableExternalRulesets` | boolean | `false` | 🟡 see ROADMAP (build/semantic wiring) |
| `al.ruleSetPath` | string\|null | `null` | 🟡 |
| `al.assemblyProbingPaths` | string[] | `[]` | 🟡 |
| `al.outputAnalyzerStatistics` | boolean | `false` | 🟡 |

## Editor features

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableCodeActions` | boolean | `true` | ✅ |
| `al.inlayHints.parameterNames` | boolean | `true` | ✅ |
| `al.inlayHints.returnTypes` | boolean | `false` | ✅ |

## Native lint (reserved)

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.enableNativeLint` | boolean | `true` | ⛔ inert (native lint engine not implemented) |
| `al.nativeLintRules` | object | `{}` | ⛔ inert |

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
| `al.compilationOptions` | string[] | `[]` | ✅ (passed to `alc`) |
| `al.incrementalBuild` | boolean | `false` | ✅ |
| `al.useOfficialCompiler` | boolean | `false` | ✅ escape hatch → `dotnet alc` |

## Debug adapter

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.editorServicesPath` | string\|null | `null` | ✅ (legacy DAP) |
| `al.editorServicesLogLevel` | off/error/warning/info/debug/trace | `warning` | ✅ |

## Project scaffolding

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.rootNamespace` | string\|null | `null` | ✅ |
| `al.publisher` | string\|null | `null` | ✅ |
| `al.namespaceTemplate` | string\|null | `null` | ✅ |
| `al.algoSuggestedFolder` | string\|null | `null` | ✅ |

## Resource limits & escape hatches

| Setting | Type | Default | Status |
| --- | --- | --- | --- |
| `al.maxDocumentSizeBytes` | integer\|null | `null` | ✅ |
| `al.useOfficialLsp` | boolean | `false` | ✅ delegate to Microsoft AL LSP (ALTool v17+) |
| `al.useOfficialDap` | boolean | `false` | ✅ use Microsoft `EditorServices.Host` (extension-side toggle) |

> `al.useOfficialLsp` / `al.useOfficialDap` are extension-side launch toggles (resolved in
> `src/settings.rs`), stripped before settings are forwarded to the server.

## Environment variables

For CI/troubleshooting (see `docs/settings.md` for full descriptions): `AL_TOOL_PATH`,
`AL_COMPILE_TIMEOUT_SECS`, `AL_LOG_FILE_LEVEL`, `AL_LSP_ALLOW_HTTP_FEED`, `AL_EDITOR_SERVICES_PATH`,
`AL_BRIDGE_DIR`, `AL_ERROR_CODES_LIVE`, `AL_DAP_CAPTURE`, `AL_OAUTH_DISABLE_KEYRING`. BC credentials
(secrets, prefer OAuth/keyring): `BC_CLIENT_ID`, `BC_TOKEN`/`BC_ACCESS_TOKEN`,
`BC_USERNAME`/`BC_PASSWORD`, `BC_TENANT`.

## Project-file schemas

Associate `schemas/{app,ruleset,appsourcecop,migration}.json` with Zed's bundled JSON LS via
`json.schemas` for autocomplete/validation on every channel today — see `docs/settings.md` and
[language-assets](../features/language-assets.md).
