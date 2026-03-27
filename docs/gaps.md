# AL Extension — Known Gaps & Future Work

## Settings Autocomplete
- **Gap**: `zed_extension_api` v0.8.0 adds `language_server_workspace_configuration_schema()` and `language_server_initialization_options_schema()` for settings autocomplete. BUT v0.8.0 is not yet published to crates.io (latest is 0.7.0).
- **When available**: Upgrade `Cargo.toml` from `zed_extension_api = "0.7.0"` to `"0.8.0"`, then implement the schema methods in `src/lib.rs` returning our `schemas/settings.json` content. This will enable autocomplete in Zed's settings UI.
- **Workaround**: Settings documented in `schemas/settings.json` and below. Users must type setting names manually until v0.8.0 is published.
- **References**: [Zed issue #18287](https://github.com/zed-industries/zed/issues/18287), [PR #26633](https://github.com/zed-industries/zed/pull/26633)

## Available Settings (all configurable via Zed settings.json)

```json
{
  "lsp": {
    "al-lsp": {
      "settings": {
        "al.enableCodeAnalysis": true,
        "al.backgroundCodeAnalysis": true,
        "al.diagnosticsScope": "project",
        "al.diagnosticsTrigger": "continuous",
        "al.codeAnalyzers": ["CodeCop", "AppSourceCop", "UICop", "PerTenantCop"],
        "al.enableNativeLint": true,
        "al.nativeLintRules": {},
        "al.enableCodeActions": true,
        "al.inlayHints.parameterNames": true,
        "al.inlayHints.returnTypes": false,
        "al.semanticFolding": true,
        "al.enableExternalRulesets": false,
        "al.ruleSetPath": null,
        "al.assemblyProbingPaths": [],
        "al.compilationOptions": [],
        "al.incrementalBuild": false,
        "al.rootNamespace": null,
        "al.publisher": null,
        "al.editorServicesPath": null,
        "al.editorServicesLogLevel": "warning",
        "al.symbolsCountryRegion": null,
        "al.outputAnalyzerStatistics": false
      }
    }
  }
}
```

## Settings Wiring Status

| Setting | Config field | Wired? | Notes |
|---------|-------------|--------|-------|
| al.enableCodeAnalysis | enable_code_analysis | YES | Gates CodeAnalysis bridge diagnostics |
| al.backgroundCodeAnalysis | background_code_analysis | YES | Gates background analysis in diagnostic pipeline |
| al.diagnosticsScope | diagnostics_scope | YES | "project" = lint all files; "openFiles" = only open tabs |
| al.diagnosticsTrigger | diagnostics_trigger | PARTIAL | Config parsed but continuous/onSave switching not fully implemented |
| al.codeAnalyzers | code_analyzers | YES | Passed to bridge analyze(), supports custom DLL paths |
| al.enableNativeLint | enable_native_lint | YES | Master toggle for AL-L001+ rules |
| al.nativeLintRules | native_lint_rules | PARTIAL | Config parsed but per-rule override not checked in lint walker |
| al.enableCodeActions | enable_code_actions | YES | Gates code action generation |
| al.inlayHints.parameterNames | inlay_hints.parameter_names | YES | Controls parameter name inlay hints |
| al.inlayHints.returnTypes | inlay_hints.return_types | PARTIAL | Config parsed but return type hints not implemented |
| al.semanticFolding | semantic_folding | PARTIAL | Config parsed but no semantic folding distinct from tree-sitter |
| al.enableExternalRulesets | enable_external_rulesets | NO | Config parsed, not wired |
| al.ruleSetPath | rule_set_path | NO | Config parsed, not wired |
| al.assemblyProbingPaths | assembly_probing_paths | NO | Config parsed, not wired to analyzer DLL discovery |
| al.compilationOptions | compilation_options | YES | Passed to alc on compile |
| al.incrementalBuild | incremental_build | YES | Used in compile command |
| al.editorServicesPath | editor_services_path | YES | Used by DAP adapter |
| al.editorServicesLogLevel | editor_services_log_level | YES | Used by DAP adapter |
| al.symbolsCountryRegion | symbols_country_region | YES | Used in symbol download |
| al.outputAnalyzerStatistics | output_analyzer_statistics | NO | Config parsed, not wired |

## Feature Gaps vs Cursor (MS AL Extension)

### CodeLens
- **Status**: LSP returns correct CodeLens data (reference counts on procedures/triggers)
- **Gap**: Zed 0.228.0 does not render CodeLens annotations in the editor UI
- **Fix needed**: Zed client support for CodeLens display

### Compiler Diagnostics via DLL
- **Status**: `SemanticBridge::analyze()` exists and can call CodeAnalysis DLLs directly
- **Gap**: The analyze call is wired into the diagnostic pipeline but the bridge may not be initialized for all project configurations
- **Fix needed**: Ensure bridge auto-initializes when ALTool is available

### Permission Set Warnings
- **Gap**: "The application object is not covered by any permission set" — this is a CodeAnalysis diagnostic that requires the compiler's full type resolution
- **Depends on**: Bridge analyze() returning these diagnostics correctly

### Event Publisher Restrictions
- **Gap**: "Event publisher should not have local variables" — CodeAnalysis compiler diagnostic
- **Depends on**: Bridge analyze()

### Third-Party Analyzers (e.g., BusinessCentral.LinterCop)
- **Status**: `al.codeAnalyzers` supports absolute DLL paths
- **Gap**: No auto-discovery of NuGet-installed analyzers
- **Fix needed**: Check common install locations for popular analyzers

### Unused Variable Rename/Remove Quick Fix
- **Gap**: Cursor offers "Remove unused variable" via code action. Our code actions don't include this.
- **Fix needed**: Add a quick-fix code action for AL-L005 diagnostics

### Breadcrumb Navigation
- **Gap**: Cursor shows breadcrumb trail (Object > Procedure > Block). Zed uses document symbols for this but the hierarchy may not be as detailed.

### Go-to-Implementation
- **Gap**: No `textDocument/implementation` handler for navigating from interface procedures to implementing codeunits
- **Fix needed**: Implement the LSP `implementation` capability

### Semantic Highlighting for Built-in Method Calls
- **Gap**: `Rec.Insert()`, `Rec.Modify()`, `Rec.FindFirst()` etc. should be colored as builtin methods on the record type
- **Status**: These get `function` token via the usage-site fix, but not `builtinFunction`

### LSP Pull Diagnostics (`textDocument/diagnostic`)
- **Status**: We use push-based `publishDiagnostics`. Zed supports pull diagnostics (LSP 3.17) via `diagnosticProvider` capability. Enabled by default in Zed with 50ms debounce.
- **Benefit**: Pull diagnostics give the client control over timing. Could implement `workspace/diagnostic` for project-wide diagnostics on demand.
- **Fix needed**: Add `diagnosticProvider` to server capabilities, implement `textDocument/diagnostic` handler. Push continues working alongside pull.

### LSP Runnables (`experimental/runnables`)
- **Status**: Zed's `enable_lsp_tasks: true` enables `experimental/runnables` protocol (originally from rust-analyzer). We don't implement it.
- **Benefit**: Could show run/debug buttons in the gutter for AL test procedures, event publishers, etc. — beyond what static `runnables.scm` captures provide.
- **Fix needed**: Implement `experimental/runnables` handler returning shell commands for AL tasks (compile, lint, run tests).

### `initialization_options` Usage
- **Status**: Our `language_server_initialization_options()` correctly provides `workspacePath`, `alResourceConfigurationSettings`, `setActiveWorkspace`. User overrides merge on top.
- **No gap**: Current usage is correct.

### `fetch` and `binary` Settings
- **`binary`**: Already supported — users can override al-lsp path via `"binary": { "path": "/custom/path/al-lsp" }`. Our `language_server_command()` reads this.
- **`fetch`**: Only affects Zed's built-in `LspInstaller` (for native adapters like rust-analyzer). Does NOT apply to WASM extensions. `null` is correct for us.
- **No gap**: Current handling is correct.

### Symbol Download Daemon Timeout
- **Gap**: `al download-symbols` fails with "Connection closed by daemon (EOF)"
- **Fix needed**: Increase daemon timeout for long-running operations, or use streaming response
