# Semantic Bridge (.NET CodeAnalysis)

**Module:** `crates/al-semantic/src/` + `crates/al-semantic/bridge/Bridge.cs`, `AlBridge.csproj` ·
**Status:** ✅ shipped (feature-gated) · **being retired** in favor of native Rust

The semantic bridge is the project's link to Microsoft's actual AL compiler semantics. When you want
*compiler-grade* diagnostics, type info, and completions — the things that need real name binding and
type checking — the bridge loads the .NET CLR **in-process** and calls Microsoft's `CodeAnalysis` API
through a JSON-over-FFI boundary. No subprocess, no socket: the bridge DLL and the Rust code share one
process and communicate via C-ABI function pointers.

## What it provides

| Bridge method | Used for | Rust caller |
| --- | --- | --- |
| `analyze(file, source, analyzers[], packageCache)` | compiler diagnostics + CodeCop/UICop/AppSourceCop/PerTenantCop | `server/diagnostics.rs` (phase 2) |
| `typeAt(file, pos, [unsavedText])` | hover fallback (semantic type at cursor) | `queries/hover.rs` |
| `completions_at(file, pos, [unsavedText])` | member-access completion fallback | `queries/completions.rs` |
| `builtins()` | built-in types/methods catalog | LSP startup → `SemanticCache` |
| `errorCodes()` | error-code → severity/message catalog | diagnostic message enrichment |
| `compile(project, [alcPath], [packageCache])` | run `alc`, parse SARIF | **no longer called** (native emitter is default) |
| `ping()` | health check | lifecycle |

## How it works

### Hosting & threading (`host.rs`, `bridge.rs`)

`DotNetHost` loads `hostfxr`, initializes the CLR from a runtime config, and obtains delegates for the
`Bridge.cs` entry points (`Init`, `HandleRequest`, `FreeBuffer`). Every request is JSON in both
directions: `{ "method": ..., "params": ... }` → `{ "result": ... }` or `{ "error": ... }`. All CLR
calls are serialized behind a `std::sync::Mutex<DotNetHost>`; async Rust tasks acquire it via
`tokio::task::spawn_blocking` so the tokio executor is never blocked.

### The C# side (`Bridge.cs`)

`Bridge.cs` is a thin **reflection** wrapper over `CodeAnalysis` types (SyntaxTree, SourceText,
Compilation, DiagnosticAnalyzer, NavTypeKind, ErrorCode, NavDiagnosticInfo). `Init` loads
`CodeAnalysis.dll` and caches the type/method metadata; `HandleRequest` deserializes the request,
dispatches to the appropriate handler, and serializes the response. Handlers parse AL into a syntax
tree, build a compilation against the package cache, run analyzer DLLs, walk the semantic model for
types/overloads, and (for `compile`) shell out to `alc` and parse its output.

### Correctness contracts

- **Position (F-036):** LSP 0-based UTF-16 `(line, column)` is passed through unchanged; the C# side
  walks newlines from file start and adds the column. Off-by-one here lands on the wrong token.
- **Unsaved text (F-037):** when `text` is supplied, the bridge uses the editor buffer instead of
  reading disk, so hover/completion reflect unsaved edits.
- **Resource limits:** 16 MiB per document, 256 MiB max response.

### Resilience (`lifecycle.rs`, `cache.rs`)

- **Lazy init** on first use, with double/triple-checked locking around the blocking CLR
  initialization.
- **Timeout + cooldown (T047):** 30 s per call; on timeout a cooldown gate prevents a thundering-herd
  of retries, and the cooldown stamp survives a bridge restart so a hung call from a previous CLR
  generation can't bypass it. Up to 3 restart attempts; the counter resets on a successful restart.
- **Disk cache** (under `…/al-lsp/semantic/`) stores `builtins()` and `errorCodes()` keyed by
  sanitized toolchain version, turning a ~500 ms CLR call into a cache hit; corruption is detected via
  JSON parse failure and the file is regenerated.
- **One-shot user notification** when the bridge fails persistently (the LSP won't spam you).

## Feature gating

The bridge is behind the `semantic` Cargo feature. Release binaries are built with
`--features semantic`, bundling the in-process .NET bridge and `AlBridge.dll`. Without the feature,
`DotNetHost` is a no-op stub returning `NotInitialized`. The `al.enableCodeAnalysis` setting (default
true) further gates whether the bridge is initialized at all.

## Microsoft comparison

The bridge **is** Microsoft's compiler semantics — it embeds the same `CodeAnalysis` engine the
official extension uses. The difference is packaging: this project hosts it in-process from Rust with
explicit timeouts, a cooldown gate, version-keyed caching, and a documented plan to replace it. The
official extension couples the same engine to the .NET AL Language Server and VS Code.

## Why this approach (and why it is being retired)

Exact compile-time semantics belong to Microsoft, so for diagnostics/hover/completion the project
delegates to the real compiler rather than approximating it. But an in-process CLR is heavy: it adds a
.NET runtime dependency, serializes all calls, and is the slowest part of the editor loop. The native
emitter already removed the bridge from the build path. Replacing the remaining bridge work requires:

1. native semantic diagnostics (replace `analyze`),
2. native type resolver + hover (replace `typeAt`),
3. native member completions (replace `completions_at`),
4. generated builtin catalog (replace `builtins`),
5. generated error-code catalog (replace `errorCodes`),
6. ✅ native compile (done) — **open gap:** native compile is emit-only; once §1 lands, wire semantic
   validation in before emitting the `.app`.

## How to use

You normally don't call the bridge directly — it powers diagnostics, hover, and completion
automatically. To control it:

- `al.enableCodeAnalysis` (default true) — turn the bridge on/off.
- `al.backgroundCodeAnalysis`, `al.diagnosticsTrigger`, `al.diagnosticsScope`, `al.codeAnalyzers`,
  `al.ruleSetPath`, `al.assemblyProbingPaths` — tune what runs (see the
  [settings reference](../reference/settings.md)).

## Limitations & roadmap

- No async inside a CLR call (sync mutex); single CLR per process (no multi-toolchain in one session);
  a timeout prevents *new* calls during cooldown but cannot interrupt an in-flight CLR call.
- ⛔ `compile` via the bridge is no longer used (native emitter is default), kept only behind
  `al.useOfficialCompiler`.
- Roadmap: complete the native replacements above so the .NET dependency can eventually be dropped.
