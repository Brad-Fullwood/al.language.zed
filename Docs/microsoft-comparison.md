# Microsoft Comparison

This page compares AL Language for Zed with Microsoft's AL extension and related tools: `alc`, the
.NET AL Language Server, `EditorServices.Host`, and Business Central development services. See the
feature pages for implementation details and limitations.

## Legend

- ✅ native here
- 🟡 partial or phase-gated here
- ❌ not provided
- 🔷 Microsoft is the authority, and this project delegates to it

## Editing & language server

| Capability | This project | Microsoft |
| --- | --- | --- |
| Editor | Zed (+ CLI, CI, AI) | VS Code |
| Parser | tree-sitter AL (native, incremental, error-resilient) | TextMate (highlight) + compiler (structure) |
| Hover / completion / definition / references / rename | ✅ native (bridge fallback for semantics) | ✅ (compiler-backed) |
| Document/workspace symbols, folding, semantic tokens, inlay hints, signature help, CodeLens | ✅ native | ✅ |
| Formatting | ✅ native (all documented native options wired) | ✅ (different/broader compatibility surface) |
| Diagnostics | ✅ syntax native + 🔷 CodeAnalysis bridge | 🔷 CodeAnalysis |
| Code actions / refactorings | ✅ curated native set | ✅ full compiler code-fix catalog |
| Complexity metrics | ✅ | ❌ |
| Replace al-lsp with Microsoft's altool LSP | 🟡 `al.useOfficialLsp` (hover, definition, references, rename, formatting, completion only) | n/a |

## Build, symbols, packaging

| Capability | This project | Microsoft |
| --- | --- | --- |
| Produce `.app` | 🟡 pure-Rust emitter, parity measured on fixtures only. On real projects it drops page extension variables, `ActionChanges`, `DocComments.xml`, option members, `CalcFormula` text and parameter `Temporary`, and encodes folder names with spaces differently. Use `alc` for packages that go to BC | 🔷 `alc` (parse→bind→type-check→emit) |
| Compile-time validation | 🟡 syntax, project, declaration, declared-binding and integrity checks block the build. Body checks without a binder (ALN2202, ALN2203, ALN2208, ALN2209, ALN2402, ALN2404) only warn. Optional `alc` compatibility gate | 🔷 `alc` (authoritative complete semantics) |
| `.app` reading / inspection | ✅ native NAVX/ZIP, cached, composed objects | internal |
| Symbol download | ✅ NuGet + BC server, concurrent, deduped, no restart | ✅ download-symbols |
| OAuth (Entra) | ✅ PKCE + device code, token zeroization | ✅ |
| Force Microsoft compiler | ✅ `al.useOfficialCompiler` | n/a |

## Analysis (largely unique to this project)

| Capability | This project | Microsoft |
| --- | --- | --- |
| Dead-code detection (with confidence) | ✅ | ❌ |
| Multi-hop event chain tracing | ✅ | ❌ |
| SQL anti-pattern scan | ✅ | ❌ (3rd-party analyzers) |
| Impact / table impact | ✅ detailed | partial (find-references) |
| Breaking-change / upgrade reports | ✅ with `--baseline-app <old.app>` | ❌ |
| Architecture lint (`.alarch.json`) | ✅ | ❌ |
| Obsolescence timeline | ✅ | ❌ |
| Data-classification audit | ✅ | ❌ |
| Dependency graph + conflict detection | ✅ (DOT) | ❌ |
| Profiler hotspot → source mapping | ✅ | partial |
| Project-wide bulk property fixes | ✅ | partial (per-file) |
| Entry-point discovery, suggest-event | ✅ | ❌ |

## Testing

| Capability | This project | Microsoft |
| --- | --- | --- |
| Pure-logic tests **without a BC server** | ✅ native interpreter | ❌ (all tests need BC) |
| Supported workspace-record tests **without a BC server** | ✅ isolated native record runtime | ❌ (all tests need BC) |
| Test discovery (static, BC-free) | ✅ | needs toolchain |
| Routing transparency (`test-classify`) | ✅ | ❌ |
| JUnit output / coverage | ✅ (static call-graph plus opt-in dynamic statement/path/MC/DC coverage) | partial (needs BC) |
| Mutation testing | ✅ interpreter-routed, optionally parallel. Runs the unmutated suite first and refuses (no score, nonzero exit) when a test fails. Skips `Subtype = Test` codeunits unless `--files` names them | ❌ |
| Test snapshot files | live capture/replay plus BC-free validation and file diff | partial (snapshot debugging) |
| Base-app/package DB, unsupported record, HTTP/UI/report/platform tests | 🔷 classified `liveBc` and reported as skipped. Running them needs a TestRunnerHub client, not built yet (`testing-01-live-bc-dev-tests-endpoint-invented.md`) | 🔷 live BC |

## Debugging

| Capability | This project | Microsoft |
| --- | --- | --- |
| Debug adapter | ✅ native Rust DAP (direct SignalR/REST) | 🔷 `EditorServices.Host` |
| Launch / attach / breakpoints (conditional) / step / stack / scopes / variables / evaluate | ✅ | ✅ |
| Pause-while-running | ❌ (BC limitation) | ❌ (BC limitation) |
| Profiling | ✅ start/stop/analyze | ✅ |
| Snapshot debugging | ✅ capture in Zed, CLI and MCP, replay in Zed (see [snapshot-debugging](./features/snapshot-debugging.md)). `WebServiceClient` capture failed on the test Sandbox | ✅ |
| BC runtime execution | 🔷 BC server | 🔷 BC server |
| Delegate to Microsoft adapter | ✅ `al.useOfficialDap` | n/a |

## Surfaces & automation

| Capability | This project | Microsoft |
| --- | --- | --- |
| Scriptable CLI (`--json`) for exposed query/analysis commands, complete daemon catalog through MCP `al_call` | ✅ | ❌ |
| Interactive terminal TUI | ✅ (5 views) | ❌ |
| Shared daemon (JSON-RPC) | ✅ | ❌ |
| MCP server | ✅ complete shared dispatcher via `al_call`, plus named aliases | ✅ AL agent tools |
| Project/object scaffolding | ✅ (+ Copilot and API templates, an `agent` starter without the Agent API) | ✅ (fewer templates) |
| Permission-set generation | ✅ (AL + XML) | ✅ (`AL: Generate permission set containing current extension objects`, AL and XML) |
| XLIFF generate/refresh/untranslated/suggest | ✅ `.g.xlf` ids and `al-object-target` match `alc` 18.1 for `TranslationFile` and `GenerateCaptions`. Translation-memory and symbol-name suggestions | partial (3rd-party common) |

## Where Microsoft is still the authority (by design)

The project does not replace these. The Microsoft path is one setting away:

- **Compile-time semantic validation & analyzer behavior:** `alc` + CodeAnalysis
  (`al.useOfficialCompiler`, and the semantic bridge for editor diagnostics).
- **Authoritative AL runtime semantics:** the Business Central server executes AL, and publish/runtime is
  the final compatibility validator for emitted `.app`s and for record/DB/HTTP/UI/report tests.
- **Microsoft's altool LSP:** `al.useOfficialLsp` replaces the al-lsp process with `altool launchlspserver`. It is not the server VS Code runs and it lacks diagnostics, code actions, code lens and inlay hints. See [language-server](./features/language-server.md#microsoft-comparison).
- **Official debug adapter:** `al.useOfficialDap` uses `EditorServices.Host`.

## Summary

The native implementation covers editing, navigation, analysis, verified package creation,
pure-logic test execution, and the Zed debug-adapter protocol. Exact Microsoft compiler semantics,
analyzer compatibility, and Business Central runtime behavior remain explicit integration points.
