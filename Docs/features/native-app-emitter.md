# Native `.app` Emitter & Build Pipeline

**Modules:** `crates/al-emit/src/` (verification + emitter), `crates/al-compile/src/lib.rs`,
`crates/al-publish/src/lib.rs`, `crates/al-project/src/{toolchain,config}.rs`,
`crates/al-bc/src/{launch,bc_client}.rs`. **Status:** shipped, with limits.
Builds use Microsoft's `alc` when the toolchain is found and fall back to the native emitter when it
is not. `al.useOfficialCompiler` forces either: `true` is `alc` (an error without a toolchain),
`false` is native. The native package still drops page extension variables, `ActionChanges`,
`DocComments.xml` and option members, so on a real customer project it differs from the `alc` package
(see the coverage boundary below and the audit finding `emitter-02-native-app-differs-from-alc-on-dms`).
Every build result names the backend that ran. When auto mode fell back to native it also says why.

The pure-Rust compiler back end produces a Business Central `.app` directly from source without
`alc`, .NET, or the C# bridge. It is the fallback for daemon `compile`, LSP `al.compile`, publish, and
DAP launch when no toolchain is found, and the backend of `pack-native`. Microsoft's `alc` remains available for full compiler semantics and compatibility
validation.

The default native path parses and verifies the project, returns structured file/range diagnostics,
rejects blocking errors, reopens the produced package, and atomically replaces the previous artifact
only after all checks pass. These checks do not require Microsoft tooling.

## What an `.app` is, and what the emitter produces

A BC `.app` is a 40-byte **NAVX** header followed by a Deflated **ZIP** containing:
`NavxManifest.xml`, the AL source files, a `SymbolReference.json` (the compiled symbol surface),
OPC metadata (`[Content_Types].xml`, `DocComments.xml`, `MediaIdListing.xml`), an implicit
entitlement under `entitlement/<app-id>.xml` when permissions exist, per-profile
symbol-reference files, and, when supported translatable properties are
present, an XLIFF localization file. The native emitter builds the verified subset
from `app.json` + source + referenced package symbols.

Current XLIFF extraction covers supported object/field captions, named page and
request-page control captions/ToolTips, page-action captions/ToolTips, and
page-extension added-control captions/ToolTips and modified-control ToolTips at
their runtime gates. The current self-contained and Base Application fixtures
match `alc` byte-for-byte, including trans-unit IDs/order, object targets, notes,
and metadata. Report-layout captions are intentionally not extracted because
current `alc` 17 omits them from XLIFF. Declared app logos and report layouts are
copied byte-for-byte from validated project-relative paths. Loose files merely
placed under `res/` are not copied. Current `alc` 17 evidence omitted the measured
`.res` input as well. Resource shapes outside these measured contracts use the
explicit Microsoft compatibility profile.

## Pipeline (`crates/al-emit/src/`)

| Stage | File | What it does |
| --- | --- | --- |
| Orchestrate | `project.rs` | Load `app.json`, scan `.al` files (sorted for determinism), parse external symbols (cached), build `SymbolReference.json`, assemble the `.app`. |
| Extract objects | `symbol_extract.rs` | Walk tree-sitter trees → objects, fields, keys, methods, properties, enum values, page controls/actions, query elements, report layouts, permissions. |
| Build symbol ref | `symbol_reference.rs` | Merge project + external symbols. Emit groups in `alc`'s exact order. Generate per-object JSON, method IDs, TypeDefinition shapes. Per-profile symbol references. |
| Method IDs | `method_id.rs` | Reverse-engineered `alc` method-ID hashing (FNV-1 over UTF-16LE + Microsoft `Hash.Combine`, system-codeunit adjustment). |
| Manifest | `manifest.rs` | `app.json` → `NavxManifest.xml` (defaults, id ranges, dependencies, resource-exposure policy, `alc`'s casing quirks reproduced). |
| Assemble | `assemble.rs` | Generate OPC parts, implicit entitlements (Table→TableData RIMD + Execute, others→Execute), XLIFF (FNV name-hash trans-unit IDs), then write the package. |
| Write package | `package.rs` | Build the 40-byte NAVX header (magic, format v2, random GUID, ZIP length) + Deflated ZIP. |

### `alc` compatibility

Compatibility depends heavily on the shape and identifiers in `SymbolReference.json`:

- `method_id.rs` reproduces `alc`'s FNV-1 hash over **UTF-16LE** method names plus Microsoft's
  `Hash.Combine` (`((h<<5)+h+(h>>27))^h2`) and the system-codeunit positive-range adjustment.
- `serde_json` is configured with `preserve_order` so `Map` keeps insertion order and reproduces
  `alc`'s key ordering.
- A focused golden fixture checks serialized `SymbolReference.json` bytes. The differential fixture
  compares parsed JSON values because archive metadata and output ordering are not a general
  byte-for-byte package contract. The benchmark harness additionally compares complete entry sets,
  JSON semantics, manifest semantics apart from build provenance, and navigation semantics apart
  from the deliberately random control GUID.
- The env-gated Base Application differential requires no checked-in Microsoft
  packages. Point `AL_TOOL_PATH` at the directory containing `alc.dll` and
  `AL_PACKAGE_CACHE_PATH` at an existing package-cache directory, then run
  `make microsoft-contracts` with `AL_TOOL_PATH` and
  `AL_PACKAGE_CACHE_PATH` set.
  Known to pass on dotnet tool 18.0.43.1464 (`tools/net8.0/any`), checked on 2026-10-27. Under heavy
  machine load the `zed_simulation` builtin tests need `AL_TEST_REQUEST_TIMEOUT_MS=120000` and
  `AL_TEST_DIAG_TIMEOUT_MS=90000`. The Cursor extension 18.1.2842859 passes `emit_differential` and
  `pack_native_validate`, but the profile stops at its first step there: the semantic bridge cannot
  load that CodeAnalysis dll. Interface ids follow alc's `CalculateInterfaceId` except the step that
  folds in extended interface ids, which is not ported.
  The fixture covers external page/table/report bindings, direct `modify`
  properties, page customization defaults, a report layout, and an app logo.
- `.app` selection prefers the manifest-derived `{publisher}_{name}_{version}.app` name.

## Build orchestration (`crates/al-compile/src/lib.rs`, `al-project/src/toolchain.rs`)

`al-compile` wraps the Microsoft `dotnet alc` path for when it is requested: it runs the compiler in a
per-invocation temporary directory and moves the completed `.app` into the project root. It is
asynchronous, cancellable, timeout-aware (`AL_COMPILE_TIMEOUT_SECS`, default 600 s), and
`kill_on_drop`. Compiler
output is normalized into structured diagnostics. `toolchain.rs` discovers ALTool/`alc.dll`,
CodeAnalysis, the analyzer DLLs, and `.NET`, sets `DOTNET_ROLL_FORWARD=Major` so net8.0 tools run on
newer runtimes, and locates `altool` for `al.useOfficialLsp` (altool `launchlspserver`).

## Native verification gate (`verification.rs`)

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
| Project input | parseable `app.json`. Non-empty identity fields. Valid app/dependency GUIDs and four-part versions. Valid `idRanges`. Unique dependency IDs. Readable source files. Declared dependency packages present in the exact configured package paths. Object IDs inside `idRanges` | `ALN01xx` and project diagnostics block before emission. |
| Syntax | tree-sitter `ERROR` and missing nodes in every `.al` file, including truncated constructs | A statement without its `;` that is followed by another statement reports alc's `AL0111` "Semicolon expected. Add a semicolon (;) to terminate the statement." on the first token of the next statement, the range alc reports (checked against alc 18.1.43 errorlogs in `crates/al-syntax/tests/fixtures/alc`). Every other syntax error reports `ALN0001` with the parser's message: a missing `)` (alc `AL0104` "Syntax error, ')' expected" on the next token, native range has zero width), a missing `end` (alc `AL0104` twice, native reports a cascade of several errors), a missing `;` where the next statement spans more than one token in the tree (for example a following `if`), and all other shapes. Both carry file and exact 1-based UTF-16 start/end range and always block. The editor diagnostic uses the same table, so its code is `AL0111` for the mapped case and `syntax` otherwise (finding journey-15). A file that is not valid UTF-8 blocks with `ALN0002` on that file rather than failing the build. |
| Declarations | duplicate object IDs/names. Duplicate field IDs/names. Duplicate enum ordinals/names. Duplicate procedure signatures/parameter names. Unknown key/field-group fields | `ALN1xxx` diagnostics point at the complete object declaration range and block emission. |
| Declared bindings and contracts | extension targets, implemented interfaces, declared object subtypes (`Record`, `Page`, `Codeunit`, `Report`, `XmlPort`, `Query`, `Enum`, `Interface`) and `SourceTable` references resolve against project plus dependency symbols. Locally declared interface methods must be implemented | `ALN2xxx` diagnostics block emission. |
| Permissions | permission object kinds/flags are validated and permission targets resolve against project plus dependency symbols | `ALN21xx` diagnostics block emission. |
| Local procedure/event semantics | local call overload arity and ambiguity. Value/no-value returns and scalar literal return types. `Break`/`Continue` without a loop. Locally sourced publisher/subscriber parameter contracts | `ALN22xx`/`ALN23xx` diagnostics block emission. Publisher bodies/contracts absent from source-free dependency packages are an explicit Microsoft compatibility boundary. |
| Artifact integrity | reopen NAVX/ZIP. Reject duplicate/empty required entries. Compare the exact embedded AL source-path set with the verified snapshot. Parse the generated manifest and `SymbolReference.json`. Compare package id/name/publisher/version with `app.json` | `ALN3xxx` failures discard the staged artifact. |

### Native diagnostic codes

The JSON shape is stable across `pack-native`, shared `CompileResult`, daemon/MCP compile, and LSP
publication: `file`, `line`, `column`, `endLine`, `endColumn`, `severity`, `code`, and `message`.
Positions are 1-based at the build/daemon boundary and converted to 0-based UTF-16 ranges for LSP.
The Microsoft `alc` text parser cannot recover an exact end range, so its `endLine`/`endColumn`
remain `null`. Native diagnostics preserve both endpoints.

| Codes | Meaning |
| --- | --- |
| `ALN0000`–`ALN0002` | Native build infrastructure failure, AL syntax error, or a source file that is not valid UTF-8. |
| `ALN0100`–`ALN0106` | Invalid JSON, missing/invalid identity fields, invalid ranges/dependencies, or duplicate dependency IDs. |
| `ALN1001`–`ALN1003`, `ALN1007`, `ALN1008` | Duplicate object ID or name, an ID outside `idRanges`, a declared dependency with no loaded package, or two objects whose names map to the same package entry name. |
| `ALN1101`–`ALN1107` | Duplicate fields, enum values, procedures/parameters, or invalid key/field-group field references. |
| `ALN2001`–`ALN2004` | Unresolved extension target, interface, declared object subtype, or `SourceTable`. |
| `ALN2101`–`ALN2106` | Unsupported permission object type, invalid permission flags, unresolved permission target, a missing local interface member, forbidden page-customization ToolTip, or a duplicate locally added page control. |
| `ALN2201`–`ALN2211` | Local procedure contracts: no overload or more than one for the argument count, a Record passed to an Integer parameter, `Exit` with or without a value against the declared return, an incompatible literal return, a conditional `Exit(value)` with no unconditional return, `Break`/`Continue` outside a loop, an unknown local or Record method. |
| `ALN2301` | An `[EventSubscriber]` whose parameters do not match a locally declared publisher. |
| `ALN2401`–`ALN2405` | Body binding: unknown Record subtype, undeclared identifier, incompatible assignment, a Record field that neither the table nor any of its table extensions declares, an unknown field property. |
| `ALN2501` | A report layout that names a file outside the project. |
| `ALN3001`–`ALN3006` | Unreadable/corrupt artifact, missing/empty or duplicate entries, source-snapshot mismatch, generated metadata parse failure, or package identity mismatch. |

`build_verified_app_from_project` preserves all structured diagnostics.
`build_app_from_project` is the simpler API and also refuses invalid input. `native_compile` maps the
native diagnostics into the shared `CompileResult`, writes through a staged temporary file, syncs it,
and atomically persists it. Daemon compile/package, LSP `al.compile`, MCP `al_build`, publish, DAP
builds and `pack-native` therefore share the same native correctness boundary.

Workspace-aware daemon builds add `al-analysis::native_workspace_diagnostics` before emission. With
native lint enabled (the default), its `AL-NC*` object/project checks and resolved call/event-graph
transaction rules are shared with editor diagnostics. Error-severity findings block the build and
warnings are returned with the successful result. The daemon reports
`verificationLevel: native-syntax-project-binding-symbol-graph`. Direct `pack-native` reports the
lower, always-on `native-syntax-project-binding` gate.

### Build profiles

| Profile | Default? | Microsoft dependency | Intended use |
| --- | --- | --- | --- |
| **Native verified** | Fallback | None | Run the shipped syntax/project/declaration/binding/integrity checks, then emit. |
| **Native + compatibility** | No | Required for the extra check | `al-explorer pack-native --validate`: run native verification first, then ask `alc` for authoritative compatibility diagnostics before writing the native artifact. |
| **Official build** | Yes, when the toolchain is found | Required | `al.useOfficialCompiler` unset or `true`: `alc` both validates and emits. |

Microsoft compatibility checking does not sit on the default critical path:

- never require .NET/ALTool for the default native profile.
- Run native syntax/project checks first, so obviously broken code never starts the CLR or `alc`.
- Invoke it only for explicit `--validate` compatibility checks or the explicit official build.
- Run it once per project, never once per file.
- Use differential verification in this repository's test suite: compare native diagnostics and
  accept/reject decisions with `alc` across fixtures, then turn mismatches into focused native
  verifier work.

If `--validate` is requested but Microsoft tooling is unavailable, the command fails closed. It does
not silently downgrade. This keeps Microsoft tooling an oracle and compatibility safety net rather
than an architectural dependency.

### Performance measurement

Verification parses each file once and package assembly reuses that tree-derived model. Referenced
package symbols remain cached in-process by the selected package-path fingerprint, preserving the
dominant warm-build optimization. The release harness measures process/package-index-cold,
warm-unchanged, and one-file-edit states, alternates backend order, discards round zero, and records
machine load for every sample. “Process cold” means a fresh benchmark process and fresh in-process
indexes. It does not claim an OS page-cache flush. Native builds publish their input, dependency
index, verification, emission, artifact-check, and output-write phases. Microsoft `alc` exposes
only total wall-clock time, so its total is never presented as a phase-equivalent comparison.

## Publish (`al-publish`, `al-bc/src/launch.rs`, `al-bc/src/bc_client.rs`)

`al-publish` resolves a `launch.json`/`.zed/debug.json` config, compiles (native by default), and
uploads the `.app` to the BC dev API, optionally through **RAD** incremental deploy when `app.json` has
an id, which must be a GUID. Each phase (`PublishPhase::Compile`/`Upload`/`Rad`) is tracked.
`launch.rs` parses the debug configs and builds dev-endpoint URLs for on-prem vs cloud with tenant
validation, sent as the BC dev API's documented `?tenant=` query parameter. `bc_client.rs` is the
hardened REST client (size caps: 500 MB upload / 16 MB JSON / 500 MB binary, error-body redaction
of bearer tokens, Basic credentials, passwords and secrets, case-insensitively).

Reachable as `al-explorer publish [--config <name>] [--incremental]`, which calls the daemon's
`publish` method. That method calls `al_publish::publish` and adds no pipeline of its own.

Native DAP launch does **not** go through `al-publish`. `al-dap`'s `bc_debug/rest.rs` and
`al-publish` both post a multipart form to `{base}/dev/apps` (Microsoft's route,
`AppsApiClient.cs:38-72`), `al-publish` with credentials from `BC_ACCESS_TOKEN` or
`BC_USERNAME`/`BC_PASSWORD`, the debugger with the OAuth token from its own sign-in. Publish
succeeds when BC answers with an HTTP success status. The response body is not read, as in
Microsoft's client (`AppsApiClient.cs:66`). RAD is the same route with `IsRad=true`. Not yet run
against a live server yet.

`bc_client.rs` implements only that one BC dev endpoint, `POST /dev/apps`. It does
**not** implement extension install/uninstall, an application-status query, or an AAD device-code
sign-in flow. `AuthMethod::AAD` requires a pre-provisioned bearer token in `BC_ACCESS_TOKEN` (or the
legacy `BC_TOKEN`) and otherwise fails fast with `MissingCredentials`. `AuthMethod::Windows`
authenticates via plain HTTP Basic using `BC_USERNAME`/`BC_PASSWORD`. It is **not** an
NTLM/Negotiate handshake, so a BC server that requires genuine Windows-integrated auth (and rejects
a Basic fallback) will not authenticate through this client.

## Benchmarks

[`BENCHMARKS.md`](../../BENCHMARKS.md) records the comparative measurement contract and current
release evidence.
[`Docs/benchmarks.md`](../benchmarks.md) documents the deterministic Criterion benchmarks used for
native parser, interpreter, symbol-index, graph, and completion regressions. Quantitative
native-versus-`alc` claims are made only from committed raw results that satisfy every validity and
package-semantic-equivalence check.

> **Coverage boundary.** The native verifier rejects syntax, project/declaration integrity, declared
> symbol bindings, deterministic local interface contracts, permission targets/flags, and locally
> resolvable procedure/event contracts. Microsoft-wide type inference, path-sensitive reachability,
> source-free package event binding, and analyzer policy remain compatibility-gated. Use `pack-native --validate` for an
> additional Microsoft compatibility gate or `al.useOfficialCompiler: true` when exact `alc`
> semantics are required.
>
> **Real projects differ from `alc`.** Archive, manifest and symbol parity is measured on the fixtures
> only. On a real customer project (139 objects) the native package differed from the `alc` 18.1
> package in 107 objects. What the native emitter drops or gets wrong: page extension variables,
> `ActionChanges`, `DocComments.xml`, option members,
> `CalcFormula` text (truncated), method parameter `Temporary`, XLIFF units (208 against 347), and
> entry names for source folders with
> spaces (`alc` writes `%2520`, native writes a raw space). The `TargetObject` app id of extensions
> (wrong in 13 cases) and manifest `Features` were fixed after that measurement. Do not give a native package to Business
> Central. Builds therefore use `alc` by default when it is installed. Details: finding
> `emitter-02-native-app-differs-from-alc-on-dms`.
>
> **The verifier warns, it does not prove.** The text checks for local procedure calls, return
> values, overloads, `Continue`, identifiers in field triggers and record fields (ALN2202, ALN2203,
> ALN2208, ALN2209, ALN2402, ALN2404) cannot be exact without a binder. They are reported as
> warnings and do not stop the build (finding `emitter-01-native-verifier-rejects-valid-dms-project`).
> A clean native build does not mean `alc` would accept the project.

## Microsoft comparison

| Aspect | This project | Microsoft `alc` |
| --- | --- | --- |
| Implementation | pure Rust | .NET application |
| Work performed | parse → native verify → emit → package integrity check | parse → bind → type-check → emit |
| Comparative performance | Native phase telemetry plus total wall time | Total wall time (no comparable phase telemetry exposed) |
| Build-time validation | partial: native syntax/project/declaration/declared-binding checks block the build, heuristic body checks only warn | **yes**: authoritative Microsoft semantics |
| Optional parity | `pack-native --validate` runs native first, then `alc` | reference |
| Output fidelity | Archive/manifest/symbol parity is measured on the self-contained, dependency, small-through-XL, and focused Base Application fixtures only. Byte-identical XLIFF on translatable fixtures. A real customer project differs from `alc` (see the coverage boundary) | reference |
| Availability | fallback when `alc` is absent. Runs anywhere | default when found, or `al.useOfficialCompiler: true` |

## Why this approach

Paying `alc`'s startup and full semantic cost on every edit gives up much of the native toolchain's
advantage, but packaging unchecked code and relying on a later server rejection is not a complete
compiler workflow. The intended balance is a fast, deterministic native verification-and-emission
path for every build, with Microsoft validation used deliberately at the documented compatibility
boundaries. Deterministic file ordering, content/dependency fingerprints and in-process caches make
repeated verified builds reproducible without weakening the verification gate.

## How to use

- **CLI:** `al-explorer compile`, `al-explorer package`, and
  `al-explorer pack-native --project <dir> --out <path>` (verified native build). Add `--validate`
  for the Microsoft compatibility gate and global `--json` for machine-readable success metadata
  and full diagnostics. Validation failure exits non-zero and writes no `.app`.
- **LSP:** the `al.compile` execute command.
- **MCP:** `al_build` is the named compile alias. The other shared build dispatcher methods, including
  `package`, are available through `al_call`.
- **Backend:** `al-explorer compile` (alias `build`) uses `alc` when the toolchain is found. Set
  `al.useOfficialCompiler: true` to require `alc` or `false` to force the native emitter.

## Compatibility boundaries

- Native verification is wired into every native build surface and invalid projects cannot replace
  the last good artifact.
- Microsoft-wide expression type inference/overload selection, path-sensitive control-flow
  reachability, source-free package event binding, and analyzer policy remain compatibility checks. Native and authoritative
  diagnostics remain distinguishable.
- XLIFF trans-unit IDs/order, metadata, and the measured extension-field/control extraction shape
  are locked to the current `alc` oracle. Base-app control/property/page-customization bindings,
  navigation name defaults/translation keys, and declared app-logo/report-layout resources have
  focused differential coverage. `DocComments.xml` is emitted and covered by current fixtures.
- Compile-capable paths share one `BuildRequest`, compiler-setting conversion,
  cancellation/timeout policy, exact configured dependency package selection,
  structured result, and atomic artifact handoff. Workspace-only graph
  diagnostics remain at the LSP/daemon transport boundary because those
  processes own the live workspace index.
- Business Central tenant publish/install/runtime behaviour is validated by
  the strict `make live-bc-contracts` profile. Missing inputs are unavailable
  (exit 2), not a skipped pass, and tenant behavior cannot be inferred from an
  offline package comparison.
