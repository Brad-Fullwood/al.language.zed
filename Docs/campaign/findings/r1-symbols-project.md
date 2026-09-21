# R1 review: al-symbols, al-semantic, al-types, al-source, al-project, al-workspace

Adversarial read-only review, 2026-09-21. Baseline: AUDIT-BACKLOG.md section
"Symbols & Project" (2026-07-31). Items from that audit that are still present in
current code are tagged [STILL-OPEN].

## Coverage

- [x] AUDIT-BACKLOG.md "Symbols & Project" re-verification
- [x] al-symbols/src/index.rs
- [x] al-symbols/src/app_reader.rs
- [x] al-symbols/src/app_inspect.rs
- [x] al-symbols/src/manifest.rs
- [x] al-symbols/src/cache.rs
- [x] al-symbols/src/virtual_file.rs
- [x] al-symbols/src/source_index.rs
- [x] al-symbols/src/model.rs
- [x] al-symbols/src/events.rs
- [x] al-symbols/src/composition.rs
- [ ] al-symbols/src/nuget.rs
- [ ] al-symbols/src/bc_server.rs
- [ ] al-symbols/src/oauth.rs
- [x] al-symbols/src/source_availability.rs + language_data.rs + lib.rs
- [ ] al-semantic/src/bridge.rs
- [ ] al-semantic/src/host.rs + cache.rs + lifecycle.rs
- [ ] al-types (jsonc.rs, filename.rs, rest)
- [ ] al-source/src/file_index.rs
- [ ] al-source/src/documents.rs + parsing.rs
- [ ] al-project/src/project.rs
- [ ] al-project/src/config.rs
- [ ] al-project/src/toolchain.rs
- [ ] al-project/src/analyzers.rs + errors.rs
- [ ] al-workspace/src/lib.rs
- [ ] al-workspace/src/semantic_lifecycle.rs
- [ ] al-workspace/src/test_results.rs + doctor.rs

## Findings

### [SECURITY] ZIP-offset probing bounds the attempt count but not the cost per attempt
- where: crates/al-symbols/src/app_reader.rs:157-196 (`find_zip_offset`)
- severity: medium
- scenario: `is_valid_zip` calls `ZipArchive::new`, which in zip 2.4.2 eagerly parses the
  whole central directory. The fix for the old audit item caps the number of *failed*
  attempts at 64 but each attempt still costs a full central-directory parse. A crafted
  200 MB `.app` that starts with `NAVX`, plants 64 `PK\x03\x04` signatures in the first
  1 MiB, and ends with an EOCD pointing at a ~3 million entry central directory whose last
  record is malformed makes the reader parse and allocate that directory 64 times before
  returning `NoZipSignature`. Reached from `app_inspect::list_app_file` (al-explorer
  package inspection) and `app_reader::read_app_bytes`.
- fix: validate a candidate offset cheaply before paying for `ZipArchive::new` — read the
  EOCD once for the whole buffer and check that `offset + cd_offset + cd_size` lands inside
  the data and that the bytes there are `PK\x01\x02`. Alternatively move the
  `archive.len() > MAX_ARCHIVE_ENTRIES` check inside the probe by using
  `ZipArchive::with_config`/a bounded reader so a probe cannot parse an unbounded directory.
- status: open

### [PERF] Every entry lookup inside a `.app` walks the archive with `by_index`
- where: crates/al-symbols/src/app_reader.rs:296-328 (`find_file_in_archive`), called twice
  per package from `read_archive` (:113-118)
- severity: medium
- scenario: `find_file_in_archive` loops `archive.by_index(i)` for each of up to
  `MAX_ARCHIVE_ENTRIES` (200 000) entries. `by_index` seeks to and parses the local file
  header of every entry, so locating `NavxManifest.xml` and then `SymbolReference.json`
  costs two full passes of local-header reads over the archive. A cloud-targeted Base
  Application `.app` carries one archive entry per source file (tens of thousands), so a
  cold `load_packages_cached` pays two seek-heavy scans per package on top of the JSON parse.
- fix: iterate `archive.file_names()` (index-only, no local-header read) to resolve the
  entry name, and resolve both wanted names in a single pass.
- status: open

### [SIMPLIFY] `read_app_file` and `read_app_bytes` locate the ZIP payload by different rules
- where: crates/al-symbols/src/app_reader.rs:57-91
- severity: low
- scenario: `read_app_bytes` requires a `PK\x03\x04` local header within the first 1 MiB
  (`find_zip_offset`), while `read_app_file` hands the whole file to `ZipArchive::new` and
  relies on the zip crate inferring the prefix from the EOCD. The same bytes therefore take
  two different code paths with different accept/reject behaviour and different errors (a
  NAVX file wrapping an entry-less ZIP is `NoZipSignature` through one path and `NoManifest`
  through the other), and the probing hardening in `find_zip_offset` does not apply to the
  file path at all.
- fix: pick one. Since the zip crate already handles prefixed archives, drop
  `find_zip_offset` from `read_app_bytes` too, or make `read_app_file` memory-map/read and
  go through the same probe. Keeping both means a package that loads from disk may fail
  when the same bytes arrive over the wire.
- status: open

### [BUG] `render_outline` quotes object/field/enum names but not key, method, parameter or variable names
- where: crates/al-symbols/src/virtual_file.rs:546-575 (method + parameters), :605-612 (keys),
  :622-629 (variables). `format_name` is defined at :513 and applied only at :535, :578, :590, :616.
- severity: medium
- scenario: the audit's `format_name` fix covers only some of the identifiers this function
  emits. `SymbolReference.json` stores key field names unquoted, and BC's primary keys use
  names like `No.`, `Entry No.`, `Line No.`. Navigating to `table 18 Customer` in a package
  with no embedded source renders `key(Key1; No.)`, which is not valid AL — the doc comment
  right above the function (":506-509") and `Docs/features/symbol-and-package-engine.md:177`
  both claim valid AL. The same holds for a procedure named `"Post Document"`, a parameter
  named `"Line No."`, and a global variable named `"Sales Header"` (all legal quoted AL
  identifiers that appear in real packages), each rendered bare.
- fix: route `k.name`, every entry of `k.field_names`, `m.name`, `p.name` and `v.name`
  through the existing `format_name`.
- status: open

### [GAP] The process-global `.app` source-index cache is never evicted in production
- where: crates/al-symbols/src/source_index.rs:30-31, :337-397
- severity: medium
- scenario: `SOURCE_INDEX_CACHE` and `SOURCE_BUILD_LOCKS` are `OnceLock<DashMap<PathBuf, …>>`
  statics keyed by canonical `.app` path. `clear_source_index_cache` is the only removal
  path and grep shows it is called from tests only. `SymbolIndex::replace_packages_cached`
  and `remove_package_identities` drop symbols for a package but leave its `AppSourceIndex`
  (which holds two `HashMap`s plus one `String` per embedded `.al` — tens of thousands of
  entries for a source-bearing Base Application) alive for the life of the daemon. A long
  session that re-points `al.packageCachePath`, or that downloads successive symbol versions
  (each a new filename), accumulates one full index per path ever seen.
- fix: call `clear_source_index_cache`, or a per-path removal, from
  `SymbolIndex::clear_loaded_packages` / `remove_package_identities`. The memory report in
  `SymbolIndex::memory_stats` also does not count these allocations, so the growth is
  invisible to the workspace diagnostics endpoint.
- status: open

### [GAP] Only the first object in a multi-object embedded `.al` is reachable by navigation
- where: crates/al-symbols/src/source_index.rs:409-488 (`parse_object_header_inner` returns
  on the first declaration), consumed at :170-175
- severity: low
- scenario: AL allows several objects in one file, and packages built from such files ship
  them as one archive entry. `by_kind_id`/`by_kind_name` get only the first declaration, so
  `source_path_for_entry` misses every later object in that entry and
  `virtual_file::get_or_create` silently falls back to the generated outline. The user sees
  "Reconstructed public API from SymbolReference.json" for an object whose real source is in
  the package. This mirrors the still-open `al-source/src/file_index.rs` single-declaration
  gap, so the two should be fixed with one shared multi-declaration scanner.
- fix: keep scanning after the first header and record every `(kind, id, name)` found in the
  entry.
- status: open

### [BUG] A corrupt `.zed/settings.json` in the symbol cache fails every package navigation
- where: crates/al-symbols/src/virtual_file.rs:51 and :656-673
- severity: low
- scenario: `get_or_create_with_availability` calls `ensure_readonly_settings` first and
  propagates its error. If `~/.cache/al-lsp/symbols/.zed/settings.json` is not valid JSON, is
  not an object, or has a non-array `read_only_files` (a hand edit, or a partial file left by
  a killed process on a filesystem where the rename was not durable), every go-to-definition
  into a package returns `InvalidData` and no source is materialized at all.
- fix: this file only makes the editor mark the generated files read only. Log and continue
  on a malformed settings file (or overwrite it) rather than failing the navigation that
  triggered it.
- status: open

### [BUG] Background virtual-file GC can delete a cache entry between the existence check and the read
- where: crates/al-symbols/src/virtual_file.rs:65 (`if !file_path.is_file()`), :110-111,
  :185-259
- severity: low
- scenario: `gc_cache_once` spawns a detached sweep thread that deletes any `.al` under the
  cache root older than 30 days. A request that finds an existing but 30-day-old cache file
  takes the `is_file()` branch, skips generation, and then calls `enforce_readonly` and
  `materialized_availability`, both of which open the path. If the sweep removes the file in
  that window the request fails with `NotFound` instead of serving or regenerating the file.
  The comment at :113-115 claims the GC "can never race the create/write above", which is
  true for the write path and not for the reuse path.
- fix: treat a `NotFound` from `enforce_readonly`/`materialized_availability` as a cache miss
  and regenerate once, or have the sweep skip files whose mtime it cannot claim exclusively.
- status: open

### [PERF] Every cache save re-scans the whole symbol cache directory, from every rayon worker
- where: crates/al-symbols/src/cache.rs:256 (`self.cleanup_stale_tmp()` inside `save`),
  :166-192
- severity: low
- scenario: `load_package_batch_via_cache` runs `load_package_via_cache` on a rayon pool, and
  each cache miss calls `cache.save`, which calls `cleanup_stale_tmp`: a full `read_dir` of
  `~/.cache/al-lsp/index` plus a `metadata` call per `.tmp.` candidate. A cold start with 40
  packages in `.alpackages` therefore performs 40 concurrent directory scans of a cache
  directory allowed to hold up to 4 GiB of entries. The same sweep is already covered by
  `gc_once`, which the three cached entry points call before the batch.
- fix: drop `cleanup_stale_tmp` from `save` and fold the `.tmp.` sweep into `gc`, which is
  already once-per-process.
- status: open

### [PERF] Symbol cache GC runs synchronously on the workspace initialization path
- where: crates/al-symbols/src/cache.rs:310-380, called from
  crates/al-symbols/src/index.rs:618, :639, :664
- severity: low
- scenario: `gc_once` runs `gc` on the calling thread: a `read_dir` plus one `metadata` per
  `.cache` entry, then possibly a sort and a delete loop over a directory bounded at 4 GiB.
  That happens inside `load_packages_cached`, which is on the LSP's `initialize` path, so a
  large cache directory adds a stat storm before the first package is parsed. The comparable
  sweep in `virtual_file::gc_cache_once` deliberately spawns a background thread for exactly
  this reason.
- fix: spawn the sweep like `virtual_file::gc_cache_once` does, or run it after the batch
  completes rather than before it.
- status: open

### [SLOP] `is_workspace_entry` duplicates the workspace-package test from `source_availability`
- where: crates/al-symbols/src/composition.rs:65-68 vs
  crates/al-symbols/src/source_availability.rs:69-72
- severity: low
- scenario: both compare `entry.package` against the literals `"workspace"` and
  `"(workspace)"`. The composition copy carries a doc comment pointing at the other one
  ("see `source_availability::classify`"), which is the tell that it should be a call
  instead. A third spelling added in one place silently changes composed-base selection or
  availability reporting but not both.
- fix: export one `pub fn is_workspace_package(package: &str) -> bool` from
  `source_availability` and call it from `composition`.
- status: open

