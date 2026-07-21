# Native `.app` Emitter & Build Pipeline

**Modules:** `crates/al-emit/src/` (verification + emitter) + `crates/al-compile/src/`,
`publish.rs`, `toolchain.rs`, `launch.rs`, `config.rs` · **Status:** ✅ shipped (verified native
build is the default); `alc` compatibility/fallback retained

This is one of the project's headline differentiators: a **pure-Rust compiler back end** that
produces a deployable Business Central `.app` package directly from source — no `alc`, no .NET
runtime, no C# bridge. It is the default for daemon `compile`, LSP `al.compile`, publish, and native
DAP launch. Microsoft's `alc` remains available as an explicit fallback for full compile-time
semantic validation.

> **Verified native builds.** Fast packaging alone is not called “compile”. The default native path
> parses and verifies the project, returns structured file/range diagnostics, rejects definite
> errors, reopens the produced package, and atomically replaces the previous artifact only after all
> checks pass. None of those checks requires Microsoft tooling.

## What an `.app` is, and what the emitter produces

A BC `.app` is a 40-byte **NAVX** header followed by a Deflated **ZIP** containing:
`NavxManifest.xml`, the AL source files, a `SymbolReference.json` (the compiled symbol surface),
OPC metadata (`[Content_Types].xml`, `DocComments.xml`, `MediaIdListing.xml`), an implicit
entitlement under `entitlement/<app-id>.xml` when permissions exist, per-profile
symbol-reference files, and an XLIFF localization file. The
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

## Build orchestration (`crates/al-compile/src/lib.rs`, `toolchain.rs`)

`al-compile` wraps the Microsoft `dotnet alc` path for when it is requested: it runs the compiler in a
per-invocation temp dir (atomic rename of the `.app` into the project root, F-OPEN-058), is async,
cancellable, timeout-aware (`AL_COMPILE_TIMEOUT_SECS`, default 600 s), and `kill_on_drop`. Compiler
output is normalized into structured diagnostics. `toolchain.rs` discovers ALTool/`alc.dll`,
CodeAnalysis, the analyzer DLLs, and `.NET`, sets `DOTNET_ROLL_FORWARD=Major` so net8.0 tools run on
newer runtimes, and locates `altool` for official-LSP delegation.

## Native verification gate (`verification.rs`) — shipped

The emitter consumes one source snapshot and uses the same parse tree for verification and symbol
extraction. A native build succeeds only when the verifier has no blocking errors.

```text
source snapshot + app.json + dependency fingerprint
             │
             ▼
  native parse / project verification ──► structured diagnostics
             │ no blocking errors
             ▼
        symbol extraction ──► package assembly ──► reopen/integrity check ──► atomic replace
```

The shipped verifier performs:

| Layer | Checks | Build behaviour |
| --- | --- | --- |
| Project input | valid `app.json` required fields, readable source files, declared dependency packages present in `.alpackages`, object IDs inside `idRanges` | Definite errors block before emission. |
| Syntax | tree-sitter `ERROR` and missing nodes in every `.al` file, including truncated constructs | `ALN0001` carries file and exact range and always blocks. |
| Declarations | duplicate object IDs/names; duplicate field IDs/names; duplicate enum ordinals/names; duplicate procedure signatures/parameter names; unknown key/field-group fields | `ALN1xxx` diagnostics block emission. |
| Declared bindings | extension targets, implemented interfaces, declared object subtypes (`Record`, `Page`, `Codeunit`, `Report`, `XmlPort`, `Query`, `Enum`, `Interface`) and `SourceTable` references resolve against project plus dependency symbols | `ALN2xxx` diagnostics block emission. |
| Artifact integrity | reopen NAVX/ZIP; require manifest, symbols, content types, doc comments and media listing; verify the embedded AL-source count | `ALN3xxx` failures discard the staged artifact. |

`build_verified_app_from_project` preserves all structured diagnostics.
`build_app_from_project` is the simpler API and also refuses invalid input. `native_compile` maps the
native diagnostics into the shared `CompileResult`, writes through a staged temporary file, syncs it,
and atomically persists it. Daemon compile/package, LSP `al.compile`, MCP `al_build`, publish, DAP
builds and `pack-native` therefore share the same native correctness boundary.

Workspace-aware daemon builds add `al-analysis::native_workspace_diagnostics` before emission. With
native lint enabled (the default), its `AL-NC*` object/project checks and resolved call/event-graph
transaction rules are shared with editor diagnostics; error-severity findings block the build and
warnings are returned with the successful result. The daemon reports
`verificationLevel: native-syntax-project-binding-symbol-graph`; direct `pack-native` reports the
lower, always-on `native-syntax-project-binding` gate.

### Build profiles

| Profile | Default? | Microsoft dependency | Intended use |
| --- | --- | --- | --- |
| **Native verified** | Yes | None | Run the shipped syntax/project/declaration/binding/integrity checks, then emit. |
| **Native + compatibility** | No | Required for the extra check | `al-explorer pack-native --validate`: run native verification first, then ask `alc` for authoritative compatibility diagnostics before writing the native artifact. |
| **Official build** | No | Required | Existing `al.useOfficialCompiler: true` path where `alc` both validates and emits. |

Microsoft compatibility checking does not sit on the default critical path:

- never require .NET/ALTool for the default native profile;
- run native syntax/project checks first, so obviously broken code never starts the CLR or `alc`;
- invoke it only for explicit `--validate` compatibility checks or the explicit official build;
- run it once per project, never once per file;
- use differential verification in this repository's test suite: compare native diagnostics and
  accept/reject decisions with `alc` across fixtures, then turn mismatches into focused native
  verifier work.

If `--validate` is requested but Microsoft tooling is unavailable, the command fails closed; it does
not silently downgrade. This keeps Microsoft tooling an oracle and compatibility safety net rather
than an architectural dependency.

### Performance contract

Verification parses each file once and package assembly reuses that tree-derived model. Referenced
package symbols remain cached in-process by `.alpackages` fingerprint, preserving the dominant warm
build optimization. Benchmarks must report verification separately from emission and include cold,
warm-unchanged and one-file-edit cases; the historical figures below predate the verification gate
and must be refreshed before being presented as verified-build measurements.

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

> **Coverage boundary.** The native verifier rejects syntax, project/declaration integrity and the
> declared-symbol binding errors listed above. It does not yet reproduce every Microsoft expression,
> overload, control-flow, permission, event and analyzer rule. Use `pack-native --validate` for an
> additional Microsoft compatibility gate or `al.useOfficialCompiler: true` when exact `alc`
> semantics are required.

## Microsoft comparison

| Aspect | This project | Microsoft `alc` |
| --- | --- | --- |
| Implementation | pure Rust, instant startup | .NET app (CLR startup + JIT per invocation) |
| Work performed | parse → native verify → emit → package integrity check | parse → bind → type-check → emit |
| Speed (historical emit-only benchmark) | 10–12× faster cold, 60–465× warm; verified-build refresh required | baseline |
| Build-time validation | **yes** — shipped native syntax/project/declaration/declared-binding checks | **yes** — authoritative Microsoft semantics |
| Optional parity | `pack-native --validate` runs native first, then `alc` | reference |
| Output fidelity | byte-identical `SymbolReference.json` on the supported fixture | reference |
| Availability | default; runs anywhere | `al.useOfficialCompiler: true` |

## Why this approach

Paying `alc`'s startup and full semantic cost on every edit gives up much of the native toolchain's
advantage, but packaging unchecked code and relying on a later server rejection is not a complete
compiler workflow. The intended balance is a fast, deterministic native verification-and-emission
path for every build, with Microsoft validation used deliberately for parity, release confidence and
still-unsupported language areas. Deterministic file ordering, content/dependency fingerprints and
in-process caches should make repeated verified builds reproducible and near-instant.

## How to use

- **CLI:** `al-explorer compile` (Zed task *AL: Compile*), `al-explorer package` (*AL: Package*),
  `al-explorer pack-native --project <dir> --out <path>` (verified native build), and add
  `--validate` for the Microsoft compatibility gate.
- **LSP:** the `al.compile` execute command.
- **MCP:** `al_build` is the named compile alias; the other shared build dispatcher methods, including
  `package`, are available through `al_call`.
- **Force Microsoft `alc`:** set `al.useOfficialCompiler: true`.

## Limitations & roadmap

- ✅ Native verification is wired into every native build surface and invalid projects cannot replace
  the last good artifact.
- 🟡 Expand from declared-symbol binding into procedure-body expression/overload/control-flow checks;
  keep native verification and authoritative Microsoft compatibility results distinguishable.
- 🟡 Control-property type inference and page-customization bindings against base-app symbols need
  broader differential coverage; `DocComments.xml` is emitted and covered by current fixtures.
- 🟡 Compile-capable paths share the core build service and verifier, but cancellation/configuration
  policy and workspace-only graph diagnostics still need one uniform request abstraction.
- `ROADMAP.md` (Native App Emission): expand fixtures across more object kinds/resources/dependencies/
  profiles/permissions/reports/translations/control add-ins; differential-test against `alc` for
  every fixture; expand invalid expression/overload/control-flow fixtures; benchmark verified
  cold/warm/one-file-edit builds; validate emitted packages against a live BC tenant before
  broadening claims.
