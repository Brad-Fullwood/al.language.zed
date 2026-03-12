# Zed AL Extension Settings

This document defines supported settings and maps Cursor/VS Code `al.*` options to Zed equivalents.

## Status Definitions
1. Supported: implemented and wired through `al-core` and LSP settings.
2. Planned: in scope and required by the plan; implementation pending.
3. Not supported: explicitly out of scope and ignored.

## Mapping Table

| Cursor Key | Status | Zed Setting | Notes |
| --- | --- | --- | --- |
| `al.algoSuggestedFolder` | Supported | `al.project.scaffold.suggestedFolder` | Used by AL:Go scaffolder |
| `al.appLocalFolderPaths` | Supported | `al.symbols.localAppPaths` | Extra local app roots for symbol indexing |
| `al.areProfileLensesSupported` | Planned | `al.features.profileLenses` | Toggle profiler lenses in editor |
| `al.assemblyProbingPaths` | Supported | `al.semantic.assemblyProbingPaths` | Additional probing paths for CodeAnalysis |
| `al.backgroundCodeAnalysis` | Supported | `al.semantic.backgroundAnalysis` | Controls background semantic analysis |
| `al.browser` | Planned | `al.launch.browser` | Browser selection for publish/debug launch |
| `al.codeAnalyzers` | Supported | `al.semantic.codeAnalyzers` | External analyzer DLL paths |
| `al.compilationOptions` | Supported | `al.compiler.options` | Passed to compiler for CLI compile |
| `al.disableTestRunning` | Planned | `al.tests.disable` | Toggle test discovery and execution |
| `al.editorServicesLogLevel` | Supported | `al.dap.editorServicesLogLevel` | Controls DAP host logging |
| `al.editorServicesPath` | Supported | `al.dap.editorServicesPath` | Explicit path to EditorServices.Host |
| `al.enableCodeActions` | Supported | `al.features.codeActions` | Toggle code actions |
| `al.enableCodeAnalysis` | Supported | `al.semantic.enableCodeAnalysis` | Toggle semantic diagnostics |
| `al.enableExternalRulesets` | Supported | `al.semantic.enableExternalRulesets` | Allow local file rulesets only |
| `al.enableScriptIntelliSense` | Not supported | `` | Control add-in JS/TS is out of scope |
| `al.entraIdAuthentication` | Planned | `al.auth.entraId` | Entra ID auth config for server connections |
| `al.extendGoToSymbolInWorkspace.IncludeSymbolFiles` | Not supported | `` | Replaced by al-core workspace symbol index |
| `al.extendGoToSymbolInWorkspace.ResultLimit` | Not supported | `` | Replaced by al-core workspace symbol index |
| `al.extendGoToSymbolInWorkspace.enabled` | Not supported | `` | Replaced by al-core workspace symbol index |
| `al.incognito` | Planned | `al.launch.incognito` | Incognito/InPrivate mode for launch browser |
| `al.incrementalBuild` | Supported | `al.compiler.incremental` | Use incremental build when compiling |
| `al.inlayhints.functionReturnTypes.enabled` | Supported | `al.features.inlayHints.returnTypes` | Toggle return type hints |
| `al.inlayhints.parameterNames.enabled` | Supported | `al.features.inlayHints.parameterNames` | Toggle parameter name hints |
| `al.namespaceTemplate` | Supported | `al.project.namespaceTemplate` | Used by scaffolder and code actions |
| `al.nugetFeeds` | Supported | `al.symbols.nugetFeeds` | Custom NuGet feeds for symbol download |
| `al.outputAnalyzerStatistics` | Supported | `al.semantic.outputAnalyzerStatistics` | Expose analyzer perf stats |
| `al.packageCachePath` | Supported | `al.symbols.packageCachePath` | Overrides .alpackages directory |
| `al.profilerColors` | Planned | `al.profiler.colors` | Profiler color palette mapping |
| `al.publisher` | Supported | `al.project.publisher` | Default publisher for scaffolding |
| `al.rootNamespace` | Supported | `al.project.rootNamespace` | Default root namespace for scaffolding |
| `al.ruleSetPath` | Supported | `al.semantic.ruleSetPath` | Ruleset file path (local only) |
| `al.semanticFolding.enabled` | Supported | `al.features.semanticFolding` | Toggle LSP-provided folding |
| `al.showExplorerAtStartup` | Not supported | `` | Zed tasks control Explorer startup |
| `al.showHomeAtStartup` | Not supported | `` | No AL Home UI in Zed |
| `al.snapshotDebuggerLinesHitDecoration` | Planned | `al.snapshot.decoration` | Decoration for snapshot hit lines |
| `al.snapshotDebuggingPath` | Planned | `al.snapshot.path` | Snapshot debugging sources path |
| `al.snapshotOutputPath` | Planned | `al.snapshot.outputPath` | Snapshot output directory |
| `al.statementLensMin` | Planned | `al.profiler.statementLensMin` | Threshold for statement lenses |
| `al.symbolsCountryRegion` | Supported | `al.symbols.countryRegion` | Localized symbols download |
| `al.testCoverageCachePath` | Planned | `al.tests.coverageCachePath` | Cache location for test coverage |
| `al.useInteractiveLogin` | Planned | `al.auth.useInteractiveLogin` | Interactive login flow for auth |
| `al.useLegacyRuntime` | Not supported | `` | Only modern .NET runtime supported |
| `al.useOnlyCustomFeeds` | Supported | `al.symbols.useOnlyCustomFeeds` | Disable built-in feeds |
| `al.useVsCodeAuthentication` | Not supported | `` | VS Code auth not applicable in Zed |
| `al.vsCodeAuthenticationProvider` | Not supported | `` | VS Code auth not applicable in Zed |

## Zed Settings Schema
- These keys live under `settings.json` and are passed via LSP initialization and workspace configuration.
- `al.*` keys are interpreted by `al-core` and the CLI/TUI where relevant.