# AL Language for Zed — Documentation

This `/Docs` tree is the complete, code-grounded reference for **AL Language for Zed**: a
native Business Central AL toolchain for the [Zed](https://zed.dev) editor, written almost
entirely in Rust. It is *not* a syntax package with a thin language-server wrapper — it is a
language server, debug adapter, native `.app` compiler/emitter, symbol engine, graph-based
analysis engine, native test interpreter, CLI/TUI, and MCP (AI agent) server, all sharing one
core crate.

> **How this documentation was produced.** Every subsystem below was read directly from source
> (~150k lines of Rust across the workspace). Each feature page cites the files and functions
> that implement it, states honestly what is native vs. delegated to Microsoft, compares against
> Microsoft's official AL tooling, explains why this project's approach differs, documents how to
> use the feature from every surface (Zed, CLI, MCP, daemon), and lists the per-feature roadmap.

## Start here

| Document | What it covers |
| --- | --- |
| [00 — Overview & Philosophy](./00-overview.md) | What the project is, the native-first philosophy, the native-vs-Microsoft matrix |
| [01 — Architecture](./01-architecture.md) | Crates, binaries, runtime modes, the transport-boundary rule, data flow |
| [02 — Zed Extension Integration](./02-zed-extension.md) | Extension manifest, binary resolution, settings, tasks, schemas |
| [Microsoft Comparison](./microsoft-comparison.md) | Feature-by-feature comparison against the official AL extension |
| [Consolidated Roadmap](./roadmap.md) | Per-feature roadmap, cross-referenced to source |

## Feature reference

| Document | Subsystem |
| --- | --- |
| [Parsing & Syntax Engine](./features/parsing-and-syntax.md) | tree-sitter AL parser, tokens, navigation, folding, formatting, complexity |
| [Language Server (LSP)](./features/language-server.md) | hover, completion, definition, references, rename, symbols, semantic tokens, inlay hints, CodeLens, signature help, diagnostics, formatting |
| [Code Actions & Refactorings](./features/code-actions.md) | quick fixes and source actions |
| [Symbol & Package Engine](./features/symbol-and-package-engine.md) | `.app` reading, symbol index, caching, composition, NuGet/server download, OAuth |
| [Native `.app` Emitter & Build](./features/native-app-emitter.md) | pure-Rust `.app` compiler, method-id hashing, build/publish, benchmarks |
| [Semantic Bridge](./features/semantic-bridge.md) | the .NET CodeAnalysis bridge for compiler-grade diagnostics |
| [Analysis & Insight Engine](./features/analysis-and-insight.md) | impact, events, call graph, dead code, SQL scan, arch lint, breaking/upgrade/obsolete, audits, deps, profiler hints, duplicates, bulk fixes |
| [Native Test Runtime](./features/native-test-runtime.md) | the pure-logic AL interpreter, routing, mutation testing, coverage, snapshots |
| [Debugging (DAP) & BC Runtime](./features/debugging-dap.md) | native debug adapter, SignalR/REST integration, profiling, snapshots |
| [AI & MCP Server](./features/ai-mcp.md) | the `al-tools` MCP context server and its tools |
| [CLI & TUI (`al-explorer`)](./features/cli-and-tui.md) | the terminal companion: command surface and interactive views |
| [Daemon Protocol](./features/daemon-protocol.md) | the shared JSON-RPC backend over Unix sockets |
| [XLIFF & Translation](./features/xliff-translation.md) | generate/refresh/untranslated/suggest workflows |
| [Scaffolding & Code Generation](./features/scaffolding-and-codegen.md) | new projects, object generators, permission sets, completion data |
| [Language Assets & Schemas](./features/language-assets.md) | tree-sitter queries, snippets, themes, JSON schemas |

## Reference tables

| Document | Contents |
| --- | --- |
| [Settings Reference](./reference/settings.md) | every `al.*` setting, type, default, and status |
| [CLI Command Reference](./reference/cli-commands.md) | every `al-explorer` subcommand |
| [LSP Command Reference](./reference/lsp-commands.md) | LSP methods, execute commands, CodeLens IDs |
| [MCP Tool Reference](./reference/mcp-tools.md) | every MCP tool and its schema |
| [Daemon Method Reference](./reference/daemon-methods.md) | every JSON-RPC method the daemon dispatches |

## Conventions used in these docs

- **Native** means implemented in this repository in Rust with no Microsoft binary at runtime.
- **Microsoft-backed / fallback** means the feature delegates to `alc`, the .NET CodeAnalysis
  bridge, the official AL LSP, or a live Business Central server.
- Code references use `path:line` form (clickable in editors that support it). Line numbers are
  accurate as of the documented commit and may drift as the code evolves; the function/struct
  names are the durable anchors.
- Status tags: ✅ shipped · 🟡 partial / phase-gated · ⛔ parsed-but-inert / not-yet-wired.

## Honesty contract

This project's `ROADMAP.md` states: *"every README/settings/schema claim must match current
code."* These docs follow the same rule. Where a feature is aspirational, scaffolded, or only
partially wired, it is marked 🟡 or ⛔ and the gap is described — not hidden. If you find a claim
here that the code contradicts, that is a documentation bug worth filing.
