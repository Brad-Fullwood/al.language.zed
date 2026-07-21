# AL Language for Zed

AL Language for Zed is a native Business Central AL toolchain for Zed. It is not a syntax-highlighting package with a thin language-server wrapper. The repository contains a Rust language server, debug adapter, CLI/TUI, MCP server, symbol engine, query engine, test runner, and generated Zed language package for Microsoft Dynamics 365 Business Central AL development.

The toolchain is available from Zed, the terminal, CI, and MCP clients. Microsoft tooling remains available where exact compiler semantics or Business Central runtime behavior is required.

> Tester callout: this project is ready for serious testers across Zed editing, `al-lsp`, `al-explorer`, MCP, debugging, symbol downloads, and pure-logic test execution. Please test real Business Central projects, compare behavior against the official Microsoft tooling, and report exact commands, project shape, platform, expected result, actual result, and whether the issue is native-only or also reproduces through the Microsoft fallback. The roadmap in [ROADMAP.md](./ROADMAP.md) lists known gaps so testers can distinguish expected limitations from regressions.

## What Makes This Different

The standard Microsoft AL tooling is powerful, but most of it is coupled to the VS Code extension, the official language server, Business Central service assumptions, and opaque editor commands. That makes it hard to build deep Zed integration, hard to run narrow analysis from CI, hard to expose AL-aware tools to agents, and hard to test behavior outside the Microsoft extension boundary.

This project rewrites a large part of that experience in native Rust:

- AL parsing and Zed grammar integration are generated from AL language data and maintained in the bundled `tree-sitter-al` submodule.
- Workspace state is owned by `al-lsp`: file indexes, parse trees, symbol maps, package maps, document caches, insight graphs, call graphs, test discovery, and daemon command routing.
- `.app` symbol packages are read directly, with manifest parsing, `SymbolReference.json` extraction, virtual package navigation, and bounded archive safety checks. When packages embed `.al` source it is exposed as virtual source; otherwise navigation falls back to generated outlines from public symbol metadata.
- Symbol discovery is an in-memory indexed data model instead of repeated ad hoc package scans.
- Native `.app` compilation is implemented in Rust: source is syntax/project/declaration/binding verified, packaged into NAVX/ZIP `.app` artifacts, reopened for integrity checks, and atomically handed off only after all blocking checks pass.
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
| Build/package | Pure-Rust verifier + `.app` emitter in `crates/al-emit`: syntax, project/dependency, declaration, declared-symbol binding and package-integrity diagnostics; atomic artifact handoff; native defaults for daemon `compile`, LSP `al.compile`, publish, and DAP launch | `pack-native --validate` adds an explicit `alc` compatibility gate; `al.useOfficialCompiler=true` opts into Microsoft emission and full compiler semantics |
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

The default `.app` build path is verified native Rust. Microsoft `alc` remains available as an explicit compatibility gate or fallback for exact compiler semantics and analyzer behavior.

- The daemon `compile` dispatcher, LSP `al.compile`, publish path, and native DAP launch compile default to the pure-Rust verifier and `.app` emitter: no `alc`, no C# bridge.
- Native builds reject syntax errors, malformed manifests, invalid dependencies/id ranges, duplicate declarations, unresolved declared object types/targets/interfaces/SourceTable bindings, and malformed emitted packages with structured `ALNxxxx` diagnostics and exact native UTF-16 start/end ranges.
- Workspace-aware daemon/LSP/MCP builds also run the shared native `AL-NC*` semantic checks and resolved call/event-graph transaction diagnostics before emission. Available Microsoft/third-party AL bodies embedded in loaded `.app` packages participate in that graph; error-severity findings gate the build.
- Native artifacts are staged, synced, reopened, and atomically persisted only after generated metadata parses, package identity matches `app.json`, and the exact embedded source-path set matches the verified snapshot, so a failed build preserves the previous `.app`.
- `al-explorer pack-native --validate` runs native checks first and invokes `alc` only after they pass, providing an explicit authoritative compatibility gate without putting Microsoft tooling on the default path.
- `al.useOfficialCompiler=true` opts into Rust-managed `dotnet alc` where Microsoft compile-time validation is required.
- The native emitter builds the package directly from `app.json`, source files, package symbols, generated manifest data, and generated `SymbolReference.json`.
- The old Microsoft compiler subprocess path remains asynchronous, cancellable, timeout-aware, and killed on drop when used.
- Analyzer-backed package diagnostics and analyzer selection still belong to the Microsoft compiler path today.
- The Rust-managed `dotnet alc` fallback writes output to a per-invocation temporary directory and moves it into the project root when possible.
- `.app` selection prefers the manifest-derived `{publisher}_{name}_{version}.app` rather than arbitrary directory order.
- DAP deploy now reuses the same package-selection logic, so launch/deploy does not accidentally publish an older `.app` left in the project root.
- The emit test suite compares native output against ALC-shaped fixtures, including semantically-identical `SymbolReference.json` golden coverage (matching JSON values and method-id hashes, not raw zip-entry byte equality — zip timestamps make that comparison meaningless) for the supported project fixture.

The practical benefit is a safer and usually faster-feeling development loop: the editor and CLI keep using already-built indexes for most questions, and the compile path becomes a deterministic build step instead of the only way to understand the project.

## MCP Automation

The extension registers a Zed context server named `AL Tools`, which launches `al-lsp mcp` from `PATH`. The MCP server speaks newline-delimited JSON-RPC over stdio and forwards tool calls into the same daemon dispatcher used by the CLI.

Current MCP tools:

- `al_call` - invoke any method in the shared daemon tool catalog; this keeps the complete tool set available to MCP without a separate allow-list.
- `al_debug` - retain and drive a Business Central debug session across agent calls (start/attach,
  conditional breakpoints, state, stack, locals/globals/expansion, evaluate, step, continue,
  history, stop).
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
- `al_suggestevent` - suggest integration event publishers to subscribe to, by tracing the call/event graph from a procedure, table, or event.
- `al_testclassify` - classify every discovered AL test by where it actually runs today (local interpreter vs. requires live BC), with the reasons behind each decision.
- `al_testcoverage` - report static test coverage across the workspace (which objects/procedures are reached by tests via the call graph). No live BC required.
- `al_depgraph` - build the project's dependency graph from `app.json` (this app plus its declared dependencies), as JSON or Graphviz `dot`.

The named tools intentionally mirror Microsoft's AL agent tool surface where possible, while adding analysis tools the official surface does not expose. They are ergonomic aliases, not an availability boundary: `al_call` forwards any method and parameter object to the same daemon JSON-RPC dispatcher used by the CLI and Zed tasks. Native LSP requests and execute commands use LSP server handlers, while sharing lower-level workspace, query, build, symbol, and test code.

## Native AL Test Runtime

Yes, this project includes a native AL language runner for tests.

The local runner interprets a supported subset of AL test bodies directly from tree-sitter trees, without a .NET runtime or live Business Central server. It has enforced pure-logic and workspace-record modes, with conservative live-BC fallback for platform semantics. It handles:

- A defined subset of statement execution for blocks, `if`, `while`, `for`, `foreach`, `repeat`, `case`, assignment, expression statements, `exit`, and `asserterror`.
- A defined subset of expression evaluation for literals, identifiers, unary and binary operators, arithmetic, comparisons, boolean logic, string concatenation, and parenthesized expressions.
- A small set of built-ins and stubs such as `Error`, `Message`, `StrSubstNo`, `Format`, `StrLen`, `CopyStr`, `LowerCase`, `UpperCase`, `IndexOf`, `Library Assert`, `Library - Variable Storage`, `Library Random`, and `Any`.
- Static, BC-free test discovery. Codeunits are discovered by `Subtype = Test` or contained `[Test]` procedures, but only `[Test]` methods are returned as executable tests; lifecycle methods are not listed as tests.
- JUnit XML, default static-call-graph Cobertura, and opt-in dynamic executed-line/decision coverage from interpreter runs.
- Interpreter-backed mutation testing over both local tiers, including stable parallel mutant execution; mutants without interpreter-runnable coverage are reported as survived.

The router is conservative and currently pattern-based. `Interp` and supported `InterpRecord` codeunits run locally from `test-run`, `test-run-all`, the TUI, and MCP `al_runtests`; codeunits that need base-app/package tables, unsupported record behavior, HTTP, UI, reports, sessions, or transactions route to live BC.

The record runtime is wired to workspace table definitions, with isolated in-memory data, keys, BC-style filters, common CRUD/navigation methods, and CalcFormula-backed FlowFields. It intentionally does not emulate platform triggers, transactions, permissions, RecordRef/FieldRef, or package-only table schemas.

`test-snapshot replay` validates an existing snapshot file and `test-snapshot diff` compares two
files. Live Business Central snapshot capture is not exposed as a command.

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
- Breaking-change and upgrade analysis: compare public surfaces, obsolete metadata, permissions,
  and upgrade risk against a previous `.app` supplied with `--baseline-app`. Without a baseline the
  commands report that the analysis was not evaluated.
- Permission and data audits: inspect permission sets, table data classification, and missing metadata.
- XLIFF tooling: generate, refresh, inspect untranslated entries, and suggest translations from workspace symbols.
- Bulk fixes: add application areas, tooltips, data classification, organize files, and sort members.

The shared daemon catalog is available from both CLI and MCP (`al_call` provides complete dispatcher
coverage); Zed tasks and editor actions use the same implementations. XLIFF
refresh/untranslated/suggest and code actions are callable through `al_call` even when they do not
have a dedicated named alias. Low-level `.app` inspection remains a Rust library API rather than a
daemon or MCP method.

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

`al-explorer` is the terminal companion to `al-lsp`. In command mode it is a JSON-RPC client for the daemon and supports `--json` for scripts and CI. With no subcommand, it opens a TUI with object browsing, event chains, call graphs, profiler views, and test runner views on Linux, macOS, and Windows.

The CLI command surface includes:

- Project/setup: `setup`, `doctor`, `diag`, `new`, `packages`, `deps`, `deps-graph`, `clear-cache`, `init-debug`.
- Build/toolchain: `compile`, `pack-native`, `package`, `download-symbols`, `authenticate`.
- LSP-style queries: `hover`, `definition`, `references`, `signature`, `completions`, `symbols`, `folding`, `tokens`, `parse`, `rename`, `hints`.
- Symbols and objects: `search`, `object`, `by-id`, `composed`, `builtins`, `rules`, `error-codes`, `generate-completions`, `version`.
- Events and insight: `events`, `subscribers`, `event-source`, `trace`, `intercept`, `entrypoints`, `graph`, `impact`, `suggest-event`, `insight-stats`.
- Analysis: `metrics`, `dead-code`, `sql-scan`, `duplicates`, `arch-lint`, `native-check`, `breaking`, `upgrade`, `obsolete`, `audit-data`, `permission-audit`, `profiler-hints`.
- Formatting/refactoring/codegen: `format`, `lint`, `fix`, `permissions`, `generate`, `add-application-area`, `add-tooltips`, `add-data-classification`, `sort-members`, `organize-files`.
- Debug/profiling: `debug`, `snapshot`, `profile`.
- Tests: `tests`, `test-run`, `test-run-all`, `test-coverage`, `test-mutate`, `test-affected`, `test-classify`, `test-snapshot`, `test-results`.
- Translation: `xlf`.

Nested command groups include `debug start|breakpoint|state|eval|continue|step|history|stop`, `snapshot start|list|download`, `profile start|stop|analyze`, `test-snapshot replay|diff`, and `xlf generate|refresh|untranslated|suggest`.

### Daemon Protocol

`al-lsp daemon` is the shared backend used by the CLI, MCP bridge, and Zed tasks. Native `al-lsp --stdio` LSP mode uses LSP handlers over the same workspace/query/build modules rather than the daemon dispatcher. `al-lsp --dap` is a separate stdio DAP mode with its own debug session plumbing.

## Symbol And Package Architecture

The symbol engine is one of the key reasons the project can support advanced AL queries.

- `.app` files are read natively as NAVX/ZIP packages.
- `NavxManifest.xml` and `SymbolReference.json` are parsed directly.
- The `al-symbols::app_inspect` library API can list, classify, and safely extract package entries
  for format and provenance audits. It is not currently exposed through `al-explorer` or MCP.
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
- The daemon `compile` dispatcher, LSP `al.compile`, publish path, and native DAP launch compile default to the pure-Rust verified `.app` pipeline and return structured native diagnostics.
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
- `tree-sitter-al/data/*.json` contains committed language metadata consumed by `al-syntax`. Some files are generated by `al-gen`, some by `al-extract`, and some are static curated data.
- `languages/al/` is tracked generated output. Do not edit it by hand. Canonical parser queries are copied from `tree-sitter-al/queries`; Zed-specific config, tasks, runnables, overrides, injections, inline values, bracket rules, outline rules, and semantic token rules are generated from templates in `tree-sitter-al/generator`.
- `themes/bc-themes.json` is generated from Business Central VS Code theme data.

Release-critical invariants:

- `extension.toml` `[grammars.al].rev` must match the submodule commit recorded in the superproject gitlink for the release commit. A leading `+` in `git submodule status` is a release blocker.
- Product versions for `zed-al`, `extension.toml`, `al-lsp`, and their lockfile entries move
  together. Library crates keep independent semantic versions.
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

Every native archive includes `al-lsp`, `al-explorer`, and the semantic bridge files (`.exe` binaries on Windows). Daemon IPC uses Unix-domain sockets on Linux/macOS and named pipes on Windows.

Zed auto-resolves or downloads `al-lsp` for LSP and DAP. The MCP context server requires `al-lsp` on `PATH`. Zed tasks require `al-explorer` on `PATH`; release archives ship it on Linux, macOS, and Windows.

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

`al.useOfficialLsp` is the explicit escape hatch for delegating to Microsoft's official AL LSP. The
default path is this project's native `al-lsp`. Set `AL_DOTNET_PATH` to select a specific executable
for Microsoft .NET-hosted AL tools; otherwise the toolchain resolves `dotnet` from `PATH`.

**Every setting—with types, defaults, and descriptions—is documented in the
[settings reference](Docs/reference/settings.md), and a ready-to-copy template is available at
[examples/zed-settings.jsonc](examples/zed-settings.jsonc).** `al.enableNativeLint` and
`al.nativeLintRules` control the native file, project-semantic, and resolved call/event-stack rules
(`AL-NL*`/`AL-NC*`). Microsoft's CodeAnalysis bridge is optional and additive.

On Zed Dev/Nightly (extension API >= 0.8) the `lsp.al-lsp.settings` keys autocomplete and validate as you type; on Stable Zed the settings still apply, just without in-editor autocomplete (use the template above). This lights up on Stable automatically once the 0.8 extension API reaches the registry.

### Project-file schemas (app.json, rulesets)

This extension ships JSON Schemas for the AL project files you edit by hand: `app.json`,
`*.ruleset.json`, `AppSourceCop.json`, and `migration.json`. Associate them with Zed's bundled JSON
language server using the `json.schemas` block in
[examples/zed-settings.jsonc](examples/zed-settings.jsonc). The mapping is documented in the
[settings reference](Docs/reference/settings.md#project-file-schemas-appjson-rulesets-).

## Debugging

The extension registers the `al` debug adapter and debug locator. Snippets cover common Business
Central launch and attach configurations, including browser launch, tenant/environment settings,
sandbox attach, and agent-session fields. Debug-config schemas are tracked for reference;
configurations are authored through the bundled snippets. See [Zed Settings](#zed-settings) and the
[settings reference](Docs/reference/settings.md) for language-server and project-file schemas.

Debug support has two important layers:

- Native Zed/DAP integration in `al-lsp --dap`.
- Stateful MCP debug control through the `al_debug` tool in `al-lsp mcp`.
- Business Central runtime/debug service integration for actual AL execution.

The native adapter currently implements the core launch/attach, publish, breakpoint, stepping, stack, scopes, variables, and evaluate flow against BC REST/SignalR. MCP clients can attach, set conditional breakpoints, inspect state, stack, locals, globals, and expanded values, evaluate expressions in a selected frame, continue, step, inspect history, and stop. The MCP process retains the native session between tool calls; it exposes structured debug operations rather than raw DAP frames. The BC runtime remains the source of truth for executing AL in a server environment. See [the DAP feature guide](Docs/features/debugging-dap.md#mcp-debug-control).

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for generated-file ownership, the verification matrix, and
the required grammar-first publishing workflow.

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
cargo test -p zed-al
cargo test -p al-protocol
make wasm
cargo test -p al-test-harness
scripts/check-repo-consistency.sh
scripts/check-release-hygiene.sh
make language
```

Daemon IPC is tested against the host's real transport: Unix-domain sockets on Linux/macOS and
named pipes on Windows. CI's native `windows-latest` job builds `al-lsp` and `al-explorer`, runs the
protocol tests, and runs `cli_smoke` plus `extension_smoke` end to end; a cross-compile alone is not
treated as sufficient named-pipe coverage. See the [testing guide](Docs/testing-guide.md#daemon-ipc-on-linux-macos-and-windows).

The semantic bridge is feature-gated. Release binaries build `al-lsp` with `--features semantic` so the in-process .NET bridge and `AlBridge.dll` are included.

## Release Process

Releases are tag-driven. Pushing a `v*` tag starts `.github/workflows/release.yml`, which first runs the release hygiene preflight, then builds native binaries for Linux, macOS, and Windows, builds the Zed WASM extension, generates checksums, and creates the GitHub release. The preflight blocks artifact creation unless the tag/version/submodule/generated-file invariants pass and CI has succeeded on the exact tagged commit.

Before tagging:

1. Start from a clean worktree except for the intentional release changes.
2. Commit and push any `tree-sitter-al` changes inside the submodule.
3. Verify the grammar commit is reachable from the grammar remote.
4. Update the superproject gitlink and `extension.toml` revision to that commit.
5. Regenerate `languages/al` and confirm `git submodule status` has no leading `+`.
6. Bump the synchronized product versions in `extension.toml`, root `Cargo.toml`, `al-lsp`, and
   `Cargo.lock`; version library crates independently when their APIs change.
7. Make the tag name match the product version, for example `v0.2.2` for version `0.2.2`.
8. Run repository consistency checks, release hygiene checks, and the relevant test suite.
9. Require green CI on the exact commit being tagged; the tag workflow enforces this before building artifacts.
10. Push the superproject commit only after the referenced grammar commit is available remotely.
11. Tag from the exact commit you want users to install.

## License

MIT.
