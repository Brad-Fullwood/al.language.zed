# Symbol & Package Engine

**Module:** `crates/al-core/src/symbols/` · **Status:** ✅ shipped

The symbol engine is why this project can answer advanced AL questions (search, completion, object
lookup, event discovery, impact) as ordinary in-memory queries instead of repeatedly invoking the
compiler or crawling packages. It reads Business Central `.app` packages natively, parses their
`SymbolReference.json` and manifest, caches the parsed result on disk, and indexes everything into
concurrent in-memory maps.

## Overview

| Capability | File | Summary |
| --- | --- | --- |
| Symbol index | `index.rs` | Concurrent (DashMap) multi-package index with secondary indexes |
| Symbol model | `model.rs` | `ObjectKind` (20+ kinds) + `SymbolEntry`; `SymbolReference.json` parsing |
| Disk cache | `cache.rs` | mtime/size/schema-validated binary cache of parsed packages |
| `.app` reader | `app_reader.rs` | NAVX header + ZIP → manifest + `SymbolReference.json` → entries |
| `.app` inspector | `app_inspect.rs` | enumerate/classify archive entries (source vs compiled) |
| Manifest | `manifest.rs` | parse `NavxManifest.xml` (Id/Name/Publisher/Version) |
| Composition | `composition.rs` | merge base object + extensions into a composed view |
| Events | `events.rs` | discover publishers/subscribers across the index |
| Source index | `source_index.rs` | map (kind,id,name) → embedded `.al` path for navigation |
| Virtual files | `virtual_file.rs` | extract embedded source or render an outline for go-to-def |
| NuGet | `nuget.rs` | resolve AL deps → NuGet package IDs; download `.nupkg`; extract `.app` |
| BC server | `bc_server.rs` | download `.app` from the BC dev endpoint |
| OAuth | `oauth.rs` | Microsoft Entra auth-code (PKCE) + device-code flows |
| Language data | `language_data.rs` | object types + runtime enums from generated JSON |

## How it works

### The index (`index.rs`)

`SymbolIndex` stores shared `Arc<SymbolEntry>` and maintains parallel DashMap secondary indexes:
by lowercase name, by kind+id, by kind, by extension target (`extends`), and a composed-object cache.
Common queries become direct map lookups. Packages load in parallel (rayon `par_iter`). A
pre-computed 30-entry default-completions slice answers the "blank completion at top level" path in
O(1) (ISSUE-162). On package removal, all secondary indexes are pruned in lockstep (the "T049"
discipline) so no dangling `Arc` references remain. Synthetic pseudo-enums (generated for
Option-typed fields, id = -1) are excluded from id-based lookups and search.

### Reading `.app` (`app_reader.rs`, `manifest.rs`, `app_inspect.rs`)

A `.app` is a NAVX header followed by a ZIP. The reader validates the `NAVX` magic, locates the ZIP
(fast path at the standard offset, with a linear-scan fallback), and decompresses `NavxManifest.xml`
and `SymbolReference.json` (stripping UTF-8 BOMs, which BC emits inconsistently, and tolerating
trailing NUL/EOF padding). Nested namespaces are flattened into a flat `Vec<SymbolEntry>`.
`app_inspect` separately enumerates every archive entry and classifies it (AL source / JSON / XML /
.NET assembly / other) by name and magic bytes, so you can tell a source-bearing package from a
symbol-only one. All of this is bounded against decompression bombs: ≤200 MB file/JSON/manifest,
≤200,000 archive entries, ≤1 GiB total decompressed.

### Caching (`cache.rs`)

Parsed packages are cached under the user cache dir as `[len][header JSON][objects JSON]`, keyed by
filename + FNV-1a path hash. The cache is validated against the `.app`'s mtime (sub-second precision)
and size, plus a `CACHE_SCHEMA_VERSION`; any mismatch silently re-parses. Writes are atomic (temp +
rename), the cache directory is locked to 0700, and orphaned temp files from crashed writers are
cleaned up. Warm starts therefore skip re-reading and re-parsing large `SymbolReference.json`
payloads (the Base Application alone is ~6 MB).

### Composition (`composition.rs`)

`get_composed(kind, name)` returns a `ComposedObject` merging a base object with every applicable
extension — fields (sorted by id, deduped), methods, controls, and enum values (sorted by ordinal).
It is cycle-safe by construction (it only walks base → extensions, never extension → extension,
F-OPEN-038) and cached with per-name invalidation. Composing 15 extensions runs in <5 ms.

### Source navigation (`source_index.rs`, `virtual_file.rs`)

For go-to-definition into a dependency, `source_index` memory-maps the `.app` and scans `.al` headers
to map (kind, id) / (kind, name) → internal ZIP path (with double-checked per-path locking and
mtime-based staleness). `virtual_file` then either extracts the embedded `.al` source or, when the
package ships no source, **renders an outline** (fields, methods, keys, enum values, properties) as a
read-only virtual file and locates the member's range so the editor can jump to it. Cache files are
regenerated when either the `.app` or the `al-lsp` binary is newer (F-041).

### Symbol acquisition (`nuget.rs`, `bc_server.rs`, `oauth.rs`)

Two download backends:

- **NuGet (`nuget.rs`):** resolves `app.json` dependencies to NuGet v3 package IDs — including the
  empirically-discovered, inconsistent Microsoft IDs (e.g. `Microsoft.Application.symbols`,
  `Microsoft.BaseApplication.symbols.{GUID}`) and country-specific variants — caches the service
  index, downloads `.nupkg`s concurrently behind a bounded semaphore, dedupes concurrent requests for
  the same package via a per-package lock (F-OPEN-019), and extracts the inner `.app`.
- **BC server (`bc_server.rs`):** GETs `/dev/packages?publisher=…&appName=…&versionText=…` with auth,
  size-capped at 200 MB, streamed to a temp file then renamed. Caches the access token in an `RwLock`
  and clears it on 401/403 to recover from stale tokens (F-OPEN-013).
- **OAuth (`oauth.rs`):** Microsoft Entra authorization-code flow with **PKCE** (browser → localhost
  redirect) and a **device-code** fallback for headless environments, with disk-cached refresh
  tokens, tenant/GUID validation, env-var overrides (`BC_CLIENT_ID`/`BC_ACCESS_TOKEN`/`BC_TENANT`),
  and **zeroized** token memory on drop (F-OPEN-010).

Freshly downloaded symbols are loaded into the workspace **without a daemon restart**.

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| `.app` reading | native NAVX/ZIP reader, in-process | compiler/extension internal |
| Symbol model | in-memory multi-dimensional index | flat symbol lists downloaded by the extension |
| Caching | disk cache validated by mtime/size/schema; warm starts skip re-parse | server-side compile of system apps |
| Composition | explicit composed-object view (base + extensions) | resolved internally by the compiler |
| Download | native NuGet **and** BC-server backends, concurrent, deduped, no restart | extension's download-symbols command |
| Auth | native Entra PKCE + device code, token zeroization | extension/VS Code auth |
| Source-only packages | renders a public-API outline for navigation | navigates compiler symbols |

## Why this approach

Microsoft's symbol download gives you packages; this engine turns them into a queryable model once
and reuses it everywhere. The disk cache makes warm starts cheap, composition makes table/page
extension workflows correct, and the dual NuGet/BC-server download with no-restart loading means
dependency changes don't interrupt your session. Crucially, the same index powers completions, hover,
definitions, event discovery, and impact analysis — so all of those are fast for the same reason.

## How to use

- **CLI:** `al-explorer search <q> [--limit N]`, `object <type> <name>`, `by-id <type> <id>`,
  `composed [<kind>] <name>`, `packages`, `deps`, `events <name>`, `subscribers <event>`.
- **Download:** `al-explorer download-symbols --source server|nuget` (Zed tasks: *AL: Download
  Symbols (Server/NuGet)*); `al-explorer authenticate`; `al-explorer clear-cache`.
- **LSP execute commands:** `al.downloadSymbols`, `al.downloadSymbolsServer`,
  `al.downloadSymbolsNuget`, `al.clearSymbolCache`.
- **MCP:** `al_downloadsymbols`, `al_symbolsearch`.

## Symbol sources & `appLocalFolderPaths`

The index loads `.app` packages from three places:

- the project's `.alpackages/` (the default, scanned automatically);
- packages downloaded on demand via the NuGet / BC-server backends above;
- any **arbitrary folder** passed to `SymbolIndex::load_packages` — the loader reads a `.app` from any
  path, indexes its objects, and records the originating path (`app_path`) for navigation.

The `al.appLocalFolderPaths` setting — the way the Microsoft AL extension points at extra local `.app`
folders — is **parsed into `AlConfig`** (`al-project` `config.rs`) **but is not yet wired into symbol
loading**: nothing reads that field to feed those folders into the index, so setting it today has no
effect on which packages are resolved. The underlying capability it would drive (loading a `.app` from
a non-`.alpackages` folder) already works and is regression-tested
(`al-symbols` `index.rs::load_packages_resolves_app_from_an_arbitrary_local_folder`); only the
config→loader plumbing is outstanding. Until it lands, place extra packages in `.alpackages/`.

## Limitations & roadmap

- `.app` symbols expose the **public API declaration, not call-site bodies**. A package entry carries
  an object's signatures, fields, keys, enum values and properties, but procedure *bodies* are
  compiled away — they are never shipped in a symbol package. Consequences:
  - When a package has no embedded source, navigation opens a **reconstructed outline** (see
    `virtual_file::render_outline`): valid AL with full signatures but no `begin…end` bodies. That
    virtual file is now prefixed with an explicit header (`virtual_file::OUTLINE_NOTE`) stating it is
    the public API only, with bodies unavailable, so the reader is never misled into thinking an empty
    body means an empty method.
  - The `source` query (`al-explorer source` / daemon `source`) returns the same outline with a
    structured `note` — *"Rendered from symbol metadata … no implementation bodies"*.
  - Cross-package "who calls this" cannot be recovered from package symbols alone; workspace source
    fills this in (affected-test selection treats `.app`-only declarations as having no call sites —
    see gap B7).
- 🟡 BC-server downloads don't yet use the NuGet path's explicit concurrency-limit/dedupe controls.
- `ROADMAP.md` (Symbol And Package Engine): add benchmark-grade cold/warm load data, make the symbol
  perf audit CI-deterministic with fixtures, add byte-level memory accounting, distinguish embedded
  source vs generated outline vs metadata-only in user output, and wire `appLocalFolderPaths` into the
  loader (it is parsed today; see above).
