# Spike: AL compiler / `.app` emit feasibility

**Date:** 2026-06-15 - **Status:** complete - **Owner:** native-first initiative

## Goal

Decide whether a "true native" AL compile path - emitting a deployable `.app`
**without shelling out to Microsoft's `alc` subprocess** - is feasible, before
committing to any reimplementation. This was the "feasibility spike first" gate
chosen for the native-first compiler work.

## Environment

- AL toolchain **v17.0.34.45391** installed at `~/.local/bin/.store/.../tools/net8.0/any` (`alc.dll`, `altool.dll`, `Microsoft.Dynamics.Nav.CodeAnalysis.dll`, and - notably - `Microsoft.CodeAnalysis.dll` + `Microsoft.CodeAnalysis.CSharp.dll`, i.e. Roslyn C#).
- .NET 10 SDK (roll-forward from the net8.0 toolchain).
- Real Microsoft symbol `.app`s (v26.5) as fixtures under `crates/al-test-harness/data/test_al_project/.snapshots/.alpackages/`.

## What a `.app` actually contains

A `.app` is a `NAVX` header (40 bytes) + an OPC/ZIP archive. We inspected two kinds:

**Symbol package** (downloaded from the symbol feed, e.g. Base Application):
`NavxManifest.xml`, a large `SymbolReference.json` (the public API surface),
`MovedObjectsManifest.json`, `[Content_Types].xml`. **No source, no compiled code.**

**`alc`-built source package** (we compiled a minimal project, target Cloud):
`NavxManifest.xml`, **`src/src/Hello.al` - the AL source in plaintext**,
`SymbolReference.json`, `DocComments.xml`, `entitlement/<id>.xml`,
`MediaIdListing.xml`, `[Content_Types].xml`. **No IL, no C#, no .NET assembly.**

**Key takeaway:** `alc` does **not** do machine-code/IL codegen into the `.app`.
A source package is *source + symbol table + metadata XML*. The **BC server**
compiles to runtime artifacts at publish/install time. The "undocumented
bytecode codegen" blocker feared at planning time does **not** apply to producing
a source `.app`.

## IP protection (resourceExposurePolicy vs runtime packages)

Setting `resourceExposurePolicy.allowDownloadingSource: false` in `app.json`
records `<ResourceExposurePolicy AllowDownloadingSource="false" .../>` in the
manifest **but leaves the AL source physically in the `.app`**. It is a flag the
**server enforces** (refuses to serve source / debugging), not a source-stripper.

True physical source removal only happens in the **runtime package (NEA format)**,
which is produced **server-side** by `Get-NavAppRuntimePackage`, is OnPrem-only
to generate, and is version-locked. The probe found
`ReadyToRunPackageOutputter.CreatePackage(...)` in the CodeAnalysis DLL -
**"ReadyToRun" = .NET native AOT**, matching the "compiled to machine code /
can't decompile" runtime-package form. Producing it needs server-compiled
artifacts, so it stays a server operation.

## Can `CodeAnalysis` emit a package in-process? - YES

Reflecting over `Microsoft.Dynamics.Nav.CodeAnalysis.dll` (the DLL the bridge
**already hosts in-process**) found the entire compile -> emit -> package pipeline,
with public entrypoints:

- **`Compilation.Emit(EmitOptions, ModuleOutputter, CancellationToken) -> EmitResult`** - **public** (Roslyn-style in-process emit).
- **`Microsoft.Dynamics.Nav.CodeAnalysis.Emit.ModuleOutputter`** - **public**, with public methods that assemble exactly the `.app` entries we observed: `AddApplicationObject`, `AddEntitlementObject`, `AddXmlDocumentationComments`, `AddMovedObjects`, `AddNavigationObject`, `AddProfileObject`, `AddReportLayout`, `FinalizeModule`, `GetDiagnostics`.
- A full `...CodeAnalysis.Emit` namespace (`ApplicationObjectEmitter`, `ObjectCodeEmitter`, public `EmitResult`, `ModuleBuilder`) - i.e. the **codegen** path, not just analysis.
- Package writers: `NavAppPackageWriter`, `NavAppManifest`, `ReadyToRunPackageOutputter.CreatePackage(...)`.
- `alc.dll` is a thin CLI over the same library: its driver is `Microsoft.Dynamics.Nav.CodeAnalysis.CommandLine.Alc.Run(...)` (internal) behind `Program.Main(string[])` (public).

The bridge already builds in-process `Compilation` objects via reflection
(`Compilation.Create`, `GetSemanticModel`, `GetDiagnostics`) and constructs
internal types reflectively. So driving `Compilation.Emit` into a
`ModuleOutputter` - or invoking `Alc.Run` in-process - is reachable with the
machinery the bridge already uses. **`HandleCompile` shells out to `alc` for
expedience, not necessity.**

## Conclusion

- **True in-process emit (no `alc` subprocess) is FEASIBLE** by reusing the
  hosted `CodeAnalysis` engine - either `Compilation.Emit` + `ModuleOutputter`,
  or invoking the `Alc.Run` driver in-process. **No Rust codegen reimplementation
  is required**, and no undocumented bytecode is involved for a source `.app`.
- A **pure-Rust** reimplementation is still large (compiler-grade semantic
  analysis + byte-compatible `SymbolReference.json`) but the moat is smaller than
  feared (no IL codegen for source packages). Given the architecture constraint
  that the C# bridge can't be dropped, reusing `CodeAnalysis` in-process strictly
  dominates a from-scratch rewrite.
- **IP protection**: the cloud-sanctioned lever is the `resourceExposurePolicy` /
  `ShowMyCode` manifest flags (server-enforced; source stays in the package). The
  hard, source-stripped form is the server-generated runtime package (NEA /
  ReadyToRun) - wrap `Get-NavAppRuntimePackage`, don't reimplement.

## Recommendation / next steps

1. **Highest-value, low-risk**: add an in-process emit method to the bridge
   (`Compilation.Emit` -> `ModuleOutputter.FinalizeModule`) so the daemon's
   `compile` produces the `.app` with **no `alc` subprocess** - the genuine
   "native compiler" the initiative wants. The native-first wiring (Parts 1-2)
   already routes deploy/publish/debug through the bridge, so this drops in
   behind the existing seam.
2. Validate the bridge-emitted `.app` byte-for-byte against an `alc`-built one
   (manifest, `SymbolReference.json`, entry set) and that it publishes to a BC
   sandbox.
3. **IP protection (Part 4)**: surface `resourceExposurePolicy` in scaffolding;
   wrap `Get-NavAppRuntimePackage` for OnPrem runtime packages; validate
   runtime-package-as-cloud-PTE empirically.

## Delivered by this spike

- **`crates/al-core/src/symbols/app_inspect.rs`** - native `.app` unpacker /
  inspector: `list_app_entries`, `list_app_file`, `extract_app` (traversal-safe),
  with content classification (`AlSource` / `Json` / `Xml` / `DotNetAssembly` /
  `Other`) and `has_source()` / `has_compiled_code()` helpers. This is the
  IP-inspection tool referenced by Part 4. Unit-tested.
- Reusable reflection probe approach for the CodeAnalysis emit surface
  (run ad-hoc against the toolchain DLL; not committed to the production bridge).

## Update: pure-Rust emit path (no Microsoft dependency) - validated

Direction chosen: emit the `.app` in **pure Rust with no Microsoft runtime
dependency**, reverse-engineering the format rather than reusing CodeAnalysis.
This is research and foundation work for the production compiler path, not the
default compile path yet.

The container + source + most metadata XML are trivial (inverse of the unpacker).
The one hard file is `SymbolReference.json`, and within it the one genuinely
opaque value is each method's generated `Id`. **That hash is now fully reverse-
engineered and reproduced exactly.** Decompiled `Microsoft.Dynamics.Nav.CodeAnalysis`
(via `ilspycmd`) gives the algorithm (`MethodSymbol.CalculateMethodIdForNewVersions`,
runtime >= Spring2021):

```text
hash = FNV1(Name.ToUpperInvariant())                       // FNV-1 over UTF-16LE bytes
hash = Hash.Combine(hash, ReturnType.NavTypeKind)          // Combine(h1,h2)=((h1<<5)+h1+(h1>>27))^h2
for (i, param) in params:
    hash = Hash.Combine(hash, i, param.IsVar?1:0, param.NavTypeKind)
hash = AdjustIdForSystemCodeunits(hash)                    // |h| % 1_250_000_000 for obj id >= 2e9
```

`NavTypeKind.GetHashCode()` is the enum's int value (`None`/void = 0, `Text` =
16646153, `Integer` = 12451845, ...). Verified against alc 17.0.34 across 12+ test
vectors (void/return/param/var/multi-param). Implemented + tested in
**`crates/al-core/src/emit/`** (`method_id.rs`, `nav_type_kind.rs`).

### Remaining for a full pure-Rust `.app` emitter

- **Easy** (days): NAVX+ZIP packager (inverse of `app_inspect`); `NavxManifest.xml`
  from `app.json`; static `[Content_Types].xml`, `MediaIdListing.xml`,
  `entitlement/*.xml`, `DocComments.xml`.
- **The real work** (weeks for full fidelity): the `SymbolReference.json` emitter -
  walk parsed AL objects (we have tree-sitter + the symbol model) and serialise
  every object type + members to match alc byte-for-byte. Open sub-items: the
  overload-disambiguation subtype hash for Record/Codeunit/Enum params; trigger-id
  folding; the full AL-type to `NavTypeKind`+subtype map. All tractable by
  differential testing against the local `alc` oracle.
- **Critical validation gate** (needs a live BC tenant): does a natively-emitted
  `.app` *publish*? If the server recompiles from source and regenerates symbols,
  a "good enough" emitter suffices; if it strictly validates `SymbolReference.json`,
  byte-perfection is required. Answer this cheaply before investing weeks in full
  fidelity.
