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
- [x] al-symbols/src/nuget.rs
- [~] al-symbols/src/bc_server.rs (auth/download paths + panic scan only)
- [~] al-symbols/src/oauth.rs (panic + encoding scan only)
- [x] al-symbols/src/source_availability.rs + language_data.rs + lib.rs
- [~] al-semantic/src/bridge.rs (panic scan only)
- [x] al-semantic/src/host.rs + cache.rs + lifecycle.rs
- [x] al-types (jsonc.rs, filename.rs, rest)
- [x] al-source/src/file_index.rs
- [x] al-source/src/documents.rs + parsing.rs
- [x] al-project/src/project.rs
- [x] al-project/src/config.rs
- [x] al-project/src/toolchain.rs
- [x] al-project/src/analyzers.rs + errors.rs (analyzers read; errors.rs skimmed)
- [x] al-workspace/src/lib.rs
- [x] al-workspace/src/semantic_lifecycle.rs
- [x] al-workspace/src/test_results.rs + doctor.rs

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
- status: fixed 8095dcd7

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
- status: fixed 96d7c300, severity overstated. Measured on a 30 000-entry archive read from
  disk, the two `by_index` passes cost about the same as opening the archive, so the win is
  simplification rather than time. The single pass landed anyway.

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
- status: fixed c0e57362

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
- status: fixed ddaad70f. Per-path removal on package unload, plus a 64-entry LRU bound, plus
  the cached bytes in `SymbolIndexMemoryStats`.

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
- status: fixed 06b1eb57

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
- status: fixed 06b1eb57

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
- status: fixed 936bc5c7

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
- status: fixed 936bc5c7

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
- status: fixed 861c667a

### [BUG] The workspace file index keeps only one owner per (object name, kind), across all files
- where: crates/al-source/src/file_index.rs:540-547
- severity: medium
- scenario: `owners.retain(|e| e.path != path && !e.kind.eq_ignore_ascii_case(&info.kind))`
  keeps an existing owner only when it differs in *both* path and kind, so an existing owner
  of the same kind in a **different file** is evicted. In a multi-app workspace (an app plus
  its test app, or several customer apps under one root — the ordinary Zed/VS Code layout)
  both apps commonly declare `codeunit "Install"`, `codeunit "Upgrade"` or
  `page "Setup"`. Whichever file is indexed last wins: go-to-definition from app A's code on
  `Install` jumps into app B's file, `object_paths` returns one path although its doc comment
  promises "every file that declares an object named `name`", and `object_count` under-reports
  the workspace. Object names are only unique within one extension in AL, not across them.
- fix: keep one owner per (path, kind) rather than one per kind — `retain(|e| e.path != path
  || !e.kind.eq_ignore_ascii_case(&info.kind))` — and make `object_path`/`object_path_of_kind`
  prefer the owner under the same project root as the referring file.
- status: fixed a8ead4ab

### [BUG] A dangling symlink or unreadable subdirectory aborts the whole toolchain search
- where: crates/al-project/src/toolchain.rs:409-448 (`search_dir_recursive`)
- severity: low
- scenario: the walk propagates every `read_dir`, `file_type` and `canonicalize` failure with
  `?`. `canonicalize` fails on a dangling symlink, which a `dotnet tool` store routinely
  contains after a version is removed, and `read_dir` fails on a subdirectory the user cannot
  read. Either one makes the entire package directory report `Err` — `search_dotnet_tool_store`
  then logs it and skips that package directory, so a single broken link inside the only
  installed AL toolchain hides that toolchain and the workspace reports `has_toolchain: false`.
- fix: skip an entry whose `canonicalize`/`file_type` fails and continue the walk, the same
  way `search_dotnet_tool_store` already tolerates a failing package directory. Only an error
  reading `root` itself should abort.
- status: fixed e705e010

### [SIMPLIFY] Three separate JSONC strippers
- where: crates/al-types/src/jsonc.rs:11 (`strip_json_comments`),
  crates/al-project/src/config.rs:759 (`strip_jsonc`),
  crates/al-lsp/src/server/workspace.rs:1454 (`strip_jsonc_comments_and_parse`)
- severity: low
- scenario: `al-project` already depends on `al-types`, and `config.rs:759` is a near
  character-for-character copy of the `al-types` version with its own comment/string state
  machine. The old audit item was fixed by adding block-comment support to `al-types::jsonc`;
  the two other copies were not part of that fix, so a future correction (for example the
  quotes-inside-block-comments desynchronisation the `al-types` tests cover) has to be made
  three times.
- fix: delete `al-project::config::strip_jsonc` and the al-lsp copy's stripper and call
  `al_types::jsonc::strip_json_comments`.
- status: fixed 861c667a

### [SLOP] `CoreInitError::SymbolPackages` is unreachable
- where: crates/al-workspace/src/lib.rs:775
- severity: low
- scenario: the variant wraps `al_symbols::PackageLoadError`, which was produced by the old
  all-or-nothing `load_packages_cached` call. `initialize_core_workspace` now uses
  `load_packages_cached_lenient`, which returns failures in `CoreInitResult`, and nothing else
  in `al-workspace` produces a `PackageLoadError`. The variant and its `#[from]` remain.
- fix: remove the variant.
- status: fixed 58645d20

### [BUG] [STILL-OPEN] The dependency source index is still all-or-nothing per package
- where: crates/al-workspace/src/lib.rs:407-419
- severity: low
- scenario: the audit's per-file fix landed (a `.al` that does not parse is skipped with a
  warning), but the two package-level calls still use `?`:
  `source_index::get_or_build(app_path)?` and `source_index.extract_all_sources()?`. One
  package whose embedded source trips a limit in `source_index` (a single `.al` over 32 MiB,
  1 GiB total, a non-UTF-8 embedded file, or an archive entry that disappeared because the
  `.app` was rewritten mid-build) fails the whole generation, so call-graph and insight
  features go dark for every package rather than for that one.
- fix: collect per-package failures into a list the way `load_packages_cached_lenient` does
  and index the packages that succeed.
- status: fixed 58645d20

### [BUG] A call-graph build in flight republishes a stale graph over a concurrent invalidation
- where: crates/al-workspace/src/lib.rs:518-637 (`get_or_build_call_graph`), :331-340
  (`invalidate_insight_graph`), :341-349 (`invalidate_call_graph_only`)
- severity: medium
- scenario: the build reads `self.file_index` inside `build`, which runs for 100-200 ms, and
  publishes afterwards. `invalidate_insight_graph` sets the caches to `None` without taking
  `call_graph_build_lock`. Sequence: thread A takes the build lock and starts building from
  the file index as of t0; the user saves a file, so `file_refresh.rs:172-174` calls
  `file_index.add_file` and then `invalidate_insight_graph` (caches → `None`); thread A then
  writes its t0 graph and a dependency fingerprint that still matches, because the fingerprint
  covers only `.app` paths/sizes/mtimes and says nothing about workspace files. Every later
  query hits the cache and gets a call graph that does not contain the saved edit, until an
  unrelated invalidation happens.
- fix: `Workspace` already has `generation_revision` and `mark_generation_changed` for exactly
  this staging pattern. Capture `generation_revision()` before `build`, and at publication
  time only store the result if the revision is unchanged. Otherwise take
  `call_graph_build_lock` in the invalidators.
- status: fixed 8432281a. Uses two dedicated counters bumped by the invalidators rather than
  `generation_revision`, which the invalidators do not bump, and tags each published graph with
  the counter read before the build so a stale publication is a cache miss.

### [BUG] Deleting one `.app` while the daemon runs disables the call graph for every package
- where: crates/al-workspace/src/lib.rs:484-507 (`dependency_package_fingerprint`)
- severity: low
- scenario: the fingerprint walks `symbols.loaded_package_paths()` and propagates the first
  `fs::metadata` failure as `DependencySourceError::InspectPackage`. `loaded_package_paths`
  reflects what was indexed at load time, so deleting or renaming one `.app` in `.alpackages`
  (a symbol re-download, a `git clean`) makes `get_or_build_dependency_source_index` and
  `get_or_build_call_graph` return `Err` for the whole workspace until packages are reloaded,
  even though every other package is still present and indexed.
- fix: skip a package whose metadata cannot be read, record it, and build from the rest; the
  fingerprint change alone already forces the rebuild.
- status: fixed 58645d20

### [SLOP] Two `DependencySourceError` variants are never constructed
- where: crates/al-workspace/src/lib.rs:102-117 (`ParseSource`, `MissingObjectDeclaration`)
- severity: low
- scenario: both were produced by the pre-fix all-or-nothing dependency-source build. The
  current code logs and `continue`s for those two cases (:425-458), and a repo-wide grep for
  `DependencySourceError::ParseSource` / `::MissingObjectDeclaration` finds no construction
  site. The variants and their format strings remain as dead surface on a public enum.
- fix: remove both variants.
- status: fixed 58645d20

### [BUG] One unrecognized `al.*` key in `.vscode/settings.json` fails daemon and CLI startup
- where: crates/al-project/src/config.rs:414 (`key.starts_with("al.")`), :595-598
  (`_ => unknown_keys.push(key)`), :371-380 (`load_effective` turns issues into
  `ConfigLoadError::InvalidSettings`)
- severity: medium
- scenario: `merge_editor_settings` filters bare keys through `is_al_setting_key` but admits
  **any** key beginning with `al.`, and `merge_in_place` then reports every key it does not
  model as unknown. `load_effective` converts a non-empty issue list into a hard error, and
  `merge` is atomic, so the whole settings file is discarded as well. A `.vscode/settings.json`
  that carries an `al.` setting this crate does not model — a Microsoft AL Language extension
  setting outside the 23-key list, a forward-compatible key, or a plain typo such as
  `"al.enablecodeanalysis": true` — makes `al-explorer build`
  (crates/al-explorer/src/cli/commands/build.rs:128), the daemon
  (crates/al-lsp/src/server/daemon/mod.rs:130), the MCP server
  (crates/al-lsp/src/server/mcp.rs:1327) and `al-lsp` startup
  (crates/al-lsp/src/bin/al-lsp.rs:380) all fail to start. The settings file is shared with
  Microsoft's extension, so keys this project does not know about are the normal case.
- fix: treat an unknown `al.*` key from an editor settings file as a warning: log it, keep the
  keys that did parse, and reserve the hard error for a key whose *value shape* is wrong. Keep
  the strict behaviour for `AlConfig::load` of this project's own persisted settings file.
- status: fixed db935d62. A wrong value shape is also a warning, not an error: Microsoft types
  `al.backgroundCodeAnalysis` as an enum string and `al.compilationOptions` as an object, so a
  real settings file hits that path too. Only an unreadable settings root still fails.


### [PERF] [STILL-OPEN] Substring search still scans the whole name catalogue
- where: crates/al-symbols/src/index.rs:960-969
- severity: low
- scenario: after the exact and prefix stages the third stage iterates every unique lowercase
  object name and runs `contains` on each. With a Base Application-scale index that is roughly
  50 000 `contains` calls per query, and it runs whenever the prefix stage produced fewer than
  `limit` hits — the ordinary case for a workspace-symbol query like `xyz` or any mid-identifier
  fragment. The companion `search_in_package` half of the old audit item is fixed (it now uses
  the `by_package` bucket).
- fix: if this shows up on the hot path, add a trigram or suffix-start index over
  `sorted_names`; otherwise cap the substring stage at a fixed scan budget.
- status: open

### [GAP] Dependency source index and package source indexes are missing from memory stats
- where: crates/al-workspace/src/lib.rs:640-696 (`memory_stats`), :180-186
  (`dependency_source_index`)
- severity: low
- scenario: `WorkspaceMemoryStats` reports the symbol index, document store, file index,
  package metadata and both graphs, but not `dependency_source_index`, which holds a whole
  `FileIndex` over every embedded `.al` of every loaded package (text plus tree-sitter tree
  per file — the largest single allocation in the process for a source-bearing Base
  Application), and not the global `source_index::SOURCE_INDEX_CACHE`. The diagnostics
  endpoint therefore reports a small `tracked_bytes` while RSS is dominated by these two.
- fix: add a `FileIndexMemoryStats` for the dependency index and an accessor on
  `source_index` that sums its cached indexes.
- status: fixed e5ad9e1d (dependency index) and ddaad70f (package source indexes).

### [SECURITY] nupkg `.app` extraction misses the Windows drive-relative ZIP-slip case
- where: crates/al-symbols/src/nuget.rs:700-721
- severity: low
- scenario: the guard strips path separators and rejects `..`, empty names and embedded
  separators, then does `dest.join(raw_filename)`. A nupkg entry named `C:evil.app` survives
  every check (no `/`, no `\`, no `..`), and on Windows `PathBuf::push` replaces the whole
  path when the argument carries a drive prefix, so the file is written to `C:evil.app` in the
  current directory of drive C rather than under `dest`. A malicious or compromised NuGet feed
  configured through `al.nugetFeeds` is the delivery path. `app_inspect::safe_join`
  (crates/al-symbols/src/app_inspect.rs:340-353) already handles this correctly by rejecting
  `Component::Prefix`.
- fix: replace the string checks with the existing `safe_join` helper, or require
  `Path::new(raw_filename).components()` to be exactly one `Component::Normal`.
- status: fixed 5e4dc1e6

### [BUG] One truncated line permanently bricks the test result store
- where: crates/al-workspace/src/test_results.rs:44-84 (`append`), :88-91 (`read_all`),
  `read_records_no_lock`
- severity: low
- scenario: `append` opens the file in append mode, does `write_all` + `flush` (tokio's
  `flush` is a userspace flush, not `sync_all`), so a kill or crash between the write and
  writeback can leave a partial JSON line. `append` then calls `read_records_no_lock` to
  build its bucket counts, and that function returns `PersistenceError::CorruptRecord` for
  any line that does not deserialize. From that point on every `append` and every `read_all`
  fails, so no further test runs can be recorded and the whole history is unreadable — the
  file has no repair path short of deleting it by hand.
- fix: skip and log a line that does not deserialize (keeping the strict behaviour behind an
  explicit "verify" entry point), or rewrite the file dropping trailing garbage on the first
  corrupt read.
- status: fixed 22cc74bc (both: skip on read, repair on the next append, strict `verify`)

## Review complete

Twenty-four findings. The five that matter most:

1. `al-source/src/file_index.rs:540` keeps one object owner per (name, kind) across the whole
   workspace, so in a multi-app root the second app's `codeunit "Install"` evicts the first
   and go-to-definition jumps into the wrong app.
2. `al-project/src/config.rs:414` admits any `al.*` key from `.vscode/settings.json` and then
   reports it as unknown, and `load_effective` turns that into a hard error — one setting
   meant for Microsoft's AL extension stops the daemon, MCP server, CLI build and `al-lsp`
   from starting.
3. `al-workspace/src/lib.rs:518` publishes a call graph built before a concurrent
   `invalidate_insight_graph`, so a saved edit can be silently missing from the cached graph
   until the next unrelated invalidation. The `generation_revision` counter that would fix it
   already exists and is not used here.
4. `al-symbols/src/virtual_file.rs:605-612` renders key field names, procedure names,
   parameters and variables without `format_name`, so navigating to a package table with no
   embedded source produces `key(Key1; No.)` — not valid AL, against the function's own
   contract.
5. `al-symbols/src/source_index.rs:30` never evicts its process-global `.app` source-index
   cache outside tests, and `al-workspace`'s `memory_stats` counts neither it nor the
   dependency source index, so the daemon's largest allocations grow unbounded and invisibly.
