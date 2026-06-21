# 00 — Overview & Philosophy

## What this project is

**AL Language for Zed** is a native Business Central AL development toolchain for the Zed editor.
"AL" is Microsoft's domain-specific language for extending Dynamics 365 Business Central. The
canonical developer experience for AL is the official Microsoft **AL Language** extension for VS
Code, which couples editing, IntelliSense, compilation (`alc`), symbol management, debugging, and
test execution to the VS Code extension host and the .NET-based AL Language Server.

This project re-implements as much of that stack as is practical in **Rust**, exposing it through
five surfaces that all share one engine:

1. **Zed editor** — a language package + WASM extension that wires up LSP, DAP, tasks, snippets,
   themes, and an MCP context server.
2. **`al-lsp`** — the language server binary. It also hosts the daemon, the MCP server, the native
   debug adapter, and the official-LSP delegation mode.
3. **`al-explorer`** — a terminal companion: a JSON-RPC CLI for scripts/CI and an interactive TUI.
4. **MCP (`al-tools`)** — a Model Context Protocol server that exposes AL-aware tools to AI agents.
5. **Daemon** — a long-lived backend over a Unix socket that the CLI, MCP bridge, and Zed tasks
   all talk to.

The single source of truth for analysis logic is the `al-core` crate. Zed, CLI, MCP, daemon, and
LSP are thin transports over the same workspace, query, symbol, build, and test modules.

## The guiding idea

> Make the AL developer experience **native, inspectable, scriptable, fast, and available** from
> Zed, the terminal, CI, and AI agents.

Most of Microsoft's AL tooling is coupled to VS Code, the official language server, Business
Central service assumptions, and opaque editor commands. That makes it hard to:

- build deep editor integration outside VS Code,
- run a *narrow* analysis (e.g. "who calls this procedure?") from CI without spinning the compiler,
- expose AL-aware tools to AI agents, or
- test behavior outside the Microsoft extension boundary.

This project owns as much of the day-to-day editor and analysis stack as possible, while keeping
Microsoft components exactly where exact compiler or runtime behavior still belongs to Microsoft.

## Native-first, Microsoft-compatible

The project is deliberately honest about what is native today and what still delegates. The
`README.md` and `ROADMAP.md` enforce a rule that *every* user-facing claim must match the code.
These docs follow the same contract.

| Area | Native in this repo | Microsoft-backed / fallback |
| --- | --- | --- |
| Editor language package | Zed config, queries, snippets, themes, tasks, grammar metadata | Source data used by the generator only |
| Parsing | tree-sitter AL grammar + Rust syntax helpers | Microsoft TextMate grammar used as generator input |
| Language server | `al-lsp` transport, indexing, document store, completions, hover, definitions, references, rename, formatting, folding, symbols, semantic tokens, inlay hints, CodeLens, code actions, diagnostics plumbing | Optional official AL LSP via `al.useOfficialLsp` |
| Semantic compiler checks | Bridge host, daemon plumbing, caching, command surfaces | .NET AL CodeAnalysis bridge + Microsoft compiler semantics |
| Build / package | Pure-Rust `.app` emitter; native compile defaults for daemon `compile`, LSP `al.compile`, publish, DAP launch; Rust-managed `dotnet alc` fallback | `al.useOfficialCompiler=true` for Microsoft `alc`; daemon `package` is the analyzer-backed Microsoft surface |
| `.app` inspection & emit | Native NAVX/ZIP inspection, manifest parsing, generated `SymbolReference.json`, profile symbol refs, XLIFF, navigation, control add-ins, entitlements, permissions, ALC golden tests | BC publish/runtime remains the final compatibility validator |
| Symbols | Native `.app` reader, symbol model, package cache, source map, composed objects, NuGet/server download | Package contents & compiler output formats come from the BC ecosystem |
| Analysis | Native impact, event tracing, call graph, dead code, SQL scan, duplicates, arch lint, breaking/upgrade/obsolete/audit | Package-only call-site bodies can't be recovered when `.app` symbols carry no source |
| Tests | Native discovery, batch router, pure-logic interpreter, stubs, JUnit output, static Cobertura-shaped coverage, early mutation testing | Single-codeunit run + all DB/HTTP/UI/report/XmlPort/session/transaction/mixed/record/platform tests use live BC |
| Debugging | Native Zed DAP adapter, config conversion, compile/publish/deploy plumbing, SignalR + BC debug mapping | BC runtime/debug service is the execution backend |
| Automation | `al-explorer`, daemon JSON-RPC, Zed tasks, MCP server | BC credentials/runtime required for publish/debug/live-test |

See [microsoft-comparison.md](./microsoft-comparison.md) for the feature-by-feature comparison and
[roadmap.md](./roadmap.md) for where native coverage is expanding.

## Why the native approach matters

The architecture is built to avoid repeated work, which is what makes the toolchain feel fast on
real projects. (These are architectural claims grounded in the code, not published benchmark numbers
— except for the native emitter, which *is* benchmarked: see
[native-app-emitter.md](./features/native-app-emitter.md) and `BENCHMARKS.md`.)

- **Cached symbol discovery.** `.app` packages are parsed once, cached on disk (mtime/size/schema
  validated), and indexed into concurrent in-memory maps. Common queries are direct map lookups, not
  package traversals.
- **Cached analysis queries.** The daemon keeps a real workspace model (`FileIndex`, `DocumentStore`)
  — file text, object metadata, parse trees, document symbols, procedure defs, event subscribers,
  reverse indexes, insight/call graphs — invalidated by edit kind.
- **Concurrent symbol acquisition.** Downloads are deduplicated, bounded, concurrent, and loaded
  without a daemon restart.
- **Bounded memory.** Shared `Arc<String>` document text, LRU parse-tree caps, archive payload
  limits, and lazy TUI hydration keep editor workflows from exploding into repeated copies.
- **Deterministic native build loop.** The default `.app` build path is pure Rust; the compile path
  becomes a deterministic build step rather than the only way to understand a project.

## What "better" means here

This documentation makes "why ours is better" claims throughout. They reduce to a small set of
structural advantages that follow from the native-first design:

1. **Editor-independence.** The same engine serves Zed, a terminal, CI, and AI agents. Microsoft's
   stack is VS Code-first.
2. **Scriptability.** Every analysis is a CLI subcommand with `--json` output and a daemon JSON-RPC
   method. You can put "fail the build if dead code appears" in CI without launching the compiler.
3. **Analyses Microsoft does not ship.** Dead-code detection with confidence levels, multi-hop event
   chain tracing, SQL anti-pattern scanning, cross-version breaking-change/upgrade reports,
   architecture linting, obsolescence timelines, data-classification audits, and AI-facing tools are
   first-class here and largely absent from the official extension.
4. **Speed of the common path.** Warm symbol caches and a pure-Rust emitter make the everyday loop
   (edit → analyze → build) fast; the emitter is 10–12× faster cold and 60–465× faster warm than
   `alc` at producing an artifact (with the honest caveat that the native emitter does not perform
   `alc`'s semantic validation — that is the LSP's and the BC server's job).
5. **Honesty.** Fallbacks to Microsoft are explicit, documented, and tested rather than hidden.

Where Microsoft is genuinely better — exact compile-time semantic validation, analyzer behavior,
and authoritative runtime semantics — this project keeps the Microsoft path one setting away
(`al.useOfficialCompiler`, `al.useOfficialLsp`, `al.useOfficialDap`).

## Document map

Continue to [01 — Architecture](./01-architecture.md) for crates, binaries, and runtime modes, or
jump to any feature page from the [documentation index](./README.md).
