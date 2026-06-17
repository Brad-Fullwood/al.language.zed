# C# Bridge Retirement Plan

Last reviewed: 2026-06-17.

This document tracks what still depends on the C#/.NET CodeAnalysis bridge, what
has already been replicated natively, and what must be finished before
`crates/al-core/bridge/` can be removed.

The C# bridge means:

- `crates/al-core/bridge/AlBridge.csproj`
- `crates/al-core/bridge/Bridge.cs`
- Rust hosting via `crates/al-core/src/semantic/{host,bridge,lifecycle,cache}.rs`
- build/release/install plumbing that builds and ships `AlBridge.dll`

It does **not** mean every Microsoft dependency. `dotnet alc`, Business Central
server publish/debug APIs, and official-LSP fallback are separate escape hatches.

## Current Bridge API Surface

| Bridge method | Rust entrypoint | Current users | Purpose |
| --- | --- | --- | --- |
| `analyze` | `SemanticBridge::analyze` | `server/diagnostics.rs` | CodeAnalysis syntax/semantic diagnostics plus optional CodeCop/UICop/AppSourceCop/PTE analyzers. |
| `typeAt` | `SemanticBridge::type_at` | `queries/hover.rs` fallback | CodeAnalysis semantic-model lookup at cursor. |
| `completions` | `SemanticBridge::completions_at` | `queries/completions.rs` member-access fallback | CodeAnalysis semantic-model member completion. |
| `builtins` | `SemanticBridge::builtin_types` | LSP startup/cache in `server/lsp.rs` and `semantic/lifecycle.rs` | Extract built-in types, methods, parameters, returns, enum values. |
| `errorCodes` | `SemanticBridge::error_codes` | `server/lsp.rs`, diagnostics enrichment | Extract compiler error-code messages/severities from CodeAnalysis. |
| `compile` | `SemanticBridge::compile` | daemon `compile`, publish path | Runs Microsoft `alc` via the C# bridge and maps SARIF/stdout into structured diagnostics. |
| `ping`/`Init` | bridge lifecycle | lazy init, health checks | Prove CLR/CodeAnalysis can load and recover from failures. |

## Already Native Or Mostly Native

| Area | Native state | Bridge dependency left |
| --- | --- | --- |
| Parsing and grammar | `tree-sitter-al` parser, generated scanner, generated queries, and generated `languages/al` package. | None for syntax tree generation. |
| Syntax diagnostics | Tree-sitter error/missing-node diagnostics are native. | Compiler-grade semantic diagnostics still come from `analyze`. |
| Formatting/folding/tokens/navigation | Native formatting, folding, semantic tokens, outline, symbols, document/workspace lookup, rename/references/definition paths. | Some hover/completion fallbacks still call `typeAt` and `completions`. |
| Static language data | Generated JSON data for keywords, token classification, object types, page controls, implicit variables, runtime enums, `NavTypeKind`, system objects, and builtin functions. | Built-in type member catalog still uses `builtins` for exact CodeAnalysis method/enum metadata. |
| `.app` package reading | Native NAVX/ZIP reader, manifest parsing, `SymbolReference.json` model, source extraction, package cache, composed objects, event discovery. | None for reading symbol packages. |
| Symbol/query engine | Native symbol maps and query commands: search, composed objects, dependencies, impact, dead code, event tracing/subscribers, source/virtual navigation. | None for package-query execution. |
| AL test runner | Native test discovery/routing plus interpreter/live-BC backends and mock-record work. | No direct C# bridge dependency; live BC remains a runtime dependency for unsupported cases. |
| `.app` emission research | `crates/al-core/src/emit/` now has pure-Rust NAVX/package, manifest, source extraction, `SymbolReference.json`, method-id hashing, external-symbol resolver, and `pack-native` CLI work in the current tree. | Not yet the production compile path everywhere; needs live-BC publish validation and final wiring before it can replace bridge compile. |
| Rust-managed `dotnet alc` | `crate::build::compile_project` runs `dotnet alc` directly with timeout/output/diagnostic handling. | Not bridge-dependent, but still Microsoft compiler-dependent. |
| DAP compile paths | DAP launch compile uses Rust-managed `dotnet alc` paths, not the C# bridge. | Separate Microsoft compiler dependency, not a bridge blocker. |

## What Must Be Replicated To Delete The Bridge

### 1. Semantic Diagnostics And Analyzers

Current bridge role:

- Parse source through CodeAnalysis.
- Return compiler diagnostics with CodeAnalysis spans/messages.
- Build a `Compilation` with package-cache references.
- Load analyzer DLLs and run CodeCop/UICop/AppSourceCop/PerTenantExtensionCop when configured.

Native replacement required:

- A semantic diagnostic engine that covers at least the diagnostics currently
  expected from CodeAnalysis in editor workflows.
- Analyzer compatibility decision:
  - either reimplement the high-value analyzer rules natively, or
  - keep an explicit official compiler/analyzer command that is not part of the
    default bridge-free editor loop.
- Ruleset/analyzer configuration support or deliberate removal from settings
  claims.
- Tests proving `al.enableCodeAnalysis`, `backgroundCodeAnalysis`, analyzer
  selection, package references, and diagnostic severities behave without the
  bridge.

Deletion gate:

- `server/diagnostics.rs` no longer calls `get_or_init_bridge`.
- README/settings docs clearly distinguish native diagnostics from optional
  official compiler checks.

### 2. Hover Type Resolution

Current bridge role:

- `hover_full` first tries native hover, then calls `typeAt` when native lookup
  cannot answer.
- `typeAt` uses CodeAnalysis semantic model around the cursor, with unsaved-text
  support.

Native replacement required:

- Complete native expression/type resolver for:
  - variables, parameters, fields, globals, implicit variables
  - record variables and table field access
  - object/subtype references
  - builtin type members and return types
  - procedure return types and overload selection
  - package symbols and composed extension members
- Documentation formatting for native type/member docs.
- Regression tests for unsaved buffer text, UTF-16 LSP positions, member access,
  and package-backed symbols.

Deletion gate:

- `queries/hover.rs` has no bridge fallback and still returns equal-or-better
  answers for workspace, package, and builtin symbols.

### 3. Member Completions

Current bridge role:

- Native completions cover keywords, object context, workspace symbols, variables,
  snippets, and many static cases.
- Member-access completions can fall back to CodeAnalysis via `completions`.

Native replacement required:

- Member completion from the native type resolver.
- Builtin type method/property completion from generated static metadata.
- Record/table field completion with extension/composed-object support.
- Completion ranking/dedup that preserves the current native-first behavior.
- Tests for unsaved text, UTF-16 position handling, member chains, package
  fields, builtin methods, and incomplete syntax.

Deletion gate:

- `queries/completions.rs` never calls `SemanticBridge::completions_at`.

### 4. Built-In Type Member Catalog

Current bridge role:

- `builtins` reflects CodeAnalysis type symbols to extract built-in AL types,
  methods, parameters, return types, enum values, and docs.
- Rust caches this per toolchain version and builds `SemanticCache`.

Native replacement required:

- Generated, versioned data under `tree-sitter-al/data` or another generator-owned
  source for the full built-in type/member catalog.
- Coverage for method overloads, `var` parameters, return types, enum values,
  and docs where available.
- A generator in `tree-sitter-al` that can refresh this data from Microsoft
  tooling during maintainer updates, without needing the runtime bridge in users'
  editor sessions.
- `SemanticCache` populated from static/generated data instead of
  `SemanticBridge::builtin_types`.

Deletion gate:

- `ensure_builtins_loaded` does not initialize the bridge.
- Disk cache for bridge-extracted builtins is removed or migrated to generated
  data versioning.

### 5. Error Code Catalog

Current bridge role:

- `errorCodes` reflects CodeAnalysis `ErrorCode` and `NavDiagnosticInfo` to map
  compiler codes to messages/severities.
- Diagnostics use this to enrich messages.

Native replacement required:

- Generated static error-code catalog keyed by AL toolchain version.
- Clear behavior for unknown/new compiler codes.
- Tests for diagnostic enrichment without bridge initialization.

Deletion gate:

- `ensure_error_codes_loaded` reads generated data, not `SemanticBridge`.

### 6. Compile/Package Production Path

Current bridge role:

- daemon `compile` and publish default to `SemanticBridge::compile`.
- The C# handler currently shells out to Microsoft `alc`, parses SARIF/stdout,
  discovers the output `.app`, and returns bridge-shaped diagnostics.
- `al.useOfficialCompiler=true` opts into Rust-managed `dotnet alc`; that path
  does not require the C# bridge but is still a Microsoft compiler path.

Native replacement required:

- Decide final production default:
  - pure-Rust `pack-native`, or
  - Rust-managed `dotnet alc`, or
  - explicit per-command policy.
- If pure-Rust is the default:
  - wire `emit::build_app_from_project` / `pack-native` into daemon `compile`,
    publish, LSP compile commands, and DAP launch compile where appropriate.
  - validate natively emitted `.app` packages against a live BC tenant.
  - preserve analyzer/semantic diagnostics separately from packaging success.
  - handle package dependencies, translations, resources, profile symbols,
    control add-ins, permission sets, and runtime-specific manifest fields.
- If Rust-managed `dotnet alc` remains as fallback:
  - keep it explicit and documented as Microsoft compiler fallback, not native.
  - remove the bridge compile wrapper and call `crate::build::compile_project`
    directly where official compile is requested.

Deletion gate:

- No call site uses `SemanticBridge::compile`.
- Release binaries no longer need `AlBridge.dll` to build/package/publish.

### 7. Bridge Host, Build, Release, And Install Plumbing

Current bridge role:

- `netcorehost` loads the CLR and `AlBridge.dll`.
- `build.rs`, `Makefile`, release workflow, install scripts, and archives build
  and copy the bridge.
- Runtime failure paths mention "semantic bridge".

Native replacement required:

- Remove `crates/al-core/bridge/`.
- Remove `semantic` feature semantics that mean "ship CLR bridge".
- Remove `netcorehost`/nethost dependencies if no other code uses them.
- Remove bridge build/copy logic from `build.rs`, `Makefile`, release archives,
  install docs, and CI.
- Replace user-facing diagnostics:
  - no "Bridge not initialized" messages
  - settings/docs no longer imply CodeAnalysis is loaded in-process
  - `al.useOfficialCompiler` remains only if Microsoft compiler fallback stays

Deletion gate:

- `rg "get_or_init_bridge|SemanticBridge|AlBridge|netcorehost|CodeAnalysis bridge"`
  has no production call sites.

## Suggested Removal Order

1. Generate static builtins and error-code catalogs from CodeAnalysis into
   generator-owned data files.
2. Populate `SemanticCache` and diagnostic enrichment from generated data.
3. Finish native type resolver and use it for hover/member completions.
4. Replace bridge `compile` with the chosen build service.
5. Rework diagnostics/analyzers so editor diagnostics are native by default and
   official compiler/analyzer checks are explicit.
6. Delete bridge host/build/release plumbing.
7. Remove C# project and `netcorehost` dependencies.

## Final Removal Checklist

- [ ] `SemanticBridge::analyze` has no callers.
- [ ] `SemanticBridge::type_at` has no callers.
- [ ] `SemanticBridge::completions_at` has no callers.
- [ ] `SemanticBridge::builtin_types` has no callers.
- [ ] `SemanticBridge::error_codes` has no callers.
- [ ] `SemanticBridge::compile` has no callers.
- [ ] `crates/al-core/bridge/` is deleted.
- [ ] Release archives no longer include `bridge/AlBridge.dll` or
      `AlBridge.runtimeconfig.json`.
- [ ] `make build`, `make install`, and CI no longer build .NET bridge projects.
- [ ] README/settings/docs describe Microsoft tooling only as explicit fallback,
      not as required editor infrastructure.
