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

## Beyond Parity
1. Insight tooling is defined in `docs/insight-tools.md`.