# R1 review: al-symbols, al-semantic, al-types, al-source, al-project, al-workspace

Adversarial read-only review, 2026-09-21. Baseline: AUDIT-BACKLOG.md section
"Symbols & Project" (2026-07-31). Items from that audit that are still present in
current code are tagged [STILL-OPEN].

## Coverage

- [x] AUDIT-BACKLOG.md "Symbols & Project" re-verification
- [x] al-symbols/src/index.rs
- [x] al-symbols/src/app_reader.rs
- [x] al-symbols/src/app_inspect.rs
- [ ] al-symbols/src/manifest.rs
- [ ] al-symbols/src/cache.rs
- [ ] al-symbols/src/virtual_file.rs
- [ ] al-symbols/src/source_index.rs
- [ ] al-symbols/src/model.rs
- [ ] al-symbols/src/events.rs
- [ ] al-symbols/src/composition.rs
- [ ] al-symbols/src/nuget.rs
- [ ] al-symbols/src/bc_server.rs
- [ ] al-symbols/src/oauth.rs
- [ ] al-symbols/src/source_availability.rs + language_data.rs + lib.rs
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

