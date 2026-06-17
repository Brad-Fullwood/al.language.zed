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

## Update (2026-06-16): native emitter complete for the core object types

The pure-Rust emitter now reproduces alc's **entire `.app`** for table / codeunit /
enum / interface — every file IDENTICAL to alc's output, verified by diffing a
`pack-native` build against `alc` on the same project:

| File | Status |
|------|--------|
| `SymbolReference.json` | identical (fields, keys, methods, params, attributes, properties, implemented interfaces) |
| method `Id`s | identical incl. Record/Enum/Codeunit/Interface param overload-disambiguation + `IgnoreReturnValue` flag masking |
| interface object `Id` | identical (FNV(quoted name) + AppId-GUID `ToByteArray` + `-1`×method-count) |
| `NavxManifest.xml` | identical core attrs |
| `entitlement/*.xml` | identical (`CreateDefaultEntitlements`: TableData→RIMD 15, else Execute 16) |
| `MediaIdListing.xml`, `DocComments.xml`, source, `[Content_Types].xml` | identical |
| `TextData/*.xliff` | trans-unit ids + sources identical (`{Kind} {(long)FNV(name)+int.MaxValue}` path) |

Shipped:
- `crates/al-core/src/emit/` — `symbol_extract` (tree-sitter → objects), `symbol_reference`
  (alc-shaped JSON), `manifest`, `package` (NAVX/ZIP), `assemble` (entitlement + media +
  xliff + content-types), `project` (`build_app_from_project`), `method_id`.
- CLI: **`al-explorer pack-native [--project DIR] [--out FILE]`** — builds a deployable
  `.app` with no Microsoft `alc`.
- Data-driven per the project rule: type classification via `language_data`; NavTypeKind
  values via generated `tree-sitter-al/data/nav_type_kinds.json` (tool at
  `generator/tools/nav-type-kinds`).
- Differential regression test `emit::symbol_reference_test` (vs committed real-alc output).

Remaining (same machinery, longer tail): page/report/xmlport/query/extension object
types; XLIFF full inclusion rules (locked captions, all property kinds); and the live-BC
publish validation (needs a tenant).

## Update (2026-06-17): full `.app` byte-identical to alc (every file)

`pack-native` now reproduces alc's **entire `.app` byte-for-byte** — every entry, in
alc's entry order — with the **sole** exception of the `NavxManifest.xml` `<Build>` line
(`Timestamp` + `CompilerVersion`), which is inherently producer/clock-specific and cannot
match a given alc invocation. Validated by a full file-by-file diff of `pack-native` vs
`alc` on a 21-object fixture.

Object types — `SymbolReference.json` byte-identical for **all** of: table, codeunit, enum,
interface, tableextension, enumextension, query, permissionset, permissionsetextension,
page, report, xmlport, profile, **pageextension, reportextension, profileextension,
pagecustomization**. Locked in the differential regression fixture (now a byte-exact
string assertion, not just semantic) at `crates/al-core/src/emit/testdata/`.

Newly reproduced this round:
- **pageextension** — `ControlChanges` (`Anchor`/`ChangeKind`/`Controls`), added-field types
  resolved through the base page's `SourceTable`.
- **reportextension** — `Target`, `Variables`, `RequestPage` (request-page `ControlChanges`),
  dataset `add` `Columns` (`OwningDataItemName` + base-report dataitem-table resolution),
  `DataItems`/`Labels`/`Layouts`.
- **profileextension** — generic `TargetObject`/`Properties` path (no id).
- **pagecustomization** — `customizes` target, `Id: 0`, member ids scoped to object id 0,
  and the runtime-gated `Editable=False` injection on added fields (`< 16.0`).
- **`ProfileSymbolReferences/<MetadataName>.json`** — per-profile/-profileextension files.
- **`navigation.xml`** — page-extension `NavigationChanges/ActionChange` delta entries
  (`TargetID` = base page id, `TargetType` = base kind). The `UsageCategory` *new-entry*
  path (Page/Report/Query) is structured but not yet emitted (no fixture exercises it).
- **`TextData/*.xliff`** — full `TextDataVisitor` inclusion rules (RE'd from the decompiled
  `TextDataVisitor`, runtime Fall2024): only Table/TableExtension object+field captions and
  Page/PageExtension object captions at runtime 14 (Report needs ≥15; enum/query/permset/
  profile never); extension members fold the id-root onto the base object and emit
  `al-object-target`; `original="TextDataApp"` literal; ordinal-ignore-case ordering.
- **`NavxManifest.xml`** — exact alc layout (no XML prolog, 2-space indented, full `<App>`
  attribute set), `[Content_Types].xml` derived from the extensions actually present,
  `DocComments.xml` exact whitespace, UTF-8 BOM on the JSON symbol files.

Byte-exact JSON required `serde_json`'s `preserve_order` feature (workspace-wide; full
suite green) plus matching alc's group order, always-emitted-empty core groups
(`Codeunits`/`Reports`/`XmlPorts`/`Queries`/`ControlAddIns`/`EnumTypes`/`DotNetPackages`/
`Interfaces`/`PermissionSets`/`PermissionSetExtensions`/`ReportExtensions`), and nested
field order (`IsVar` before `Name`; enum-value `Ordinal` before `Properties`).

**ControlAddIn (now complete):** the previously-open auto-generated `PublicKeyToken` is
**cracked** — it is the first 8 bytes of `SHA256(UTF8(app name))` in lowercase hex
(`SourceControlAddInTypeSymbol.CalculatePublicKeyToken`), keyed on the *app/module name* so
every add-in in an app shares one token. The `ControlAddIns` symbol entry and the add-in
resource bundle (`addin/<MetadataName>.zip` with `manifest.xml` + `[Content_Types].xml`,
and `addin/controladdins.dock`) are reproduced content-byte-identical to alc, including
alc's exact `ControlAddInManifest.ToString()` element order (`Resources`, `ScriptUrls`,
`StyleSheetUrls`, six dimension props when set, the four stretch/shrink booleans when true
in order VerticalShrink/VerticalStretch/HorizontalShrink/HorizontalStretch, then `Version`).
Embedded local resources / inline scripts (vs external URLs) are not yet modelled.

**N/A (OnPrem/first-party only, confirmed):** the `entitlement` *object* and `dotnet`
objects do not serialize into a Cloud-target `.app` (alc emits nothing for them; `dotnet`
is rejected with AL0296 on Cloud). The default implicit `entitlement/<appid>.xml` is
already reproduced.

**Navigation new-entry path (now done):** the `ActionContainers/Departments` path for
Page/Report/Query with a non-`None` `UsageCategory` is reproduced — `ActionDefinition`
run-object actions with `RunObjectType`/`TargetID`/`DepartmentCategory`/`RunObjectSrcTable`
(resolved source/related table id)/`Name`/`CaptionML`/`ApplicationArea`/`AdditionalSearchTermsML`
and the `Caption`/`AdditionalSearchTerms` translation keys (same FNV path hash as XLIFF).
Verified byte-identical to alc with the two `ControlGUID`s masked — those are
`Guid.NewGuid()` in alc, so this part is functionally faithful but inherently
non-byte-stable (the same non-determinism as the NAVX package GUID).

**Control add-in embedded local resources (now done):** local `Scripts`/`StyleSheets`/
`Images` (relative refs, vs external http URLs) are listed under `<Resources>` and bundled
into the add-in zip at their paths; inline `StartupScript`/`RefreshScript`/`RecreateScript`
files are embedded as `<![CDATA[…]]>`; the inner `[Content_Types].xml` covers every bundled
extension. Verified byte-identical to alc (manifest, bundled files, content-types, docket).
This also fixed multi-value list-property extraction (`child_by_field_name` returned only
the first `value` node; now the full `value`-field span is taken — `Scripts = 'a', 'b';`).

**Report rendering layouts (now done):** `rendering { layout(Name) { Type = …; LayoutFile
= '…'; } }` produces a `Reports[]`/`ReportExtensions[].Layouts` entry (`{Properties, Name}`)
and bundles the referenced file verbatim at `layout/<LayoutFile path>`. Also fixed: alc
always emits a report's `RequestPage` (the default `{Id:0,Name:"RequestOptionsPage"}` when
no `requestpage` is declared). Verified byte-identical to alc (/tmp/rltest) and locked into
the fixture.

**`IncludedPermissionSets` (now done):** emitted as a property whose value keeps each
referenced permission-set name quoted only when not a bare identifier (`"PS A",PSC`), unlike
resolved object-reference properties (`RoleCenter`/`SourceTable`).

**Cross-app symbol resolution (object ids — now done):** `build_symbol_reference` takes an
`external: &Resolver` (object name → `ObjectRef{id, module_id}`) built by
`load_external_resolver`, which indexes every `.alpackages/*.app` `SymbolReference.json` via
`symbols::app_reader::read_app_file`. Project objects shadow referenced ones. Resolved:
- subtype references in referenced apps (`Record Customer` → `Subtype{ModuleId, Name, Id}`,
  with the id folded into the method-signature hash — byte-identical to alc, /tmp/xapp);
- extension targets in referenced apps (`tableextension … extends Customer` →
  `TargetObject:"#<moduleid-no-dashes>#Customer"`).
Self-contained fixtures keep `external` empty, so they stay byte-identical.

**Cross-app extension-added field types (now done):** `ExternalSymbols` also carries
referenced `(table,field)→type` and `page→SourceTable-name` maps (the loader resolves the
base page's SourceTable *id* back to a table name). A field added to a base page (e.g.
`pageextension … extends "Customer Card"` adding `Rec."Balance (LCY)"`) now resolves to the
base table field type (`Decimal`) — byte-identical to alc (/tmp/xapp).

**Remaining cross-app sub-case — system permission ids:** `system "Tools, Object Designer"`
(alc id 5210) is a platform built-in NOT in `.alpackages`. The id constants are in the
decompiled `SystemObjects` class (`ToolsObjectDesigner = 5210`, …); the AL display names
("Tools, Object Designer") live in the embedded `SystemObjectsResources.resources` of
`Microsoft.Dynamics.Nav.CodeAnalysis.dll`, keyed `{Name}SystemObjectCaption`. To resolve
system-permission names → ids: extract the `SystemObjects` Name→id constants + the resource
captions, join by the `{Name}` prefix into a generated `display-name → id` data table (stay
data-driven), and look it up when emitting `system`-type permissions.

**Net result:** `pack-native` reproduces alc's entire `.app` — every entry, content
byte-identical — for the fixtures, spanning all object types and metadata artifacts.
Unavoidable exceptions are limited to non-deterministic / producer-specific bytes: the
`NavxManifest.xml` `<Build>` line (build clock/producer), raw zip *framing*
(compression/local-header bytes; entry *contents* match), and navigation new-entry
`ControlGUID`s (alc's own `Guid.NewGuid()`). **The only remaining item is live-BC publish
validation — it needs a real BC tenant and cannot be done locally.**
