# Native `.app` Emitter & Build Pipeline

**Modules:** `crates/al-core/src/emit/` (emitter) + `build.rs`, `publish.rs`, `toolchain.rs`,
`launch.rs`, `config.rs` · **Status:** ✅ shipped (default build path); `alc` fallback retained

This is one of the project's headline differentiators: a **pure-Rust compiler back end** that
produces a deployable Business Central `.app` package directly from source — no `alc`, no .NET
runtime, no C# bridge. It is the default for daemon `compile`, LSP `al.compile`, publish, and native
DAP launch. Microsoft's `alc` remains available as an explicit fallback for full compile-time
semantic validation.

## What an `.app` is, and what the emitter produces

A BC `.app` is a 40-byte **NAVX** header followed by a Deflated **ZIP** containing:
`NavxManifest.xml`, the AL source files, a `SymbolReference.json` (the compiled symbol surface),
OPC metadata (`[Content_Types].xml`, `DocComments.xml`, `MediaIdListing.xml`), an `Entitlements.xml`
(implicit permissions), per-profile symbol-reference files, and an XLIFF localization file. The
native emitter builds all of this from `app.json` + source + referenced package symbols.

## Pipeline (`emit/`)

| Stage | File | What it does |
| --- | --- | --- |
| Orchestrate | `project.rs` | Load `app.json`, scan `.al` files (sorted for determinism), parse external symbols (cached), build `SymbolReference.json`, assemble the `.app`. |
| Extract objects | `symbol_extract.rs` | Walk tree-sitter trees → objects, fields, keys, methods, properties, enum values, page controls/actions, query elements, report layouts, permissions. |
| Build symbol ref | `symbol_reference.rs` | Merge project + external symbols; emit groups in `alc`'s exact order; generate per-object JSON, method IDs, TypeDefinition shapes; per-profile symbol references. |
| Method IDs | `method_id.rs` | Reverse-engineered `alc` method-ID hashing (FNV-1 over UTF-16LE + Microsoft `Hash.Combine`, system-codeunit adjustment). |
| Manifest | `manifest.rs` | `app.json` → `NavxManifest.xml` (defaults, id ranges, dependencies, resource-exposure policy, `alc`'s casing quirks reproduced). |
| Assemble | `assemble.rs` | Generate OPC parts, implicit entitlements (Table→TableData RIMD + Execute; others→Execute), XLIFF (FNV name-hash trans-unit IDs), then write the package. |
| Write package | `package.rs` | Build the 40-byte NAVX header (magic, format v2, random GUID, ZIP length) + Deflated ZIP. |

### Byte-for-byte `alc` compatibility

The hard part of emitting a valid `.app` is matching `alc`'s `SymbolReference.json` exactly —
including method-ID hashes, JSON key ordering, and group emission order. The emitter goes to
considerable lengths:

- `method_id.rs` reproduces `alc`'s FNV-1 hash over **UTF-16LE** method names plus Microsoft's
  `Hash.Combine` (`((h<<5)+h+(h>>27))^h2`) and the system-codeunit positive-range adjustment.
- `serde_json` is configured with `preserve_order` so `Map` keeps insertion order and reproduces
  `alc`'s key ordering.
- The emit test suite compares native output against ALC-shaped fixtures, including **byte-identical
  `SymbolReference.json` golden coverage** for the supported project fixture.
- `.app` selection prefers the manifest-derived `{publisher}_{name}_{version}.app` name.

## Build orchestration (`build.rs`, `toolchain.rs`)

`build.rs` wraps the Microsoft `dotnet alc` path for when it is requested: it runs the compiler in a
per-invocation temp dir (atomic rename of the `.app` into the project root, F-OPEN-058), is async,
cancellable, timeout-aware (`AL_COMPILE_TIMEOUT_SECS`, default 600 s), and `kill_on_drop`. Compiler
output is normalized into structured diagnostics. `toolchain.rs` discovers ALTool/`alc.dll`,
CodeAnalysis, the analyzer DLLs, and `.NET`, sets `DOTNET_ROLL_FORWARD=Major` so net8.0 tools run on
newer runtimes, and locates `altool` for official-LSP delegation.

## Publish (`publish.rs`, `launch.rs`, `bc_client.rs`)

`publish.rs` resolves a `launch.json`/`.zed/debug.json` config, compiles (native by default), and
uploads the `.app` to the BC dev API — optionally via **RAD** incremental deploy when `app.json` has
an id. Each phase (Compile/Upload/Install/Rad) is tracked. `launch.rs` parses the debug configs and
builds dev-endpoint URLs for on-prem vs cloud with tenant validation. `bc_client.rs` is the hardened
REST client (size caps: 500 MB upload / 16 MB JSON / 500 MB binary; error-body redaction of bearer
tokens/passwords/secrets). DAP deploy reuses the same package-selection logic so launch never
publishes a stale `.app`.

## Benchmarks

From `BENCHMARKS.md` (13th-gen i5, identical `.alpackages`, native release build vs `alc` 17.0):

| Project | Objects | Native (median) | `alc` (median) | Speedup |
| --- | --: | --: | --: | --: |
| small | 2 | **329 ms** | 3,720 ms | **11.3×** |
| medium | 40 | **416 ms** | 4,152 ms | **10.0×** |
| large | 200 | **385 ms** | 4,473 ms | **11.6×** |
| xl | 800 | **458 ms** | 5,567 ms | **12.2×** |

The *emit itself* is tiny (~2 ms for 2 objects, ~80 ms for 800); the flat ~330–460 ms floor is
loading the referenced symbols (the 6 MB Base App), which is **cached in-process** keyed on a
fingerprint of the `.app` files. In the long-running daemon (compile-on-save), the first build is
~10–12× faster than `alc` and **every subsequent warm build is ~60–465× faster** (~8 ms for small,
~90 ms for xl).

> **Honest caveat.** `alc` does parse → bind → type-check → emit; the native emitter does parse →
> emit. It *does less work by design* and does **not** perform semantic validation — a
> syntactically-parseable-but-wrong program will be packed into an `.app` the BC server then rejects
> on publish. Validation is covered by the LSP's continuous diagnostics (the semantic bridge) and by
> the BC server on publish. For `alc`'s compile-time guarantees, set `al.useOfficialCompiler: true`.

## Microsoft comparison

| Aspect | This project | Microsoft `alc` |
| --- | --- | --- |
| Implementation | pure Rust, instant startup | .NET app (CLR startup + JIT per invocation) |
| Work performed | parse → emit | parse → bind → type-check → emit |
| Speed (artifact) | 10–12× faster cold, 60–465× warm | baseline |
| Semantic validation | **no** (delegated to LSP + BC server) | **yes** (authoritative) |
| Output fidelity | byte-identical `SymbolReference.json` on the supported fixture | reference |
| Availability | default; runs anywhere | `al.useOfficialCompiler: true` |

## Why this approach

For the editor/CI loop, validation already happens continuously (LSP) and authoritatively (BC server
on publish), so paying `alc`'s full semantic cost on *every* build is wasted time. Making emit a fast,
deterministic step turns "produce the artifact" into a sub-second operation while keeping `alc` one
setting away for when you specifically want compile-time guarantees. The deterministic file ordering
and in-process symbol cache make repeated builds reproducible and near-instant.

## How to use

- **CLI:** `al-explorer compile` (Zed task *AL: Compile*), `al-explorer package` (*AL: Package*),
  `al-explorer pack-native --project <dir> --out <path>` (pure emitter, no compile success/fail
  semantics).
- **LSP:** the `al.compile` execute command.
- **MCP:** `al_build`.
- **Force Microsoft `alc`:** set `al.useOfficialCompiler: true`.

## Limitations & roadmap

- The emitter does **not** validate semantics (by design).
- 🟡 Page/Report/Query object extraction is partially implemented; control-property type inference and
  page-customization bindings are not fully modeled; `DocComments.xml` is emitted empty.
- 🟡 Daemon `package` is still the analyzer-backed Microsoft compiler surface; compile-capable paths
  don't yet share one service abstraction for artifact selection/diagnostics/cancellation/handoff.
- `ROADMAP.md` (Native App Emission): expand fixtures across more object kinds/resources/dependencies/
  profiles/permissions/reports/translations/control add-ins; differential-test against `alc` for
  every fixture; validate emitted packages against a live BC tenant before broadening claims.
