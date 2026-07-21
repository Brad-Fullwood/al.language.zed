# C# Bridge Retirement Plan

Last reviewed: 2026-06-17.

This document tracks what still depends on the C#/.NET CodeAnalysis bridge, what
has already been replicated natively, and what must be finished before
`crates/al-core/bridge/` can be removed.

The C# bridge means:

- `crates/al-semantic/bridge/AlBridge.csproj`
- `crates/al-semantic/bridge/Bridge.cs`
- Rust hosting via `crates/al-semantic/src/{host,bridge,lifecycle,cache}.rs`
- lifecycle orchestration via `crates/al-workspace/src/semantic_lifecycle.rs`
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
| `compile` | deprecated `SemanticBridge::compile` | none | Retired and fails closed; maintained build paths live in `al-compile`. |
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

**Status: DONE for the bridge — the production default is now pure-Rust.** As of
2026-06-17 the daemon `compile` endpoint, `al.compile`, publish, and DAP launch compile all
default to `crate::build::native_compile` (the pure-Rust `emit::build_app_from_project`
emitter — no `alc`, no C# bridge). `SemanticBridge::compile` is no longer called from any of
these paths; `al.useOfficialCompiler=true` is the explicit, documented Microsoft-`alc`
fallback (Rust-managed `crate::build::compile_project`, not the bridge). The `.app` is
byte-identical to `alc`'s output across all object types + metadata + cross-app references
(see `BENCHMARKS.md`; ~10-12x faster end-to-end).

Done: package dependencies/manifest fields, translations (`TextData/*.xliff`), resources
(control add-in bundles + report layouts), profile symbols, control add-ins, permission sets
(incl. system-object ids), cross-app subtype/extension/field resolution. Live-BC publish
validation: a natively-emitted `.app` was accepted by a real sandbox (it reached PTE
content-validation; the structure is sound).

Deletion gate (for §6 specifically): ✅ no call site uses `SemanticBridge::compile`.
(The bridge crate still can't be deleted — §1-§5 keep other callers; see the checklist.)

#### ⚠️ OPEN GAP — native compile does NOT validate (wire in after §1)

The native emitter does **parse → emit**; it does **NOT** do `alc`'s **bind → type-check**.
So `native_compile` will pack syntactically-parseable-but-semantically-wrong AL into an
`.app` that the BC server then rejects on publish. **This was a deliberate scope cut, and it
MUST be closed.** Today the safety nets are the LSP's continuous diagnostics (syntax-level
now; semantic once §1 lands) and the BC server's publish-time validation.

**Dependency + wire-in trigger:** native semantic validation depends on **§1 (Semantic
Diagnostics And Analyzers)** being implemented natively. **When §1 lands:**

1. Have `native_compile` (`crates/al-core/src/build.rs`) run the native semantic analysis
   over the project first.
2. If it yields any **error**-severity diagnostic, **fail the compile and emit nothing**
   (return the diagnostics in `CompileResult`), exactly as `alc` does — never produce an
   `.app` from invalid source.
3. Only write the `.app` when the project type-checks clean. Then surface those diagnostics
   in the daemon `compile`/publish responses (today they return an empty `diagnostics` list).

Until §1 is done, `native_compile` always succeeds on parseable input. Tracked here so it is
wired in the moment §1 is ready; do not ship a "validates on compile" claim before then.
(`al.useOfficialCompiler=true` is the interim escape hatch for users who need `alc`'s
compile-time guarantees now.)

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
- [x] `SemanticBridge::compile` has no callers. (2026-06-17 — compile/publish/DAP-deploy
      default to `crate::build::native_compile`; `al.useOfficialCompiler=true` → Rust-managed
      `dotnet alc`. ⚠️ native compile is emit-only until §1 adds semantic validation — see §6.)
- [ ] `crates/al-core/bridge/` is deleted.
- [ ] Release archives no longer include `bridge/AlBridge.dll` or
      `AlBridge.runtimeconfig.json`.
- [ ] `make build`, `make install`, and CI no longer build .NET bridge projects.
- [ ] README/settings/docs describe Microsoft tooling only as explicit fallback,
      not as required editor infrastructure.
