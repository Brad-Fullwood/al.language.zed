# AL Language for Zed

AL Language for Zed is a native Business Central AL toolchain for Zed. It is not a syntax-highlighting package with a thin language-server wrapper. The repository contains a Rust language server, debug adapter, CLI/TUI, MCP server, symbol engine, query engine, test runner, and generated Zed language package for Microsoft Dynamics 365 Business Central AL development.

The guiding idea is simple: make the AL developer experience native, inspectable, scriptable, fast, and available from Zed, the terminal, CI, and AI agents. Microsoft tooling still matters, especially for compiler-correct builds and Business Central runtime behavior, but this project owns as much of the day-to-day editor and analysis stack as possible.

> Tester callout: this project is ready for serious testers across Zed editing, `al-lsp`, `al-explorer`, MCP, debugging, symbol downloads, and pure-logic test execution. Please test real Business Central projects, compare behavior against the official Microsoft tooling, and report exact commands, project shape, platform, expected result, actual result, and whether the issue is native-only or also reproduces through the Microsoft fallback. The roadmap in [ROADMAP.md](./ROADMAP.md) lists known gaps so testers can distinguish expected limitations from regressions.

## What Makes This Different

The standard Microsoft AL tooling is powerful, but most of it is coupled to the VS Code extension, the official language server, Business Central service assumptions, and opaque editor commands. That makes it hard to build deep Zed integration, hard to run narrow analysis from CI, hard to expose AL-aware tools to agents, and hard to test behavior outside the Microsoft extension boundary.

This project rewrites a large part of that experience in native Rust:

- AL parsing and Zed grammar integration are generated from AL language data and maintained in the bundled `tree-sitter-al` submodule.
- Workspace state is owned by `al-lsp`: file indexes, parse trees, symbol maps, package maps, document caches, insight graphs, call graphs, test discovery, and daemon command routing.
- `.app` symbol packages are read directly, with manifest parsing, `SymbolReference.json` extraction, virtual package navigation, and bounded archive safety checks. When packages embed `.al` source it is exposed as virtual source; otherwise navigation falls back to generated outlines from public symbol metadata.
- Symbol discovery is an in-memory indexed data model instead of repeated ad hoc package scans.
- Native `.app` compilation is implemented in Rust: project source is packaged into NAVX/ZIP `.app` artifacts with generated manifest data, source entries, XLIFF/navigation/control-add-in assets, and Microsoft-compatible `SymbolReference.json` method-id hashing.
- Business Central-specific workflows such as impact analysis, event tracing, subscriber lookup, dead-code detection, SQL anti-pattern detection, audit checks, and upgrade reports are exposed as commands, JSON-RPC, Zed tasks, and MCP tools.
- Batch test execution includes a native interpreter path for fully pure-logic test codeunits, with conservative routing back to live Business Central for mixed, record-touching, unknown, or platform-dependent codeunits.
- Zed can talk to the same engine through LSP, DAP, tasks, and the `al-tools` MCP context server.

The goal is full native implementation where that is realistic, with Microsoft compatibility and fallback where exact compiler or runtime behavior still belongs to Microsoft.

## Native Vs Microsoft-Backed

The project is intentionally honest about what is native today and what still delegates to Microsoft components.

| Area | Native in this repository | Microsoft-backed or fallback |
| --- | --- | --- |
| Editor language package | Zed language config, queries, snippets, themes, tasks, grammar metadata | None at runtime, apart from source data used by the generator |
| Parsing | Tree-sitter AL grammar and Rust syntax helpers | Microsoft TextMate grammar is used as generator input |
| Language server | `al-lsp` LSP transport, workspace indexing, document store, completions, hover, definitions, references, rename, formatting, folding, symbols, semantic tokens, inlay hints, CodeLens, code actions, diagnostics plumbing | Optional official AL LSP via `al.useOfficialLsp` |
| Semantic compiler checks | Native bridge host, daemon plumbing, caching, command surfaces | .NET AL CodeAnalysis bridge and Microsoft compiler semantics |
| Build/package | Pure-Rust `.app` emitter in `crates/al-core/src/emit`, native project packaging, deterministic artifact naming, native compile defaults for daemon `compile`, LSP `al.compile`, publish, and DAP launch, plus Rust-managed `dotnet alc` fallback plumbing | `al.useOfficialCompiler=true` opts into Microsoft `alc` for full compile-time semantic validation; daemon `package` remains the analyzer-backed Microsoft compiler surface |
| `.app` inspection and emit fidelity | Native NAVX/ZIP inspection, manifest parsing, generated `SymbolReference.json`, profile symbol references, XLIFF, navigation metadata, control-add-in bundles, entitlements, permissions, and ALC golden tests | Business Central publish/runtime behavior is still the final compatibility validator |
| Symbols | Native `.app` reader, symbol model, package cache, source map, composed objects, NuGet/server download orchestration | Symbol package contents and compiler output formats come from the Business Central ecosystem |
| Analysis | Native impact, event, call graph, dead code, SQL scan, duplicates, architecture lint, breaking/upgrade/obsolete/audit reports | Package-only call-site bodies cannot be recovered when Microsoft `.app` symbols do not contain source bodies |
| Tests | Native discovery, per-codeunit batch router, pure-logic interpreter, stubs, JUnit output, static Cobertura-shaped coverage output, early mutation testing for interpreter-routed tests | Single-codeunit `test-run` and all database, HTTP, UI, report, XmlPort, session, transaction, mixed, record-touching, and platform-dependent test execution use live BC today |
| Debugging | Native Zed DAP adapter mode, config conversion, compile/publish/deploy plumbing, SignalR and BC debug data mapping | Business Central runtime/debug service remains the execution backend |
| Automation | `al-explorer`, daemon JSON-RPC, Zed tasks, MCP server | None required for pure analysis; BC credentials/runtime required for publish/debug/live-test workflows |

Native coverage is expanding. The current design keeps Microsoft fallback paths because compatibility is more important than pretending every AL edge case has already been replaced.

## Why The Native Approach Matters

This section describes the architecture that avoids repeated work and makes the toolchain feel fast in real projects. It is not claiming published benchmark numbers against Microsoft's tools yet; benchmark-grade comparisons belong in the roadmap.

### Cached symbol discovery

Symbols are parsed once, cached, and indexed into concurrent in-memory maps. `SymbolIndex` stores shared `Arc` entries and maintains indexes by lowercase name, kind and id, kind, extension target, package path, source path, composed object, and all entries. That makes common queries direct map lookups rather than repeated package traversal.

Package symbols are loaded in parallel, validated against a disk cache under the user cache directory, and reused when the `.app` path-derived cache key, size, timestamp, and symbol-cache schema version still match. Semantic builtins and error-code caches are keyed separately by AL toolchain version. Warm starts avoid re-reading and re-parsing large `SymbolReference.json` payloads.

### Cached analysis queries

The daemon keeps a real workspace model, not just text open in the editor. `FileIndex` caches file text, object metadata, parse trees, document symbols, procedure definitions, event subscribers, and reverse indexes. `DocumentStore` keeps open-document text as shared strings and bounds parse-tree caching to avoid unbounded memory growth.

Queries such as definitions, references, composed objects, impact analysis, event tracing, and call graph traversal reuse those maps. Insight graphs and call graphs are cached and invalidated based on the kind of edit: body-only edits do not need the same rebuild path as object topology changes.

### Concurrent symbol acquisition

Symbol acquisition is native and parallel where it is safe:

- Existing packages are skipped instead of re-downloaded.
- NuGet downloads are deduplicated with per-package locks.
- NuGet downloads of different packages run concurrently behind a bounded semaphore.
- BC-server downloads are concurrent and payload-capped, but do not use the NuGet per-package lock/semaphore path.
- NuGet service-index metadata is cached.
- Package extraction is bounded by payload limits and archive-entry limits.
- Freshly downloaded symbols are loaded into the workspace without requiring a daemon restart.

### Less memory churn

The indexing path is built around shared ownership and bounded caches. Large file limits, archive payload limits, parse-tree LRU limits, lazy TUI hydration, and `Arc<String>` document text all exist to stop editor workflows from exploding into repeated copies or unbounded state. Default completion results also have a small cache for the common "blank completion at top level" path.

### Native Compile And Deterministic Build Loop

The default `.app` build path is now native Rust. Microsoft `alc` remains available as an explicit fallback for full compile-time semantic validation and analyzer behavior.

- The daemon `compile` dispatcher, LSP `al.compile`, publish path, and native DAP launch compile now default to the pure-Rust `.app` emitter: no `alc`, no C# bridge.
- `al.useOfficialCompiler=true` opts into Rust-managed `dotnet alc` where Microsoft compile-time validation is required.
- The native emitter builds the package directly from `app.json`, source files, package symbols, generated manifest data, and generated `SymbolReference.json`.
- The old Microsoft compiler subprocess path remains asynchronous, cancellable, timeout-aware, and killed on drop when used.
- Analyzer-backed package diagnostics and analyzer selection still belong to the Microsoft compiler path today.
- The Rust-managed `dotnet alc` fallback writes output to a per-invocation temporary directory and moves it into the project root when possible.
- `.app` selection prefers the manifest-derived `{publisher}_{name}_{version}.app` rather than arbitrary directory order.
- DAP deploy now reuses the same package-selection logic, so launch/deploy does not accidentally publish an older `.app` left in the project root.
- The emit test suite compares native output against ALC-shaped fixtures, including semantically-identical `SymbolReference.json` golden coverage (matching JSON values and method-id hashes, not raw zip-entry byte equality — zip timestamps make that comparison meaningless) for the supported project fixture.

The practical benefit is a safer and usually faster-feeling development loop: the editor and CLI keep using already-built indexes for most questions, and the compile path becomes a deterministic build step instead of the only way to understand the project.

## AI And Agent Workflows

AI integration is a first-class surface, not an afterthought. The extension registers a Zed context server named `AL Tools`, which launches `al-lsp mcp` from `PATH`. The MCP server speaks newline-delimited JSON-RPC over stdio and forwards tool calls into the same daemon dispatcher used by the CLI.

Current MCP tools:

- `al_build` - compile the project and return success, diagnostics, and `.app` path.
- `al_downloadsymbols` - download dependency symbol packages into `.alpackages`.
- `al_symbolsearch` - fuzzy-search symbols across workspace and packages.
- `al_getdiagnostics` - run diagnostics for an AL file.
- `al_runtests` - discover tests and delegate to batch routing: all-`Interp` codeunits run locally; everything else requires live BC configuration.
- `al_deadcode` - find unused procedures, fields, and orphaned subscribers.
- `al_sqlscan` - detect SQL anti-patterns.
- `al_entrypoints` - list procedures with no incoming calls.
- `al_trace_event` - trace publisher/subscriber event propagation.
- `al_impact` - answer "who consumes this symbol?"

The tool names intentionally mirror Microsoft's AL agent tool surface where possible, while adding analysis tools the official surface does not expose. CLI, MCP, and Zed tasks route through the daemon JSON-RPC dispatcher. Native LSP requests and execute commands use LSP server handlers, while sharing lower-level workspace, query, build, symbol, and test code.

## Native AL Test Runtime

Yes, this project includes a native AL language runner for tests.

The current local runner is a Phase-2 pure-logic interpreter for a supported subset of AL test bodies. It walks tree-sitter AL parse trees directly, without a .NET runtime and without a live Business Central server for the cases it supports, but cross-workspace procedure execution and platform semantics remain limited. The current interpreter handles:

- A defined subset of statement execution for blocks, `if`, `while`, `for`, `foreach`, `repeat`, `case`, assignment, expression statements, `exit`, and `asserterror`.
- A defined subset of expression evaluation for literals, identifiers, unary and binary operators, arithmetic, comparisons, boolean logic, string concatenation, and parenthesized expressions.
- A small set of built-ins and stubs such as `Error`, `Message`, `StrSubstNo`, `Format`, `StrLen`, `CopyStr`, `LowerCase`, `UpperCase`, `IndexOf`, `Library Assert`, `Library - Variable Storage`, `Library Random`, and `Any`.
- Static, BC-free test discovery. Codeunits are discovered by `Subtype = Test` or contained `[Test]` procedures, but only `[Test]` methods are returned as executable tests; lifecycle methods are not listed as tests.
- Optional JUnit XML and Cobertura-shaped static procedure coverage XML from batch runs. Coverage is static call-graph coverage, not dynamic line or branch coverage.
- Early interpreter-backed mutation testing with a limited mutator set. It only executes all-`Interp` discovered test codeunits; mutants without interpreter-runnable coverage are reported as survived.

The router is conservative by design, but it is currently pattern-based. Only tests classified as `Interp` in a codeunit where all discovered tests are `Interp` run locally in batch runs such as `test-run-all` and MCP `al_runtests`. `InterpRecord` is a classification for future/local record support; today those tests route to live BC in batch runs. Single-codeunit `test-run` currently uses live BC directly.

A standalone mock record runtime is under development, including in-memory record operations, filters, keys, and CalcFormula parsing. It is not yet wired into interpreter-backed test execution or FlowField evaluation. The aim is to keep moving tests from "requires BC" to "runs locally" as native platform coverage improves.

Snapshot test commands are exposed by the CLI, but live-BC record/replay is not fully wired yet: replay currently validates snapshot loading, and diff compares existing snapshot files.

## Specialized AL Workflows

The native engine enables workflows that are difficult to get from a generic editor action.

- Impact analysis: ask what consumes `Customer`, `Customer."Credit Limit"`, `Sales-Post.PostDocument`, or another object/member symbol.
- Table impact: see record variables, record parameters, table relations, extensions, and consuming objects grouped by table.
- Event source resolution: resolve the publisher behind an `EventSubscriber`.
- Subscriber discovery: find subscribers for an integration/business event.
- Event tracing: follow publisher to subscriber chains, including multi-hop propagation trees.
- Integration event suggestion: start from an object, procedure, table, or event and get candidate integration events to subscribe to, with ready-to-paste `[EventSubscriber]` examples and path breadcrumbs.
- Entrypoint discovery: identify procedures with no incoming calls from the workspace-enriched graph.
- Dead-code detection: report unused procedures, unreferenced fields, and orphaned subscribers with confidence levels.
- SQL scan: find patterns such as `FindFirst` in loops, `Get` in loops, `CalcFields` in loops, and unfiltered `FindSet`.
- Architecture lint: validate project-specific dependency rules from `.alarch.json`.
- Duplicate detection: find repeated AL code blocks.
- Breaking-change and upgrade analysis: compare public surfaces, obsolete metadata, permissions, and upgrade risk. Note: the baseline (a previous published version to diff against) is not yet wired, so `breaking` and `upgrade` currently run against an empty baseline and report no changes.
- Permission and data audits: inspect permission sets, table data classification, and missing metadata.
- XLIFF tooling: generate, refresh, inspect untranslated entries, and suggest translations from workspace symbols.
- Bulk fixes: add application areas, tooltips, data classification, organize files, and sort members.

These are not just README ideas. Surface coverage varies: the CLI and daemon expose the broadest set, Zed tasks expose common editor workflows, and MCP exposes a curated agent-facing subset. XLIFF refresh/untranslated/suggest are CLI/daemon workflows today; Zed currently exposes XLIFF generation, and MCP does not expose XLIFF tools.

## Command And Feature Surface

### Zed Integration

The extension manifest registers:

- The `AL` language package.
- The `al-lsp` language server.
- The `al-tools` MCP context server.
- The `al` debug adapter and debug locator.
- AL and JSON snippets.
- Business Central themes.
- The AL tree-sitter grammar revision used by Zed.

The Zed extension resolves `al-lsp` in this order:

1. `lsp.al-lsp.binary.path` from user settings.
2. A previously downloaded extension binary.
3. `al-lsp` on `PATH`.
4. The latest GitHub release asset for the current platform.

### LSP Features

The native language server covers diagnostics, hover, completion, definition, references, document symbols, workspace symbols, formatting, range formatting, folding, rename, semantic tokens, CodeLens, inlay hints, signature help, code actions, and pull diagnostics.

LSP execute commands include:

- `al.downloadSymbols`
- `al.downloadSymbolsServer`
- `al.downloadSymbolsNuget`
- `al.clearSymbolCache`
- `al.formatFile`
- `al.lintFile`
- `al.getStatus`
- `al.reindex`
- `al.compile`
- `al.applyRecommendedSettings`

Code actions include quick fixes plus source actions such as add doc comment, wrap in region, add using, convert `if` to `case`, eliminate `with`, make method local, implement interface stubs, add parentheses to bare calls, convert event subscriber literals, move `ToolTip` to table field, convert promoted actions to `actionRef`, set `ApplicationArea`, and fix report layout.

CodeLens currently emits lenses with these command IDs:

- `al.findReferences`
- `al.showProfiler`
- `al.runTest`

Those CodeLens IDs are separate from the native execute-command dispatcher above.

### Zed Tasks

`languages/al/tasks.json` exposes editor tasks for day-to-day project work:

- Build/package: compile and package.
- Debug: start, stop, initialize `.zed/debug.json`.
- Symbols/auth: server and NuGet symbol downloads plus Business Central authentication.
- Formatting/fixes: lint, format, quick fixes, sort members, organize file names.
- Workspace/project: doctor, setup, diagnostics, new project, generated permission set, object explorer, search, composed object view, package listing, dependency view, and cache clearing.
- Analysis: dead code, impact, entrypoints, suggest event, trace event, subscribers, event source, SQL scan, complexity metrics, profiler hints, duplicates, architecture lint, breaking changes, upgrade report, obsolete report.
- Audits/metadata: audit data classification, permission audit, add application area, add tooltips, add data classification.
- Translation: XLIFF generation.
- Tests: discover, run all, coverage, classify, result history, and mutation testing. Affected-test, snapshot, and single-codeunit run commands are CLI/daemon surfaces rather than Zed tasks today.

### `al-explorer` CLI/TUI

`al-explorer` is the terminal companion to `al-lsp`. In command mode it is a JSON-RPC client for the daemon and supports `--json` for scripts and CI. With no subcommand on Unix, it opens a TUI with object browsing, event chains, call graphs, profiler views, and test runner views.

The CLI command surface includes:

- Project/setup: `setup`, `doctor`, `diag`, `new`, `packages`, `deps`, `deps-graph`, `clear-cache`, `init-debug`.
- Build/toolchain: `compile`, `pack-native`, `package`, `download-symbols`, `authenticate`.
- LSP-style queries: `hover`, `definition`, `references`, `signature`, `completions`, `symbols`, `folding`, `tokens`, `parse`, `rename`, `hints`.
- Symbols and objects: `search`, `object`, `by-id`, `composed`, `builtins`, `rules`, `error-codes`, `generate-completions`, `version`.
- Events and insight: `events`, `subscribers`, `event-source`, `trace`, `intercept`, `entrypoints`, `graph`, `impact`, `suggest-event`, `insight-stats`.
- Analysis: `metrics`, `dead-code`, `sql-scan`, `duplicates`, `arch-lint`, `breaking`, `upgrade`, `obsolete`, `audit-data`, `permission-audit`, `profiler-hints`.
- Formatting/refactoring/codegen: `format`, `lint`, `fix`, `permissions`, `generate`, `add-application-area`, `add-tooltips`, `add-data-classification`, `sort-members`, `organize-files`.
- Debug/profiling: `debug`, `snapshot`, `profile`.
- Tests: `tests`, `test-run`, `test-run-all`, `test-coverage`, `test-mutate`, `test-affected`, `test-classify`, `test-snapshot`, `test-results`.
- Translation: `xlf`.

Nested command groups include `debug start|breakpoint|state|eval|continue|step|history|stop`, `snapshot start|list|download`, `profile start|stop|analyze`, `test-snapshot record|replay|diff`, and `xlf generate|refresh|untranslated|suggest`.

### Daemon Protocol

`al-lsp daemon` is the shared backend used by the CLI, MCP bridge, and Zed tasks. Native `al-lsp --stdio` LSP mode uses LSP handlers over the same workspace/query/build modules rather than the daemon dispatcher. `al-lsp --dap` is a separate stdio DAP mode with its own debug session plumbing.

## Symbol And Package Architecture

The symbol engine is one of the key reasons the project can support advanced AL queries.

- `.app` files are read natively as NAVX/ZIP packages.
- `NavxManifest.xml` and `SymbolReference.json` are parsed directly.
- `app_inspect` can list, classify, and safely extract package entries for IP and format auditing.
- Source files embedded in `.app` packages can be exposed as virtual source for navigation; packages without embedded source fall back to generated public-API outlines.
- Archive entries, manifest size, symbol size, and total package size are bounded.
- Parsed package data is cached on disk and validated before reuse.
- Package indexing is parallelized and loaded into shared in-memory indexes.
- Composed object views combine base objects and extensions for table/page-style workflows.
- NuGet package IDs are resolved from AL dependencies, including country-specific symbol packages where applicable.
- Symbol downloads are deduplicated, concurrent, bounded, and loaded without daemon restart.

This is why symbol search, completions, object lookup, event discovery, and impact analysis can run as regular editor/CLI queries rather than repeatedly invoking the compiler or crawling packages.

## Build, Package, And Debug Pipeline

`al-lsp` and `al-explorer` do not pretend the Microsoft AL compiler is irrelevant. Instead, they put a better native control plane around it.

- Toolchain discovery finds ALTool, `.NET`, compiler paths, bridge files, and project manifests.
- The daemon `compile` dispatcher, LSP `al.compile`, publish path, and native DAP launch compile default to the pure-Rust `.app` emitter.
- `al.useOfficialCompiler=true` opts into Rust-managed `dotnet alc`; daemon `package` remains the analyzer-backed Microsoft compiler surface.
- Compiler output from the Microsoft path is normalized into structured diagnostics.
- The package cache path is selected explicitly.
- Analyzer lists can be passed through command surfaces.
- The Rust-managed `dotnet alc` fallback uses deterministic temp output and final handoff when possible.
- Native `.app` emission is the default build path, with live Business Central publish/runtime validation still treated as the compatibility backstop.
- Launch/debug workflows compile on launch, locate the correct `.app`, publish/deploy where needed, and connect Zed to the BC debug backend through the native adapter.

For Zed users, this means the extension can provide first-class launch/attach workflows without being a VS Code extension clone.

## Generated Files And Repository Invariants

Several directories are generated or synchronized output. This matters because manual edits in the wrong place will be overwritten or will desynchronize Zed from the native parser.

- `tree-sitter-al/generator/tools/al-gen/src/main.rs` is the main generator.
- `tree-sitter-al/generator/tools/al-gen/src/zed_language.rs` and `tree-sitter-al/generator/tools/al-gen/templates/zed-language/` own the generated Zed language package.
- `tree-sitter-al/grammar.js`, `tree-sitter-al/src/parser.c`, `tree-sitter-al/src/scanner.c`, `tree-sitter-al/src/keywords.c`, and `tree-sitter-al/src/node-types.json` are generated grammar/parser artifacts.
- `tree-sitter-al/queries/*.scm` are generated query artifacts.
- `tree-sitter-al/data/*.json` contains committed language metadata consumed by `al-core`. Some files are generated by `al-gen`, some by `al-extract`, and some are static curated data.
- `languages/al/` is tracked generated output. Do not edit it by hand. Canonical parser queries are copied from `tree-sitter-al/queries`; Zed-specific config, tasks, runnables, overrides, injections, inline values, bracket rules, outline rules, and semantic token rules are generated from templates in `tree-sitter-al/generator`.
- `themes/bc-themes.json` is generated from Business Central VS Code theme data.

Release-critical invariants:

- `extension.toml` `[grammars.al].rev` must match the submodule commit recorded in the superproject gitlink for the release commit. A leading `+` in `git submodule status` is a release blocker.
- The root package version, `extension.toml` version, crate versions, and lockfile path-package versions should move together.
- `zed_extension_api` must stay pinned to a released crates.io API in committed release state.
- Release asset names in `src/lib.rs` must stay aligned with `.github/workflows/release.yml`.
- `languages/al` must be current against `al-gen --zed-language-only`; CI and release hygiene fail when those generated files drift.
- Dirty submodule changes must be committed inside `tree-sitter-al` and then the superproject pointer must be updated before tagging a reproducible release.

## Installation

Install the published extension from Zed's extension UI when using a release.

Auto-download release assets are published for Linux x86_64/aarch64, macOS x86_64/aarch64, and Windows x86_64. 32-bit x86 is not currently published.

GitHub release assets are binary/update artifacts:

- `al-linux-x86_64.tar.gz`
- `al-linux-aarch64.tar.gz`
- `al-macos-x86_64.tar.gz`
- `al-macos-aarch64.tar.gz`
- `al-windows-x86_64.zip`
- `extension.wasm`
- `extension.toml`
- `checksums.txt`

The Zed extension archive also includes tracked repository assets such as `languages/al`, `snippets/*.json`, and `themes/bc-themes.json`. These are not separate GitHub release assets; Zed packages them as part of the extension install archive.

Unix archives include `al-lsp`, `al-explorer`, and the semantic bridge files. The Windows archive ships `al-lsp.exe` and the semantic bridge; `al-explorer` is Unix-only because its daemon IPC uses Unix sockets.

Zed auto-resolves or downloads `al-lsp` for LSP and DAP. The MCP context server requires `al-lsp` on `PATH`. Zed tasks require `al-explorer` on `PATH` and are effectively Unix-only because `al-explorer` is not shipped or supported on Windows.

## Zed Settings

Minimal manual binary override:

```json
{
  "lsp": {
    "al-lsp": {
      "binary": {
        "path": "/path/to/al-lsp"
      }
    }
  }
}
```

Common AL settings:

```json
{
  "lsp": {
    "al-lsp": {
      "settings": {
        "al.enableCodeAnalysis": true,
        "al.diagnosticsScope": "project",
        "al.useOfficialLsp": false
      }
    }
  }
}
```

`al.useOfficialLsp` is the explicit escape hatch for delegating to Microsoft's official AL LSP. The default path is this project's native `al-lsp`. Custom `dotnet` path configuration is not currently supported; the toolchain invokes `dotnet` by name.

**Every setting - with types, defaults, and descriptions - is documented in [docs/settings.md](docs/settings.md), and a ready-to-copy, fully-commented template is at [examples/zed-settings.jsonc](examples/zed-settings.jsonc).** `al.enableNativeLint` and `al.nativeLintRules` are parsed for forward compatibility but currently inert; diagnostics come from syntax parsing and the semantic CodeAnalysis bridge.

On Zed Dev/Nightly (extension API >= 0.8) the `lsp.al-lsp.settings` keys autocomplete and validate as you type; on Stable Zed the settings still apply, just without in-editor autocomplete (use the template above). This lights up on Stable automatically once the 0.8 extension API reaches the registry.

### Project-file schemas (app.json, rulesets)

This extension ships JSON Schemas for the AL project files you edit by hand: `app.json`, `*.ruleset.json`, `AppSourceCop.json`, and `migration.json`. Associate them with Zed's bundled JSON language server (the `json.schemas` block in [examples/zed-settings.jsonc](examples/zed-settings.jsonc)) to get autocomplete and validation for those files on **every Zed channel today**. See [docs/settings.md](docs/settings.md#project-file-schemas-appjson-rulesets-) for the mapping.

## Debugging

The extension registers the `al` debug adapter and debug locator. Snippets cover common Business Central launch and attach configurations, including browser launch, tenant/environment settings, sandbox attach, agent-session fields, and MCP-related debug fields. Debug-config schemas are tracked in the repository for reference; debug configurations are authored via the bundled snippets rather than a registered settings-editor schema. (For language-server settings and project-file schema autocomplete, see [Zed Settings](#zed-settings) above and [docs/settings.md](docs/settings.md).)

Debug support has two important layers:

- Native Zed/DAP integration in `al-lsp --dap`.
- Business Central runtime/debug service integration for actual AL execution.

The native adapter currently implements the core launch/attach, publish, breakpoint, stepping, stack, scopes, variables, and evaluate flow against BC REST/SignalR. Not every schema/snippet field is consumed yet, especially agent/MCP-related debug fields. The BC runtime remains the source of truth for executing AL in a server environment.

## Development

Prerequisites:

- Rust stable.
- `wasm32-wasip1` target.
- .NET SDK 8.0.
- `git submodule update --init --recursive`.

Additional prerequisites for `make grammar`:

- `tree-sitter` CLI.
- An installed Microsoft AL extension containing `syntaxes/alsyntax.tmlanguage` in VS Code or Cursor extension directories.
- Microsoft CodeAnalysis DLL inputs when regenerating extracted built-in/runtime metadata through `al-extract`.

Common commands:

```bash
make build
make install
make install-lsp
make grammar
make language
scripts/check-repo-consistency.sh
scripts/check-release-hygiene.sh
```

`make grammar` regenerates tree-sitter artifacts from the generator and then runs `tree-sitter generate`. Treat the generated diff as part of the language package, not as hand-authored grammar code.
`make language` regenerates only `languages/al` from generator-owned query outputs and templates. It does not require the Microsoft AL extension or the tree-sitter CLI, so it is the fast path after changing Zed language tasks/config/query templates.
`scripts/check-repo-consistency.sh` checks repository slug drift only. `scripts/check-release-hygiene.sh` is the broader release gate for version alignment, tag/version alignment, submodule/rev alignment, required generated artifacts, generated-source/generated-output co-change, and optional exact-commit CI success.

Useful validation:

```bash
cargo fmt --all --check
cargo test --workspace --exclude zed-al
cargo test -p zed-al --target wasm32-wasip1
cargo test -p al-test-harness
scripts/check-repo-consistency.sh
scripts/check-release-hygiene.sh
make language
```

The semantic bridge is feature-gated. Release binaries build `al-lsp` with `--features semantic` so the in-process .NET bridge and `AlBridge.dll` are included.

## Release Process

Releases are tag-driven. Pushing a `v*` tag starts `.github/workflows/release.yml`, which first runs the release hygiene preflight, then builds native binaries for Linux, macOS, and Windows, builds the Zed WASM extension, generates checksums, and creates the GitHub release. The preflight blocks artifact creation unless the tag/version/submodule/generated-file invariants pass and CI has succeeded on the exact tagged commit.

Before tagging:

1. Start from a clean worktree except for the intentional release changes.
2. Commit any `tree-sitter-al` submodule changes inside the submodule.
3. Update the superproject submodule pointer.
4. Confirm `git submodule status` has no leading `+`.
5. Keep `extension.toml` grammar rev synchronized with the superproject gitlink.
6. Bump versions consistently across `extension.toml`, `Cargo.toml`, crates, and `Cargo.lock`.
7. Make the tag name match the package version, for example `v0.2.2` for version `0.2.2`.
8. Run repository consistency checks, release hygiene checks, and the relevant test suite.
9. Require green CI on the exact commit being tagged; the tag workflow enforces this before building artifacts.
10. Tag from the exact commit you want users to install.

## License

MIT.
