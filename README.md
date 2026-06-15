# AL Language for Zed

AL Language for Zed is a native Business Central AL toolchain for Zed. It is not a syntax-highlighting package with a thin language-server wrapper. The repository contains a Rust language server, debug adapter, CLI/TUI, MCP server, symbol engine, query engine, test runner, and generated Zed language package for Microsoft Dynamics 365 Business Central AL development.

The guiding idea is simple: make the AL developer experience native, inspectable, scriptable, fast, and available from Zed, the terminal, CI, and AI agents. Microsoft tooling still matters, especially for compiler-correct builds and Business Central runtime behavior, but this project owns as much of the day-to-day editor and analysis stack as possible.

## What Makes This Different

The standard Microsoft AL tooling is powerful, but most of it is coupled to the VS Code extension, the official language server, Business Central service assumptions, and opaque editor commands. That makes it hard to build deep Zed integration, hard to run narrow analysis from CI, hard to expose AL-aware tools to agents, and hard to test behavior outside the Microsoft extension boundary.

This project rewrites a large part of that experience in native Rust:

- AL parsing and Zed grammar integration are generated from AL language data and maintained in the bundled `tree-sitter-al` submodule.
- Workspace state is owned by `al-lsp`: file indexes, parse trees, symbol maps, package maps, document caches, insight graphs, call graphs, test discovery, and daemon command routing.
- `.app` symbol packages are read directly, with manifest parsing, `SymbolReference.json` extraction, source extraction, package source navigation, and bounded archive safety checks.
- Symbol discovery is an in-memory indexed data model instead of repeated ad hoc package scans.
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
| Build/package | Native orchestration, async/cancellable process handling, diagnostics mapping, analyzer selection, temp output, deterministic `.app` selection, atomic final `.app` handoff | `alc` remains the authoritative compiler when full AL compilation is required |
| Symbols | Native `.app` reader, symbol model, package cache, source map, composed objects, NuGet/server download orchestration | Symbol package contents and compiler output formats come from the Business Central ecosystem |
| Analysis | Native impact, event, call graph, dead code, SQL scan, duplicates, architecture lint, breaking/upgrade/obsolete/audit reports | Package-only call-site bodies cannot be recovered when Microsoft `.app` symbols do not contain source bodies |
| Tests | Native discovery, per-codeunit batch router, pure-logic interpreter, stubs, JUnit output, static Cobertura-shaped coverage output, early mutation testing for interpreter-routed tests | Single-codeunit `test-run` and all database, HTTP, UI, report, XmlPort, session, transaction, mixed, record-touching, and platform-dependent test execution use live BC today |
| Debugging | Native Zed DAP adapter mode, config conversion, compile/publish/deploy plumbing, SignalR and BC debug data mapping | Business Central runtime/debug service remains the execution backend |
| Automation | `al-explorer`, daemon JSON-RPC, Zed tasks, MCP server | None required for pure analysis; BC credentials/runtime required for publish/debug/live-test workflows |

Native coverage is expanding. The current design keeps Microsoft fallback paths because compatibility is more important than pretending every AL edge case has already been replaced.

## Why The Native Approach Is Better

### Faster symbol discovery

Symbols are parsed once, cached, and indexed into concurrent in-memory maps. `SymbolIndex` stores shared `Arc` entries and maintains indexes by lowercase name, kind and id, kind, extension target, package path, source path, composed object, and all entries. That makes common queries direct map lookups rather than repeated package traversal.

Package symbols are loaded in parallel, validated against a disk cache under the user cache directory, and reused when the `.app` path, size, timestamp, schema version, and toolchain version still match. Warm starts avoid re-reading and re-parsing large `SymbolReference.json` payloads.

### Faster analysis queries

The daemon keeps a real workspace model, not just text open in the editor. `FileIndex` caches file text, object metadata, parse trees, document symbols, procedure definitions, event subscribers, and reverse indexes. `DocumentStore` keeps open-document text as shared strings and bounds parse-tree caching to avoid unbounded memory growth.

Queries such as definitions, references, composed objects, impact analysis, event tracing, and call graph traversal reuse those maps. Insight graphs and call graphs are cached and invalidated based on the kind of edit: body-only edits do not need the same rebuild path as object topology changes.

### Faster symbol downloads

Symbol acquisition is native and parallel where it is safe:

- Existing packages are skipped instead of re-downloaded.
- Downloads of the same package are serialized behind a per-package lock.
- Downloads of different packages run concurrently behind a bounded semaphore.
- NuGet service-index metadata is cached.
- Package extraction is bounded by payload limits and archive-entry limits.
- Freshly downloaded symbols are loaded into the workspace without requiring a daemon restart.

### Less memory churn

The indexing path is built around shared ownership and bounded caches. Large file limits, archive payload limits, parse-tree LRU limits, lazy TUI hydration, and `Arc<String>` document text all exist to stop editor workflows from exploding into repeated copies or unbounded state. Default completion results also have a small cache for the common "blank completion at top level" path.

### Faster build loop

The final AL compiler is still Microsoft `alc` when a real `.app` compile is required, but the build loop around it has been redesigned:

- The daemon attempts the semantic bridge compile path when available and falls back to `dotnet alc` when it is not.
- Compiler subprocesses are asynchronous, cancellable, timeout-aware, and killed on drop.
- Analyzer selection and diagnostics mapping happen in native code.
- Output is written to a per-invocation temporary directory, then moved into the project root atomically when possible.
- `.app` selection prefers the manifest-derived `{publisher}_{name}_{version}.app` rather than arbitrary directory order.
- DAP deploy now reuses the same package-selection logic, so launch/deploy does not accidentally publish an older `.app` left in the project root.

The practical benefit is a faster, safer development loop: the editor and CLI keep using already-built indexes for most questions, and the compile path becomes a deterministic build step instead of the only way to understand the project.

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
- Breaking-change and upgrade analysis: compare public surfaces, obsolete metadata, permissions, and upgrade risk.
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

- Project/setup: `setup`, `doctor`, `new`, `packages`, `deps`, `deps-graph`, `clear-cache`, `init-debug`.
- Build/toolchain: `compile`, `package`, `download-symbols`, `authenticate`.
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

`al-lsp daemon` is the shared backend used by the CLI and MCP bridge. Keeping business logic in the daemon avoids one-off behavior between editor, terminal, and agent workflows.

## Symbol And Package Architecture

The symbol engine is one of the key reasons the project can support advanced AL queries.

- `.app` files are read natively as NAVX/ZIP packages.
- `NavxManifest.xml` and `SymbolReference.json` are parsed directly.
- Source files embedded in `.app` packages can be exposed as virtual source for navigation.
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
- Compile commands use the semantic bridge when available and fall back to `dotnet alc`.
- Compiler output is normalized into structured diagnostics.
- The package cache path is selected explicitly.
- Analyzer lists can be passed through command surfaces.
- `.app` files are produced through a deterministic temp-output and final-handoff flow.
- Launch/debug workflows compile on launch, locate the correct `.app`, publish/deploy where needed, and connect Zed to the BC debug backend through the native adapter.

For Zed users, this means the extension can provide first-class launch/attach workflows without being a VS Code extension clone.

## Generated Files And Repository Invariants

Several directories are generated or synchronized output. This matters because manual edits in the wrong place will be overwritten or will desynchronize Zed from the native parser.

- `tree-sitter-al/generator/tools/al-gen/src/main.rs` is the main generator.
- `tree-sitter-al/grammar.js`, `tree-sitter-al/src/parser.c`, `tree-sitter-al/src/scanner.c`, `tree-sitter-al/src/keywords.c`, and `tree-sitter-al/src/node-types.json` are generated grammar/parser artifacts.
- `tree-sitter-al/queries/*.scm` are generated query artifacts.
- `tree-sitter-al/data/*.json` contains committed language metadata consumed by `al-core`. Some files are generated by `al-gen`, some by `al-extract`, and some are static curated data.
- `languages/al/` is the tracked Zed-facing language package. Some `.scm` files are synchronized from `tree-sitter-al/queries`, while `config.toml`, tasks, runnables, overrides, injections, inline values, and semantic token rules are maintained in this repository unless a sync script explicitly updates them.
- `themes/bc-themes.json` is generated from Business Central VS Code theme data.

Release-critical invariants:

- `extension.toml` `[grammars.al].rev` must match the submodule commit recorded in the superproject gitlink for the release commit. A leading `+` in `git submodule status` is a release blocker.
- The root package version, `extension.toml` version, crate versions, and lockfile path-package versions should move together.
- `zed_extension_api` must stay pinned to a released crates.io API in committed release state.
- Release asset names in `src/lib.rs` must stay aligned with `.github/workflows/release.yml`.
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

The Zed package also depends on tracked repository assets such as `languages/al`, `snippets/*.json`, and `themes/bc-themes.json`.

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

Other supported configuration keys include `al.backgroundCodeAnalysis`, `al.diagnosticsTrigger`, `al.codeAnalyzers`, `al.enableExternalRulesets`, `al.ruleSetPath`, `al.enableCodeActions`, `al.inlayHints.parameterNames`, `al.inlayHints.returnTypes`, `al.packageCachePath`, `al.appLocalFolderPaths`, `al.assemblyProbingPaths`, `al.compilationOptions`, `al.incrementalBuild`, `al.nugetFeeds`, `al.useOnlyCustomFeeds`, `al.symbolsCountryRegion`, `al.outputAnalyzerStatistics`, `al.editorServicesPath`, `al.editorServicesLogLevel`, `al.rootNamespace`, `al.publisher`, `al.namespaceTemplate`, `al.algoSuggestedFolder`, and `al.maxDocumentSizeBytes`.

Native lint settings such as `al.enableNativeLint` and `al.nativeLintRules` are parsed for forward compatibility, but native lint rules are currently inert. Diagnostics today come from syntax parsing and the semantic bridge/CodeAnalysis path.

## Debugging

The extension registers the `al` debug adapter and debug locator. Snippets cover common Business Central launch and attach configurations, including browser launch, tenant/environment settings, sandbox attach, agent-session debugging, and MCP-assisted BC debugging options. Debug/settings schemas are tracked in the repository for reference and future Zed API support, but they are not currently registered with Zed on the released extension API 0.7 path.

Debug support has two important layers:

- Native Zed/DAP integration in `al-lsp --dap`.
- Business Central runtime/debug service integration for actual AL execution.

The native layer owns Zed protocol handling, config normalization, compile/deploy setup, breakpoints, stack/variable mapping, and editor-facing behavior. The BC runtime remains the source of truth for executing AL in a server environment.

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
scripts/check-repo-consistency.sh
```

`make grammar` regenerates tree-sitter artifacts from the generator and then runs `tree-sitter generate`. Treat the generated diff as part of the language package, not as hand-authored grammar code.
`scripts/check-repo-consistency.sh` checks repository slug drift only; separately verify version alignment, submodule gitlink/rev alignment, generated grammar/query/data freshness, and release asset names.

Useful validation:

```bash
cargo fmt --all --check
cargo test --workspace --exclude zed-al
cargo test -p zed-al --target wasm32-wasip1
cargo test -p al-test-harness
scripts/check-repo-consistency.sh
```

The semantic bridge is feature-gated. Release binaries build `al-lsp` with `--features semantic` so the in-process .NET bridge and `AlBridge.dll` are included.

## Release Process

Releases are tag-driven. Pushing a `v*` tag starts `.github/workflows/release.yml`, which builds native binaries for Linux, macOS, and Windows, builds the Zed WASM extension, generates checksums, and creates the GitHub release. The release workflow packages artifacts; it does not replace the full CI/test/version/submodule invariant suite.

Before tagging:

1. Start from a clean worktree except for the intentional release changes.
2. Commit any `tree-sitter-al` submodule changes inside the submodule.
3. Update the superproject submodule pointer.
4. Confirm `git submodule status` has no leading `+`.
5. Keep `extension.toml` grammar rev synchronized with the superproject gitlink.
6. Bump versions consistently across `extension.toml`, `Cargo.toml`, crates, and `Cargo.lock`.
7. Make the tag name match the package version, for example `v0.2.2` for version `0.2.2`.
8. Run repository consistency checks and the relevant test suite.
9. Require green CI on the exact commit being tagged.
10. Tag from the exact commit you want users to install.

## License

MIT.
