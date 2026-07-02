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

The single source of truth for analysis logic is a set of layered library crates (`al-syntax`,
`al-symbols`, `al-semantic`, `al-analysis`, `al-insight`, `al-emit`, `al-compile`, `al-runtime`,
`al-workspace`, `al-project`, `al-bc`, `al-source`, `al-dap`, `al-publish` — see
[01-architecture](./01-architecture.md)), hosted by the `al-lsp` binary. Zed, CLI, MCP, daemon, and
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

## Platform support

The toolchain targets **Linux, macOS, and Windows**, but not every surface is
available on every platform. The split is structural: the long-lived **daemon**
and its only client, **`al-explorer`**, communicate over a Unix-domain socket
(`AF_UNIX`), which Zed/Business-Central toolchains have on Unix but which this
project does **not** wire up on Windows. The `al-lsp` language server, the MCP
server, the LSP/DAP transports, and the entire analysis engine are
platform-independent and run everywhere.

This is honest gating, not a Windows port: on Windows the daemon-backed surfaces
are compiled out (`#[cfg(unix)]`) and replaced with stubs that print a clear,
actionable message and exit non-zero, rather than panicking or failing
cryptically. A native Windows transport (e.g. named pipes or loopback TCP) is
tracked as future work — see gap **B16** in
[gaps-and-future-work.md](./gaps-and-future-work.md).

| Surface | Linux / macOS | Windows | Why |
| --- | --- | --- | --- |
| `al-lsp` language server (`--stdio`): parse, symbols, completions, hover, definitions, references, rename, **formatting**, **linting/diagnostics**, folding, semantic tokens, inlay hints, CodeLens, code actions | ✅ | ✅ | In-process; no socket. This is everything the Zed extension needs to edit AL. |
| MCP context server (`al-lsp mcp`) | ✅ | ✅ | Runs in-process over **stdio** with its own `Workspace`; does **not** use the daemon. |
| Native DAP debug adapter (`al-lsp --dap`) | ✅ | ✅\* | Portable transport; \*live debug still needs the Microsoft toolchain/BC runtime, which is orthogonal to the OS. |
| `al-lsp daemon` (long-lived shared backend) | ✅ | ❌ | Binds an `AF_UNIX` socket; the stub returns a clear "use `--stdio`/`--dap` instead" error. |
| `al-explorer` CLI + TUI (compile, package, download-symbols, analyses, test runner, profiler, object explorer, CLI `format`/`lint`, …) | ✅ | ❌ | Connects to the daemon over a Unix socket; on Windows `run()` prints the unsupported-platform message and exits non-zero. |
| Zed tasks (`languages/al/tasks.json`) | ✅ | ❌ | Every task shells out to `al-explorer`, so they inherit its Unix-only status. |

Two clarifications that the table can blur:

- **Formatting and linting work on Windows in the editor.** They are LSP
  features served directly by `al-lsp`, not by `al-explorer`. Only the
  `al-explorer format` / `al-explorer lint` *task wrappers* (and other tasks)
  are Unix-only; format-on-save and inline diagnostics come from the language
  server and are available everywhere.
- **MCP is portable.** Although AI-agent tooling is sometimes assumed to ride
  the daemon, `al-lsp mcp` is a self-contained stdio server. It is available on
  Windows wherever the `al-lsp` binary ships.

## Document map

Continue to [01 — Architecture](./01-architecture.md) for crates, binaries, and runtime modes, or
jump to any feature page from the [documentation index](./README.md).

- [architecture.md](./architecture.md) — Mermaid crate-dependency and request-flow
  diagrams, with every edge derived from the real `Cargo.toml` path-dependencies.
- [testing-guide.md](./testing-guide.md) — how to verify each layer (unit tests,
  the native `al-test-harness`, the GUI e2e harness, the env-gated `alc`/semantic
  tests, `make repro-artifacts`, and `make release-dryrun`) plus a minimal
  reproducible-report template.
