# AL for Zed — Settings reference

This is the complete reference for configuring the AL Language Server (`al-lsp`)
in Zed. A ready-to-copy, fully-commented example lives at
[`examples/zed-settings.jsonc`](../examples/zed-settings.jsonc).

## Where settings live

AL settings go inside your Zed `settings.json` (command palette → **zed: open
settings**) or a project-local `.zed/settings.json`, under the language server's
`settings` block:

```json
{
  "lsp": {
    "al-lsp": {
      "settings": {
        "al.enableCodeAnalysis": true,
        "al.diagnosticsScope": "project"
      }
    }
  }
}
```

Keys may be written flat and dotted (`"al.enableCodeAnalysis"`, recommended and
used throughout this doc) or nested under an `"al"` object — both are accepted.
The machine-readable schema is [`schemas/settings.json`](../schemas/settings.json);
a repo test keeps it in lockstep with the keys the server actually reads.

## Settings

### Semantic analysis

| Setting | Type | Default | Description |
|---|---|---|---|
| `al.enableCodeAnalysis` | boolean | `true` | Enable semantic analysis via the .NET CodeAnalysis bridge (compiler-level diagnostics). |
| `al.backgroundCodeAnalysis` | boolean | `true` | Run analysis continuously in the background. Disable to only run on explicit compile. |
| `al.diagnosticsScope` | `"project"` \| `"openFiles"` | `"project"` | Lint all `.al` files, or only open tabs. |
| `al.diagnosticsTrigger` | `"continuous"` \| `"onSave"` | `"continuous"` | Run diagnostics on every (debounced) change, or only on save. |
| `al.codeAnalyzers` | string[] | `["CodeCop","AppSourceCop","UICop","PerTenantCop"]` | Analyzers to run. Add DLL paths for third-party analyzers (e.g. BusinessCentral.LinterCop). |
| `al.enableExternalRulesets` | boolean | `false` | Honor external `.ruleset.json` severity overrides. |
| `al.ruleSetPath` | string \| null | `null` | Path to a `.ruleset.json` for severity overrides. |
| `al.assemblyProbingPaths` | string[] | `[]` | Extra probing paths for CodeAnalysis DLLs / custom analyzers. |
| `al.outputAnalyzerStatistics` | boolean | `false` | Emit analyzer performance statistics. |

### Editor features

| Setting | Type | Default | Description |
|---|---|---|---|
| `al.enableCodeActions` | boolean | `true` | Enable quick fixes, refactorings, and source actions. |
| `al.inlayHints.parameterNames` | boolean | `true` | Show parameter-name hints at call sites. |
| `al.inlayHints.returnTypes` | boolean | `false` | Show return-type hints on procedure declarations. |

### Native lint (reserved)

`al.enableNativeLint` (boolean, default `true`) and `al.nativeLintRules`
(object of `code → boolean`, default `{}`) are **parsed for forward
compatibility but currently inert** — native lint diagnostics are not yet
implemented. All AL diagnostics today come from syntax parsing and the semantic
CodeAnalysis bridge.

### Symbols & packages

| Setting | Type | Default | Description |
|---|---|---|---|
| `al.packageCachePath` | string \| null | `null` | Symbol package cache path. Default: `<project>/.alpackages/`. |
| `al.appLocalFolderPaths` | string[] | `[]` | Extra local folders of `.app` files to index for symbols. |
| `al.nugetFeeds` | object[] | `[]` | Custom NuGet v3 feeds, tried before the built-in Microsoft feeds. Each entry: `{ "name": string, "url": string }` (`url` is the v3 `index.json`). |
| `al.useOnlyCustomFeeds` | boolean | `false` | Use only `al.nugetFeeds`; skip the built-in public Microsoft feeds. |
| `al.symbolsCountryRegion` | string \| null | `null` | Country/region code for localized BC symbols (e.g. `"us"`, `"gb"`, `"au"`). |

### Compiler

| Setting | Type | Default | Description |
|---|---|---|---|
| `al.compilationOptions` | string[] | `[]` | Extra command-line options passed to the AL compiler (`alc`). |
| `al.incrementalBuild` | boolean | `false` | Use incremental build when compiling. |
| `al.useOfficialCompiler` | boolean | `false` | Compile via Microsoft's `dotnet alc` subprocess instead of the pure-Rust native `.app` emitter. Native is the default for daemon compile, `al.compile`, and publish; use this escape hatch when you need Microsoft's full compile-time validation. |

### Debug adapter

| Setting | Type | Default | Description |
|---|---|---|---|
| `al.editorServicesPath` | string \| null | `null` | Path to the `EditorServices.Host` binary. Auto-discovered when unset. |
| `al.editorServicesLogLevel` | `off`\|`error`\|`warning`\|`info`\|`debug`\|`trace` | `"warning"` | Log verbosity for the debug-adapter process. |

### Project scaffolding

| Setting | Type | Default | Description |
|---|---|---|---|
| `al.rootNamespace` | string \| null | `null` | Default root namespace for scaffolded objects. |
| `al.publisher` | string \| null | `null` | Default publisher name for scaffolding. |
| `al.namespaceTemplate` | string \| null | `null` | Namespace template, e.g. `"{publisher}.{name}"`. |
| `al.algoSuggestedFolder` | string \| null | `null` | Suggested folder for AL-Go scaffolding output. |

### Resource limits

| Setting | Type | Default | Description |
|---|---|---|---|
| `al.maxDocumentSizeBytes` | integer \| null | `null` | Refuse to ingest a single document larger than this (bytes). `null` = no cap. |

### Escape hatch

| Setting | Type | Default | Description |
|---|---|---|---|
| `al.useOfficialLsp` | boolean | `false` | Delegate the session to Microsoft's official AL Language Server (`al-lsp --official-lsp`); requires ALTool v17+. An explicit `binary.arguments` override takes precedence. |

## Project-file schemas (app.json, rulesets, …)

Beyond the language-server settings above, this extension ships JSON Schemas for
the AL project files you edit by hand. Associate them with Zed's bundled JSON
language server to get **autocomplete + validation on every Zed channel today**:

```json
{
  "lsp": {
    "json-language-server": {
      "settings": {
        "json": {
          "schemas": [
            { "fileMatch": ["app.json"], "url": "https://raw.githubusercontent.com/Brad-Fullwood/al.language.zed/v0.2.2/schemas/app.json" },
            { "fileMatch": ["*.ruleset.json"], "url": "https://raw.githubusercontent.com/Brad-Fullwood/al.language.zed/v0.2.2/schemas/ruleset.json" },
            { "fileMatch": ["AppSourceCop.json"], "url": "https://raw.githubusercontent.com/Brad-Fullwood/al.language.zed/v0.2.2/schemas/appsourcecop.json" },
            { "fileMatch": ["migration.json", "*.migration.json"], "url": "https://raw.githubusercontent.com/Brad-Fullwood/al.language.zed/v0.2.2/schemas/migration.json" }
          ]
        }
      }
    }
  }
}
```

| File pattern | Schema | Describes |
|---|---|---|
| `app.json` | [`schemas/app.json`](../schemas/app.json) | The AL app manifest (id, version, dependencies, idRanges, features, …). |
| `*.ruleset.json` | [`schemas/ruleset.json`](../schemas/ruleset.json) | Diagnostic severity overrides. |
| `AppSourceCop.json` | [`schemas/appsourcecop.json`](../schemas/appsourcecop.json) | AppSourceCop analyzer config. |
| `migration.json` | [`schemas/migration.json`](../schemas/migration.json) | Data-upgrade (table/field ownership) manifest. |

These URLs are pinned to the `v0.2.2` release tag for reproducible validation;
bump the tag when you upgrade the extension.

## Autocomplete: what works today vs. later

| Surface | Autocomplete / validation |
|---|---|
| **`.al` source files** | ✅ Today — completions, hovers, etc. from the AL language server. |
| **Project files** (`app.json`, rulesets, …) | ✅ Today on all Zed channels — via the `json.schemas` associations above. |
| **The `lsp.al-lsp.settings` block** | ⏳ On Zed Dev/Nightly (extension API ≥ 0.8) the keys autocomplete + validate as you type. On Stable Zed they are applied correctly but without in-editor autocomplete — use [`examples/zed-settings.jsonc`](../examples/zed-settings.jsonc) as a template. This lights up on Stable automatically once the 0.8 extension API ships to the registry. |

## Environment variables (advanced)

These override or diagnose `al-lsp` behavior outside of settings. Most users
never need them; they're useful for CI, troubleshooting, or constrained
environments. Set them in the environment that launches Zed / `al-lsp`.

| Variable | Purpose |
|---|---|
| `AL_TOOL_PATH` | Override the path to the ALTool binary the toolchain invokes. |
| `AL_COMPILE_TIMEOUT_SECS` | Compilation timeout, in seconds. |
| `AL_LOG_FILE_LEVEL` | Log-file verbosity (`off`/`error`/`warning`/`info`/`debug`/`trace`). |
| `AL_LSP_ALLOW_HTTP_FEED` | Allow plain-HTTP (non-HTTPS) NuGet feeds. |
| `AL_EDITOR_SERVICES_PATH` | Override the `EditorServices.Host` path (mirrors `al.editorServicesPath`). |
| `AL_BRIDGE_DIR` | Override the semantic CodeAnalysis bridge directory. |
| `AL_ERROR_CODES_LIVE` | Fetch AL error-code metadata live instead of from the on-disk cache. |
| `AL_DAP_CAPTURE` | Capture debug-adapter stderr for diagnostics. |
| `AL_OAUTH_DISABLE_KEYRING` | Disable OS-keyring storage of OAuth tokens (fall back to in-process only). |

### Business Central credentials

For symbol download / BC server access, prefer the OAuth flow (tokens stored in
the OS keyring). These environment variables are fallbacks and hold
**secrets** — avoid committing them:

| Variable | Purpose |
|---|---|
| `BC_CLIENT_ID` | OAuth client id for BC symbol access. |
| `BC_TOKEN` / `BC_ACCESS_TOKEN` | Pre-acquired access token. |
| `BC_USERNAME` / `BC_PASSWORD` | Basic-auth credentials. |
