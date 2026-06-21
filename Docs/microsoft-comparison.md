# Microsoft Comparison

This page consolidates how **AL Language for Zed** compares to Microsoft's official **AL Language**
extension for VS Code (and the surrounding toolchain: `alc`, the .NET AL Language Server,
`EditorServices.Host`, and the BC dev services). Per-feature detail lives in the feature pages; this
is the at-a-glance map plus the honest "where Microsoft is still the authority."

## Legend

✅ native here · 🟡 partial / phase-gated here · ❌ not provided · 🔷 Microsoft-authoritative (we
delegate by design)

## Editing & language server

| Capability | This project | Microsoft |
| --- | --- | --- |
| Editor | Zed (+ CLI, CI, AI) | VS Code |
| Parser | tree-sitter AL (native, incremental, error-resilient) | TextMate (highlight) + compiler (structure) |
| Hover / completion / definition / references / rename | ✅ native (bridge fallback for semantics) | ✅ (compiler-backed) |
| Document/workspace symbols, folding, semantic tokens, inlay hints, signature help, CodeLens | ✅ native | ✅ |
| Formatting | ✅ native (partial option coverage) | ✅ (more complete) |
| Diagnostics | ✅ syntax native + 🔷 CodeAnalysis bridge | 🔷 CodeAnalysis |
| Code actions / refactorings | ✅ curated native set | ✅ full compiler code-fix catalog |
| Complexity metrics | ✅ | ❌ |
| Delegate to Microsoft LSP | ✅ `al.useOfficialLsp` | — |

## Build, symbols, packaging

| Capability | This project | Microsoft |
| --- | --- | --- |
| Produce `.app` | ✅ pure-Rust emitter, 10–12× faster cold / 60–465× warm, byte-identical `SymbolReference.json` on supported fixtures | 🔷 `alc` (parse→bind→type-check→emit) |
| Compile-time semantic validation | ❌ (delegated to LSP + BC server) | 🔷 `alc` (authoritative) |
| `.app` reading / inspection | ✅ native NAVX/ZIP, cached, composed objects | internal |
| Symbol download | ✅ NuGet + BC server, concurrent, deduped, no restart | ✅ download-symbols |
| OAuth (Entra) | ✅ PKCE + device code, token zeroization | ✅ |
| Force Microsoft compiler | ✅ `al.useOfficialCompiler` | — |

## Analysis (largely unique to this project)

| Capability | This project | Microsoft |
| --- | --- | --- |
| Dead-code detection (with confidence) | ✅ | ❌ |
| Multi-hop event chain tracing | ✅ | ❌ |
| SQL anti-pattern scan | ✅ | ❌ (3rd-party analyzers) |
| Impact / table impact | ✅ detailed | partial (find-references) |
| Breaking-change / upgrade reports | 🟡 (baseline not yet wired) | ❌ |
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
| Test discovery (static, BC-free) | ✅ | needs toolchain |
| Routing transparency (`test-classify`) | ✅ | ❌ |
| JUnit output / coverage | ✅ (coverage static) | partial (needs BC) |
| Mutation testing | 🟡 early | ❌ |
| Snapshot regression | 🟡 scaffolded | partial (snapshot debugging) |
| DB/HTTP/UI/report tests | 🔷 route to live BC | 🔷 live BC |

## Debugging

| Capability | This project | Microsoft |
| --- | --- | --- |
| Debug adapter | ✅ native Rust DAP (direct SignalR/REST) | 🔷 `EditorServices.Host` |
| Launch / attach / breakpoints (conditional) / step / stack / scopes / variables / evaluate | ✅ | ✅ |
| Pause-while-running | ❌ (BC limitation) | ❌ (BC limitation) |
| Profiling / snapshots | ✅ start/stop/analyze, download | ✅ |
| BC runtime execution | 🔷 BC server | 🔷 BC server |
| Delegate to Microsoft adapter | ✅ `al.useOfficialDap` | — |

## Surfaces & automation

| Capability | This project | Microsoft |
| --- | --- | --- |
| Scriptable CLI (`--json`) for every query/analysis | ✅ `al-explorer` | ❌ |
| Interactive terminal TUI | ✅ (5 views) | ❌ |
| Shared daemon (JSON-RPC) | ✅ | ❌ |
| MCP server for AI agents | ✅ `al-tools` (names mirror MS surface + extras) | ✅ AL agent tools |
| Project/object scaffolding | ✅ (+ Copilot/Agent/API templates) | ✅ (fewer templates) |
| Permission-set generation | ✅ (AL + XML) | ❌ |
| XLIFF generate/refresh/untranslated | ✅ (suggest is a stub) | partial (3rd-party common) |

## Where Microsoft is still the authority (by design)

The project does not pretend to replace these — it keeps the Microsoft path one setting away:

- **Compile-time semantic validation & analyzer behavior** — `alc` + CodeAnalysis
  (`al.useOfficialCompiler`, and the semantic bridge for editor diagnostics).
- **Authoritative AL runtime semantics** — the Business Central server executes AL; publish/runtime is
  the final compatibility validator for emitted `.app`s and for record/DB/HTTP/UI/report tests.
- **Official AL Language Server** — `al.useOfficialLsp` delegates the whole editor session.
- **Official debug adapter** — `al.useOfficialDap` uses `EditorServices.Host`.

## The one-paragraph summary

For the day-to-day loop — editing, navigating, analyzing, building an artifact, running pure-logic
tests, and debugging — this project is native, fast, editor-independent, scriptable, and exposes a
large set of analyses Microsoft doesn't ship. For the things that must be exactly Microsoft's
(compile-time semantics, runtime behavior), it delegates explicitly and keeps those paths tested.
That combination — native where it helps, Microsoft where it must be — is the whole thesis.
