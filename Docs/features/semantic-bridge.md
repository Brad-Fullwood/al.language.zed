# Semantic Bridge (.NET CodeAnalysis)

**Module:** `crates/al-semantic/src/` + `crates/al-semantic/bridge/Bridge.cs`, `AlBridge.csproj` ·
**Status:** ✅ shipped and integration-tested (feature-gated) · **being retired** in favor of native Rust

The semantic bridge is the project's link to Microsoft's actual AL compiler semantics. When you want
*compiler-grade* diagnostics, type info, and completions — the things that need real name binding and
type checking — the bridge loads the .NET CLR **in-process** and calls Microsoft's `CodeAnalysis` API
through a JSON-over-FFI boundary. No subprocess, no socket: the bridge DLL and the Rust code share one
process and communicate via C-ABI function pointers.

## What it provides

| Bridge method | Used for | Rust caller |
| --- | --- | --- |
| `analyze(file, source, analyzers[], packageCache)` | compilation diagnostics + CodeCop/UICop/AppSourceCop/PerTenantCop | `server/diagnostics.rs` (phase 2) |
| `typeAt(file, pos, [unsavedText], packageCache)` | hover fallback (semantic type at cursor) | `queries/hover.rs` |
| `completions_at(file, pos, [unsavedText], packageCache)` | member-access completion fallback | `queries/completions.rs` |
| `builtins()` | built-in types/methods catalog | LSP startup → `SemanticCache` |
| `errorCodes()` | error-code → severity/message catalog | diagnostic message enrichment |
| `compile(...)` | legacy Rust API; fails closed with `-32601` | **retired** — builds use `al-compile` |
| `ping()` | health check | lifecycle |

## How it works

### Hosting & threading (`host.rs`, `bridge.rs`)

`DotNetHost` loads `hostfxr`, initializes the CLR from a runtime config, and obtains delegates for the
`Bridge.cs` entry points (`Init`, `GetLastError`, `HandleRequest`, `FreeBuffer`). Every request is JSON in both
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

- **Position:** LSP 0-based UTF-16 `(line, column)` is passed through unchanged. .NET string
  indexes are UTF-16 code units. The bridge validates that the line exists and that the column is on
  that line (including its end position); an invalid column cannot spill into a later line, and EOF is
  not shifted back to the previous character.
- **Unsaved text:** when `text` is supplied, the bridge uses the editor buffer instead of
  reading disk, so hover/completion reflect unsaved edits.
- **Project context:** hover and completion forward `al.packageCachePath`, or the discovered project's
  package directory, so dependency symbols participate in binding.
- **Resource limits:** 16 MiB per document, 32 MiB per JSON request, 256 MiB max response. Paths crossing
  the JSON/FFI boundary must be valid UTF-8 and fail explicitly otherwise.

### Resilience

- **Workspace lifecycle** (`al-workspace/src/semantic_lifecycle.rs`) performs lazy initialization,
  serializes restart attempts, caps repeated initialization failures, and emits one user
  notification for persistent failure.
- **Timeout and cooldown** (`al-semantic/src/bridge.rs`) apply a 30-second call timeout and prevent a
  retry stampede. Generation checks prevent a late failure from tearing down a newer bridge.
- **Disk cache** (`al-semantic/src/cache.rs`, under `…/al-lsp/semantic/`) stores `builtins()` and
  `errorCodes()` keyed by
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

## Native and Microsoft boundary

The bridge is optional Microsoft enrichment, not a prerequisite for the native
language server. Native workspace diagnostics, type/scope resolution, hover,
member completion, the generated builtin-function catalog, symbol/package
navigation, and verified `.app` emission all operate without a CLR. When a
native query has a sound result it wins; the bridge is consulted only for
additional Microsoft type/completion detail or exact CodeAnalysis diagnostics.

Version-specific Microsoft error-code descriptions and the complete built-in
type/member catalog are cached from the installed toolchain. Without that
toolchain, diagnostics still carry their native code and message and generated
builtin functions remain available; the server does not invent Microsoft-only
catalog entries.

The same boundary applies to builds: the default verified native pipeline is
independent of this FFI bridge, while `al.useOfficialCompiler` deliberately
invokes the real `alc` subprocess when exact Microsoft compiler/analyzer
compatibility is required.

## How to use

You normally don't call the bridge directly — it powers diagnostics, hover, and completion
automatically. To control it:

- `al.enableCodeAnalysis` (default true) — turn Microsoft bridge enrichment on/off.
- `al.backgroundCodeAnalysis`, `al.diagnosticsTrigger`, `al.diagnosticsScope`, `al.codeAnalyzers`,
  and `al.packageCachePath` — control scheduling, analyzer selection, and dependency lookup (see the
  [settings reference](../reference/settings.md)).

## Limitations

- No async inside a CLR call (sync mutex); single CLR per process (no multi-toolchain in one session);
  a timeout prevents *new* calls during cooldown but cannot interrupt an in-flight CLR call.
- ⛔ Bridge `compile` is disabled. `al.useOfficialCompiler` selects the maintained `al-compile`
  subprocess backend; it does not route through this FFI bridge.
- Analyzer names resolve only to shipped analyzer DLLs or explicit DLL paths. Custom analyzer DLLs run
  in-process and must be treated as trusted code.
- `al.enableExternalRulesets`, `al.ruleSetPath`, `al.assemblyProbingPaths`, and
  `al.outputAnalyzerStatistics` intentionally apply to the official `alc` backend, where Microsoft
  defines their behavior; they do not alter this focused per-document bridge.

## Verification

- `cargo test -p al-semantic --all-features` covers boundary validation, cooldown/restart state,
  cache integrity, serialization, managed initialization error propagation, and the
  disabled-feature behavior.
- `cargo test -p al-workspace --features semantic` covers workspace initialization, restart, and
  notification behavior.
- `AL_TOOL_PATH=<official-extension>/bin/<platform> AL_PACKAGE_CACHE_PATH=<project>/.alpackages make microsoft-contracts`
  loads the real Microsoft DLL and verifies initialization/health, compiler semantic diagnostics,
  unsaved-buffer type lookup, invalid-position rejection, member completion, shipped CodeCop loading,
  built-ins, and error codes. Set `AL_PACKAGE_CACHE_PATH` as well to exercise package-reference loading.
- `AL_TOOL_PATH=<official-extension>/bin/<platform> make record-methods` regenerates the checked-in,
  shared AL `Record` method catalog from Microsoft's `TableClass` metadata; syntax highlighting and
  native verification consume that same catalog instead of separate hand-maintained lists.
- `scripts/check-release-hygiene.sh --full-regenerate` finds that same CodeAnalysis DLL below the
  pinned `AL_EXTENSION_PATH` and requires a byte-identical catalog as part of generated-asset CI.
- `cargo test -p al-workspace`, `cargo test -p al-analysis`, `cargo test -p al-test`, and
  `cargo test -p al-lsp --lib --features semantic` are the consumer finish gate. They prove that the
  lifecycle, hover/completion, diagnostics, test routing/runtime, and semantic-enabled LSP wiring
  compile and pass together; a passing bridge-only test is not considered sufficient.

The full finish gate and the package-backed live contract were last run successfully on 2026-07-21.
