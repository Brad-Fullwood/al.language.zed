# Feature Parity Blueprint (Cursor AL Extension -> Zed AL Extension)

This document finalizes parity decisions and mappings. There are no open decisions left.

## Parity Scope
1. Language grammar and syntax highlighting parity.
2. Snippet parity for all Cursor AL snippets.
3. Task and command parity where feasible in Zed.
4. Debug adapter parity for common launch.json workflows.
5. Settings parity as defined in `docs/settings.md`.

## Command Mapping
| Cursor Command | Title | Status | Zed Equivalent | Notes |
| --- | --- | --- | --- | --- |
| `al.go` | Go! | Supported | Zed task: AL: Go (scaffold) | Implements project scaffolding |
| `al.newproject` | New Project | Supported | Zed task: AL: New Project | Alias of AL: Go |
| `al.package` | Package | Supported | al cli: compile | Compile current project |
| `al.fullPackage` | Package full dependency tree for active project | Supported | al cli: compile --deps | Compile full dependency tree |
| `al.publish` | Publish with debugging | Supported | Zed task: AL: Publish (Debug) | Deploy with debugging |
| `al.publishNoDebug` | Publish without debugging | Supported | Zed task: AL: Publish | Deploy without debugging |
| `al.incrementalPublish` | Rapid Application Publish with debugging | Supported | Zed task: AL: RAD Publish (Debug) | Rapid app dev publish |
| `al.incrementalPublishNoDebug` | Rapid Application Publish without debugging | Supported | Zed task: AL: RAD Publish | Rapid app dev publish no debug |
| `al.onlyDebug` | Debug without publishing | Supported | Zed task: AL: Debug | Debug without publishing |
| `al.initalizeSnapshotDebugging` | Initialize snapshot debugging | Planned | Zed task: AL: Init Snapshot Debug | Snapshot debugging init |
| `al.finishSnapshotDebugging` | Finish snapshot debugging on the server | Planned | Zed task: AL: Finish Snapshot Debug | Snapshot debugging finalize |
| `al.snapshots` | Show all snapshots | Planned | al cli: snapshots | List snapshots |
| `al.generateCpuProfileFile` | Generate profile file | Planned | al cli: profiler --cpu | Generate profiler output |
| `al.clearCredentialsCache` | Clear credentials cache | Not supported |  | Credentials cache not managed by Zed |
| `al.generateManifest` | Generate manifest | Supported | al cli: generate-manifest | Generate app.json or manifest |
| `al.downloadSymbols` | Download symbols | Supported | Zed task: AL: Download Symbols | Uses al core symbols fetch |
| `al.downloadSymbolsFromGlobalSources` | Download symbols from global sources | Supported | Zed task: AL: Download Symbols (Global) | Uses NuGet feeds only |
| `al.downloadSource` | Download source code | Supported | al cli: download-source | Download source from server |
| `al.generatePermissionSetForExtensionObjects` | Generate permission set as AL object containing current extension objects | Supported | al cli: permissionset --al | Generate permission set object |
| `al.generatePermissionSetForExtensionObjectsAsXml` | Generate permission set as XML file containing current extension objects | Supported | al cli: permissionset --xml | Generate permission set XML |
| `al.openPageDesigner` | Publish and open in the designer | Not supported |  | Page Designer UI not supported |
| `al.openExternally` | Open Externally | Not supported |  | Not applicable in Zed |
| `al.openEventRecorder` | Open Events Recorder | Supported | Zed task: AL: Open Event Recorder | Opens Event Recorder from launch config |
| `al.insertEvent` | Find Event | Supported | Insight: Find Event | Uses Insight event search |
| `al.publishExistingExtension` | Publish extension without building | Supported | Zed task: AL: Publish Existing | Publish existing app without build |
| `al.explorer` | Explorer | Supported | Zed task: AL: Open Explorer | Opens al-explorer |
| `al.home` | Home | Not supported |  | No AL Home UI in Zed |
| `al.clearProfileCodeLenses` | Clear Profile Code Lenses | Planned | al cli: profiler --clear | Clear profile lenses |
| `al.fullDependencyPublish` | Publish full dependency tree for active project | Supported | Zed task: AL: Publish Full Dependency Tree | Publish all dependencies |
| `al.explorer_refresh` | Refresh active AL Explorer tab | Supported | Explorer action: Refresh | Refresh Explorer view |
| `al.explorer_reset_layout` | Reset settings for AL Explorer | Supported | Explorer action: Reset Layout | Reset Explorer layout |
| `al.tests.refreshAll` | Refresh all tests | Planned | al cli: tests --refresh | Refresh test discovery |

## Snippet Parity
1. All 23 Cursor snippet files will be imported into `snippets/`.
2. `extension.toml` will reference all snippet files.
3. Snippet prefixes will match Cursor where possible, with Zed-friendly defaults.

## Settings Parity
1. Full mapping is defined in `docs/settings.md`.
2. Unsupported settings are explicitly marked as such with reasons.

## Debugger Parity
1. `debug_adapter_schemas/al.json` will include all common launch.json attributes.
2. `al-core::launch` will parse those attributes and `al-lsp --dap` will pass them through.

## Acceptance Criteria
1. All supported commands have Zed task or CLI equivalents.
2. All snippets load correctly in Zed.
3. Language config matches Cursor behavior where possible.
4. Debugging tasks can run with standard launch.json configs.

## Community Extension Features (Beyond MS Official)

These features are found in popular community extensions and should be matched or exceeded.

### From AZ AL Dev Tools (anzwdev/al-code-outline)
| Feature | Status | Zed Equivalent | Notes |
| --- | --- | --- | --- |
| Symbol browser (list/tree) | Supported | al-explorer + al cli search | Our Rust index is faster |
| AL object wizards (table→page/report) | Planned | al cli: generate | Generate objects from table definitions |
| Code cleanup/sorting (fields, procedures, variables, properties) | Planned | al cli: sort / code action | Configurable sort actions |
| Application area management | Planned | Code action: Add/Remove ApplicationArea | Batch support via CLI |
| Tooltip generation from dependencies | Planned | Code action: Generate Tooltip | Reuse tooltips from base app symbols |
| FlowField read-only enforcement | Planned | Lint rule | Auto-fix via code action |
| Documentation comments (XML) | Planned | Code action: Generate XML Doc | Extract param names from procedure signature |
| Duplicate code detection | Planned | al cli: duplicates | Cross-file duplicate finder |
| Warning directives panel | Planned | al cli: pragmas | List all pragma suppressions in project |
| Action images browser | Planned | al cli: images / Explorer tab | Discover available UI graphics |
| Multiple build configurations | Planned | al cli: config --profile | Status bar switching in al-explorer |
| .app symbol file viewer | Supported | al cli: object / al-explorer | Already have full .app parsing |

### From NAB AL Tools
| Feature | Status | Zed Equivalent | Notes |
| --- | --- | --- | --- |
| XLF translation refresh | Planned | al cli: xlf refresh | Iterate g.xlf, update language files |
| Translation matching from base app | Planned | al cli: xlf suggest | Match against symbol tooltips/captions |
| Find untranslated texts | Planned | al cli: xlf untranslated | List all missing translations |
| Permission set generation | Supported | al cli: permissionset | Already implemented |
| Object renumbering | Planned | al cli: renumber --range | Renumber within ID ranges |
| Markdown documentation generation | Planned | al cli: docs | Generate from XML comments |

### From AL Test Runner
| Feature | Status | Zed Equivalent | Notes |
| --- | --- | --- | --- |
| Test discovery from [Test] attribute | Planned | runnables.scm + al cli: tests | Static discovery, no BC instance needed |
| Test-to-code coverage mapping | Planned | al cli: test-coverage | Which tests call which procedures |
| Run test from gutter | Planned | runnables.scm + Zed task | Requires BC Docker container |
| Code Lens for test coverage | Planned | Inlay hints | Show coverage info inline |

### From ALCops / LinterCop
| Feature | Status | Zed Equivalent | Notes |
| --- | --- | --- | --- |
| 94+ lint rules | Partial | al-syntax lint rules + al-semantic bridge | Implement highest-value rules natively |
| Cyclomatic/cognitive complexity | Planned | al cli: metrics | Project-wide complexity dashboard |
| Unused procedure detection | Planned | al cli: dead-code | Cross-file dead code analysis |
| Fix suggestions with auto-apply | Planned | Code actions + al cli: fix | Already have fix infrastructure |
| MCP server for AI integration | Supported | al-mcp / context server | Our CLI+MCP is more comprehensive |

## Beyond Parity: Unique Features

Features that no existing AL tool provides well. These are our competitive differentiation.

### 1. Insight Tooling
Defined in `docs/insight-tools.md`. Event graph, call trace, table relations, entry point finder.

### 2. Dead Code Detection
Cross-file analysis: unused procedures, unreachable triggers, unused table fields (never referenced in any page/report), unused event subscribers (publisher removed), unused permission sets.
- **Implementation**: Reverse-reference graph from symbol index. Find nodes with zero inbound edges.
- **CLI**: `al dead-code [--json]`

### 3. Dependency Impact Analysis
"If I change this procedure signature, which extensions break?" Query symbol index across all .app packages to show downstream consumers.
- **Implementation**: Dependency graph from app.json + symbol references. Trace consumers across dependency tree.
- **CLI**: `al impact <symbol> [--json]`

### 4. AI-Assisted Event Wiring
Given a business scenario (e.g., "validate customer credit limit on sales order"), suggest the correct event publisher, subscriber pattern, and integration event chain.
- **Implementation**: Index all event publishers from base app symbols. Map business concepts to event chains via keyword matching + object context.
- **CLI**: `al suggest-event <description> [--json]`
- **Slash command**: `/al-events` with natural language argument

### 5. Automated Permission Set Generation from Usage Analysis
Analyze all table/page/report/codeunit accesses in an extension and auto-generate the minimal permission set. Update when code changes.
- **Implementation**: Walk AST for record operations (Insert, Modify, Delete, FindSet, Get) and RunObject references. Generate RIMD permissions.
- **CLI**: `al permissionset --analyze [--json]` (already have basic `permissionset`)

### 6. Code Complexity Metrics Dashboard
Project-wide complexity: hotspot detection, procedure-level rankings, per-file summaries.
- **Implementation**: AST-based cyclomatic/cognitive complexity computation in al-syntax.
- **CLI**: `al metrics [file] [--json]`

### 7. Offline Test Discovery & Scaffolding
Static analysis of test codeunits without requiring a BC instance. Discover [Test] procedures, map test-to-code coverage, generate test scaffolds for untested procedures.
- **Implementation**: Parse [Test] attributes, build call graphs from syntax, identify untested public procedures.
- **CLI**: `al tests [--scaffold] [--coverage-map] [--json]`

### 8. Smart Rename with Translation Awareness
Rename a field and automatically update: caption, tooltip, XLIFF translation keys, all code references.
- **Implementation**: Combine rename provider with XLIFF file parsing and caption/tooltip property detection.
- **LSP**: Enhanced `textDocument/rename` that includes XLIFF changes in the workspace edit.

### 9. Architectural Linting
Enforce project-specific patterns like "no direct table access from pages" or "all external calls through facade codeunit." Rules defined in a `.alarch.json` config file.
- **Implementation**: Custom rule engine in al-core checking cross-object access patterns.
- **CLI**: `al lint --arch [--json]`

### 10. Cross-Extension Event Trace Visualization
Given a starting point (e.g., "Post Sales Order"), trace the complete event chain across ALL installed extensions, showing execution sequence.
- **Implementation**: Combine event publisher/subscriber symbol index with call graph analysis across packages.
- **CLI**: `al trace --cross-extension <entry-point> [--json]`

### 11. Zed AI Integration
Leverage Zed's extension API for deep AI integration:
- **Slash commands**: `/al-symbols`, `/al-events`, `/al-trace`, `/al-deps`, `/al-lint` inject structured context
- **Indexed docs**: Feed BC symbol documentation to Zed's AI via `index_docs` provider
- **Context server**: Register al-mcp as MCP context server in Zed's Agent Panel
- Schemas defined in `docs/agentic-schemas.md`

## Grammar Updates Required

BC26/27 introduced syntax changes requiring tree-sitter-al grammar updates:

| Feature | Grammar Impact | Priority |
| --- | --- | --- |
| `continue` keyword | New statement type inside loop bodies | High |
| `@'...'` multiline strings | New string literal prefix syntax | High |
| `List of [Interface IFoo]` | Interface type params in generic collections | Medium |