# Cursor AL Extension Audit

Generated: 2026-03-12T18:22:15

This summarizes the locally installed Cursor/VS Code AL extension to drive feature parity planning.

**Source**
- `/home/bradf/.cursor/extensions/ms-dynamics-smb.al-18.0.2190758/package.json`

## Language and Grammar
- Language id: `al` name: `AL` extension: `['.al', '.dal']` configuration: `./al.configuration.json`
- Grammar scope: `source.al` language: `al` path: `./syntaxes/alsyntax.tmlanguage`

## Snippets
- `./snippets/al.json` for language `al`
- `./snippets/codeunit.json` for language `al`
- `./snippets/interface.json` for language `al`
- `./snippets/controladdin.json` for language `al`
- `./snippets/dotnet.json` for language `al`
- `./snippets/enum.json` for language `al`
- `./snippets/enumextension.json` for language `al`
- `./snippets/page.json` for language `al`
- `./snippets/pageextension.json` for language `al`
- `./snippets/query.json` for language `al`
- `./snippets/report.json` for language `al`
- `./snippets/reportextension.json` for language `al`
- `./snippets/table.json` for language `al`
- `./snippets/tableextension.json` for language `al`
- `./snippets/xmlport.json` for language `al`
- `./snippets/pagecustomization.json` for language `al`
- `./snippets/permissionset.json` for language `al`
- `./snippets/permissionsetextension.json` for language `al`
- `./snippets/profile.json` for language `al`
- `./snippets/profileextension.json` for language `al`
- `./snippets/ruleset.json` for language `json`
- `./snippets/xml.json` for language `xml`
- `./snippets/entitlement.json` for language `al`

## Commands
- `al.go` — Go!
- `al.newproject` — New Project
- `al.package` — Package
- `al.fullPackage` — Package full dependency tree for active project
- `al.publish` — Publish with debugging
- `al.publishNoDebug` — Publish without debugging
- `al.incrementalPublish` — Rapid Application Publish with debugging
- `al.incrementalPublishNoDebug` — Rapid Application Publish without debugging
- `al.onlyDebug` — Debug without publishing
- `al.initalizeSnapshotDebugging` — Initialize snapshot debugging
- `al.finishSnapshotDebugging` — Finish snapshot debugging on the server
- `al.snapshots` — Show all snapshots
- `al.generateCpuProfileFile` — Generate profile file
- `al.clearCredentialsCache` — Clear credentials cache
- `al.generateManifest` — Generate manifest
- `al.downloadSymbols` — Download symbols
- `al.downloadSymbolsFromGlobalSources` — Download symbols from global sources
- `al.downloadSource` — Download source code
- `al.generatePermissionSetForExtensionObjects` — Generate permission set as AL object containing current extension objects
- `al.generatePermissionSetForExtensionObjectsAsXml` — Generate permission set as XML file containing current extension objects
- `al.openPageDesigner` — Publish and open in the designer
- `al.openExternally` — Open Externally
- `al.openEventRecorder` — Open Events Recorder
- `al.insertEvent` — Find Event
- `al.publishExistingExtension` — Publish extension without building
- `al.explorer` — Explorer
- `al.home` — Home
- `al.clearProfileCodeLenses` — Clear Profile Code Lenses
- `al.fullDependencyPublish` — Publish full dependency tree for active project
- `al.explorer_refresh` — Refresh active AL Explorer tab
- `al.explorer_reset_layout` — Reset settings for AL Explorer
- `al.tests.refreshAll` — Refresh all tests

## Configuration Keys (Top-Level)
- `al` (45 keys)

## Configuration Keys (Full List)
- `al.algoSuggestedFolder`
- `al.appLocalFolderPaths`
- `al.areProfileLensesSupported`
- `al.assemblyProbingPaths`
- `al.backgroundCodeAnalysis`
- `al.browser`
- `al.codeAnalyzers`
- `al.compilationOptions`
- `al.disableTestRunning`
- `al.editorServicesLogLevel`
- `al.editorServicesPath`
- `al.enableCodeActions`
- `al.enableCodeAnalysis`
- `al.enableExternalRulesets`
- `al.enableScriptIntelliSense`
- `al.entraIdAuthentication`
- `al.extendGoToSymbolInWorkspace.IncludeSymbolFiles`
- `al.extendGoToSymbolInWorkspace.ResultLimit`
- `al.extendGoToSymbolInWorkspace.enabled`
- `al.incognito`
- `al.incrementalBuild`
- `al.inlayhints.functionReturnTypes.enabled`
- `al.inlayhints.parameterNames.enabled`
- `al.namespaceTemplate`
- `al.nugetFeeds`
- `al.outputAnalyzerStatistics`
- `al.packageCachePath`
- `al.profilerColors`
- `al.publisher`
- `al.rootNamespace`
- `al.ruleSetPath`
- `al.semanticFolding.enabled`
- `al.showExplorerAtStartup`
- `al.showHomeAtStartup`
- `al.snapshotDebuggerLinesHitDecoration`
- `al.snapshotDebuggingPath`
- `al.snapshotOutputPath`
- `al.statementLensMin`
- `al.symbolsCountryRegion`
- `al.testCoverageCachePath`
- `al.useInteractiveLogin`
- `al.useLegacyRuntime`
- `al.useOnlyCustomFeeds`
- `al.useVsCodeAuthentication`
- `al.vsCodeAuthenticationProvider`

## Language Configuration (al.configuration.json)
- Path: `/home/bradf/.cursor/extensions/ms-dynamics-smb.al-18.0.2190758/al.configuration.json`
- Line comment: `//`
- Block comment: `['/*', '*/']`
- Bracket pairs: 6
- Auto-closing pairs: 29
- Surrounding pairs: 5
- Folding markers: `{'start': '^\\s*#?region\\b', 'end': '^\\s*#?endregion\\b'}`
- Word pattern: `("(?:(?:\"\")|[^\"])*")|(-?\d*\.\d\w*)|([^\`\~\!\@\#\%\^\&\*\(\)\-\=\+\[\{\]\}\\\|\;\:\'\"\,\.\<\>\/\?\s]+)`
- Semantic highlighting: `True`

## Syntax Files
- Directory: `/home/bradf/.cursor/extensions/ms-dynamics-smb.al-18.0.2190758/syntaxes`
- `AnalysisViewSyntax.json`
- `alsyntax.tmlanguage`
- `appPropsSyntax.json`
- `appSourceCopSyntax.json`
- `appSyntax.json`
- `migrationSyntax.json`
- `ruleSetSyntax.json`

## Semantic Token Types
- `{'id': 'builtintypes', 'description': 'An AL builtin type.'}`
- `{'id': 'builtinfunctions', 'description': 'An AL builtin function.'}`
- `{'id': 'otherkeyword', 'description': 'The other keywords.'}`
- `{'id': 'langaugeconstant', 'description': 'The language keywords.'}`
- `{'id': 'attribute', 'description': 'Attribute names.'}`
- `{'id': 'returnparameter', 'description': 'Named return parameter.'}`
- `{'id': 'pagecontrol', 'description': 'Name of a page control, such as a repeater.'}`
- `{'id': 'pageaction', 'description': 'Name of a page control, such as an action.'}`
- `{'id': 'pageview', 'description': 'Name of a page view.'}`
- `{'id': 'tablefield', 'description': 'Name of a table field.'}`
- `{'id': 'tablekey', 'description': 'Name of a table key.'}`
- `{'id': 'tablefieldgroup', 'description': 'Name of a table fieldgroup.'}`
- `{'id': 'reportlabel', 'description': 'Name of a label in a report.'}`
- `{'id': 'reportlayout', 'description': 'Name of a layout in a report.'}`
- `{'id': 'querydataitem', 'description': 'Name of a dataitem in a query.'}`
- `{'id': 'querycolumn', 'description': 'Name of a column in a query.'}`
- `{'id': 'queryfilter', 'description': 'Name of a filter in a query.'}`
- `{'id': 'xmlporttableelement', 'description': 'Name of a xmlport table element.'}`
- `{'id': 'xmlporttextelement', 'description': 'Name of a xmlport text element.'}`
- `{'id': 'xmlportfieldelement', 'description': 'Name of a xmlport field element.'}`
- `{'id': 'xmlportfieldattribute', 'description': 'Name of a xmlport field attribute.'}`
- `{'id': 'datetime', 'description': 'Datetime literal.'}`
- `{'id': 'preprocessorkeyword', 'description': 'Keywords used in preprocessor syntax.'}`
- `{'id': 'triggername', 'description': 'The name of triggers.'}`
- `{'id': 'namespace', 'description': 'Name of a namespace.'}`
- `{'id': 'excludedCode', 'description': 'A token that represents inactive code.'}`
- `{'id': 'xmlDocCommentAttributeName', 'description': 'A token that represents an attribute in an XML documentation comment'}`
- `{'id': 'xmlDocCommentAttributeQuotes', 'description': 'A token that represents an attribute quote in an XML documentation comment'}`
- `{'id': 'xmlDocCommentAttributeValue', 'description': 'A token that represents an attribute value in an XML documentation comment'}`
- `{'id': 'xmlDocCommentCDataSection', 'description': 'A token that represents a CDATA section in an XML documentation comment'}`
- `{'id': 'xmlDocCommentComment', 'description': 'A token that represents a comment in an XML documentation comment'}`
- `{'id': 'xmlDocCommentDelimiter', 'description': 'A token that represents a delimeter in an XML documentation comment'}`
- `{'id': 'xmlDocCommentEntityReference', 'description': 'A token that represents reference to an entity in an XML documentation comment'}`
- `{'id': 'xmlDocCommentName', 'description': 'A token that represents a name in an XML documentation comment'}`
- `{'id': 'xmlDocCommentProcessingInstruction', 'description': 'A token that represents a processing instruction in an XML documentation comment'}`
- `{'id': 'xmlDocCommentText', 'description': 'A token that represents text in an XML documentation comment'}`
- `{'id': 'xmlLiteralAttributeName', 'description': 'A token that represents an attribute name in an XML literal'}`
- `{'id': 'xmlLiteralAttributeQuotes', 'description': 'A token that represents an attribute quote in an XML literal'}`
- `{'id': 'globalVariable', 'description': 'A token that represents a global variable'}`
- `{'id': 'localVariable', 'description': 'A token that represents an local variable'}`
- `{'id': 'aleventcreation', 'description': 'A token that represents a integration or business event.'}`
- `{'id': 'aleventsubscription', 'description': 'A token that represents a event subscription.'}`

## Semantic Token Scopes
- `` -> aleventcreation, aleventsubscription, namespace, globalVariable, localVariable, interface, builtintypes, builtinfunctions, otherkeyword, langaugeconstant, attribute, returnparameter, pagecontrol, pageaction, pageview, tablefield, tablekey, tablefieldgroup, reportlabel, reportlayout, querydataitem, querycolumn, queryfilter, xmlporttableelement, xmlporttextelement, xmlportfieldelement, xmlportfieldattribute, datetime, preprocessorkeyword, triggername, excludedCode, xmlDocCommentAttributeName, xmlDocCommentAttributeQuotes, xmlDocCommentAttributeValue, xmlDocCommentCDataSection, xmlDocCommentComment, xmlDocCommentDelimiter, xmlDocCommentEntityReference, xmlDocCommentName, xmlDocCommentProcessingInstruction, xmlDocCommentText, xmlLiteralAttributeName, xmlLiteralAttributeQuotes