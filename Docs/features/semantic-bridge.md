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
| `analyze(file, source, analyzers[], packageCache)` | compilation diagnostics + CodeCop/UICop/AppSourceCop/PerTenantCop | `server/diagnostics.rs` (phase 2) |
| `typeAt(file, pos, [unsavedText])` | hover fallback (semantic type at cursor) | `queries/hover.rs` |
| `completions_at(file, pos, [unsavedText])` | member-access completion fallback | `queries/completions.rs` |
| `builtins()` | built-in types/methods catalog | LSP startup → `SemanticCache` |
| `errorCodes()` | error-code → severity/message catalog | diagnostic message enrichment |
| `compile(...)` | legacy Rust API; fails closed with `-32601` | **retired** — builds use `al-compile` |
| `ping()` | health check | lifecycle |

## How it works

### Hosting & threading (`host.rs`, `bridge.rs`)

`DotNetHost` loads `hostfxr`, initializes the CLR from a runtime config, and obtains delegates for the
`Bridge.cs` entry points (`Init`, `HandleRequest`, `FreeBuffer`). Every request is JSON in both
directions: `{ "method": ..., "params": ... }` → exactly one of `{ "result": ... }` or
`{ "error": ... }`. Rust validates lengths, UTF-8, JSON, and the response envelope before exposing a
result. CLR calls run via `tokio::task::spawn_blocking`; a process-wide semaphore and a host mutex
serialize them. The process-wide gate matters after a restart: an abandoned blocking task from an old
bridge generation cannot overlap the replacement generation.

### The C# side (`Bridge.cs`)

`Bridge.cs` is a thin **reflection** wrapper over `CodeAnalysis` types (SyntaxTree, SourceText,
Compilation, DiagnosticAnalyzer, NavTypeKind, ErrorCode, NavDiagnosticInfo). `Init` is idempotent for
one canonical CodeAnalysis path, rejects a second toolchain in the same process, and validates the
minimum reflection surface before reporting success. `HandleRequest` validates and serializes one
request at a time. Handlers parse AL, build a compilation against the selected package cache, obtain
compiler diagnostics, run requested analyzer DLLs, and walk the semantic model for type and completion
information. Missing/incompatible APIs and requested analyzers are errors; they are not converted into
plausible empty results.

### Correctness contracts

- **Position (F-036):** LSP 0-based UTF-16 `(line, column)` is passed through unchanged. .NET string
  indexes are UTF-16 code units. The bridge validates that the line exists and that the column is on
  that line (including its end position); an invalid column cannot spill into a later line, and EOF is
  not shifted back to the previous character.
- **Unsaved text (F-037):** when `text` is supplied, the bridge uses the editor buffer instead of
  reading disk, so hover/completion reflect unsaved edits.
- **Project context:** hover and completion forward `al.packageCachePath`, or the discovered project's
  package directory, so dependency symbols participate in binding.
- **Resource limits:** 16 MiB per document, 32 MiB per JSON request, 256 MiB max response. Paths crossing
  the JSON/FFI boundary must be valid UTF-8 and fail explicitly otherwise.

### Resilience (`lifecycle.rs`, `cache.rs`)

- **Lazy init** on first use, serialized by a lifecycle mutex and guarded by
  `al.enableCodeAnalysis`. Failed initialization is capped at three attempts and user notification is
  one-shot.
- **Timeout + cooldown (T047):** 30 s per call; on timeout a cooldown gate prevents a thundering-herd
  of retries. It uses a monotonic clock; its stamp and process-wide in-flight gate survive a bridge
  restart, so a hung call from a previous generation cannot be bypassed. Diagnostics, hover, and
  completion invoke generation-checked recovery for fatal bridge failures; a late failure cannot tear
  down a newer bridge.
- **Disk cache** (under `…/al-lsp/semantic/`) stores `builtins()` and `errorCodes()` keyed by
  sanitized toolchain version, turning a ~500 ms CLR call into a cache hit. Each atomic cache envelope
  also stores the exact unsanitized version, schema, and catalog kind, so sanitized-name collisions,
  stale formats, oversized files, partial writes, and corrupt JSON fail closed and regenerate.
- **One-shot user notification** when the bridge fails persistently (the LSP won't spam you).

## Feature gating

The bridge is behind the `semantic` Cargo feature. Release binaries are built with
`--features semantic`, bundling the in-process .NET bridge and `AlBridge.dll`. Without the feature,
`DotNetHost` is a no-op stub returning `NotInitialized`. The `al.enableCodeAnalysis` setting (default
true) further gates whether the bridge is initialized at all.

## Microsoft comparison

The bridge executes Microsoft's `CodeAnalysis` engine and compiler/analyzer diagnostic APIs; it does
not reimplement their binding rules. It is not identical to the complete official AL Language Server:
this project constructs a focused single-document compilation and forwards a package cache, while the
official server owns more project/session configuration and editor behavior. Claims of parity should
therefore be scoped to the CodeAnalysis operations actually exercised by the live contract test, not
the whole official extension.

## Why this approach (and why it is being retired)

Exact compile-time semantics belong to Microsoft, so for diagnostics/hover/completion the project
delegates to the real compiler rather than approximating it. But an in-process CLR is heavy: it adds a
.NET runtime dependency, serializes all calls, and is the slowest part of the editor loop. The native
emitter already removed the bridge from the build path (2026-06-17), and `docs/csharp-bridge-retirement.md`
tracks replacing the rest with native Rust:

1. native semantic diagnostics (replace `analyze`),
2. native type resolver + hover (replace `typeAt`),
3. native member completions (replace `completions_at`),
4. generated builtin catalog (replace `builtins`),
5. generated error-code catalog (replace `errorCodes`),
6. ✅ verified native compile — syntax/project/declaration/declared-binding/integrity checks run
   before emission; expand §1 into procedure-body expression/overload/control-flow parity.

## How to use

You normally don't call the bridge directly — it powers diagnostics, hover, and completion
automatically. To control it:

- `al.enableCodeAnalysis` (default true) — turn the bridge on/off.
- `al.backgroundCodeAnalysis`, `al.diagnosticsTrigger`, `al.diagnosticsScope`, `al.codeAnalyzers`,
  and `al.packageCachePath` — control scheduling, analyzer selection, and dependency lookup (see the
  [settings reference](../reference/settings.md)). The parsed-only settings listed under limitations
  below do not currently affect bridge execution.

## Limitations & roadmap

- No async inside a CLR call (sync mutex); single CLR per process (no multi-toolchain in one session);
  a timeout prevents *new* calls during cooldown but cannot interrupt an in-flight CLR call.
- ⛔ Bridge `compile` is disabled. `al.useOfficialCompiler` selects the maintained `al-compile`
  subprocess backend; it does not route through this FFI bridge.
- Analyzer names resolve only to shipped analyzer DLLs or explicit DLL paths. Custom analyzer DLLs run
  in-process and must be treated as trusted code.
- `al.enableExternalRulesets`, `al.ruleSetPath`, `al.assemblyProbingPaths`, and
  `al.outputAnalyzerStatistics` are parsed settings but are not yet plumbed into this bridge; see
  [gaps and future work](../gaps-and-future-work.md). They must not be described as active controls.
- Roadmap: complete the native replacements above so the .NET dependency can eventually be dropped.

## Verification

- `cargo test -p al-semantic --features semantic` covers boundary validation, cooldown/restart state,
  cache integrity, serialization, and the disabled-feature behavior.
- `AL_TOOL_PATH=<official-extension>/bin/<platform> cargo test -p al-semantic --features semantic --test live_bridge`
  loads the real Microsoft DLL and verifies initialization/health, compiler semantic diagnostics,
  unsaved-buffer type lookup, invalid-position rejection, member completion, shipped CodeCop loading,
  built-ins, and error codes. Set `AL_PACKAGE_CACHE_PATH` as well to exercise package-reference loading.
