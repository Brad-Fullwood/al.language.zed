# Persisted symbol and source index

Build list item 10 in `ai-tooling-ideas.md`: a second cold daemon start on the same
`.alpackages` should not rebuild the dependency source index.

## 1. Baseline

### Setup

Binaries: `al-explorer` and `al-lsp` release builds of `fa4fcf8f`.

Project: a copy of `benchmarks/projects/medium` (40 AL files, idRanges 50000-69999). Its
`.alpackages` holds six `.app` files, 66 MB in all, including Base Application 28.1 (45 MB) and
System Application 28.1 (16 MB). The daemon loads 5 packages with 11,278 symbols. The dependency
source index holds 9,743 embedded AL files (113.6 MB of source) and skips 107.

Each run starts from a stopped daemon (`al-explorer daemon-shutdown`, then wait for the process to
exit). `XDG_DATA_HOME` and `XDG_CACHE_HOME` point at a scratch directory, so the first run starts
with no cache at all and no other daemon shares the log. `AL_REQUEST_TIMEOUT_MS=600000` keeps the
client from giving up at 30 s. Wall time is measured around the `al-explorer --json` process. Peak
RSS is `VmHWM` of the daemon process read right after the query returns.

The machine is a 12-thread laptop shared with other build agents. Load average was 12 to 25 during
these runs, so single numbers move by 20% between runs. The first start happened to run at lower
load than the second and third.

### Numbers

`impact "Sales-Post"` needs the call graph, which needs the dependency source index, so it waits
for both.

| Run | Query | Wall time | Daemon peak RSS |
| --- | --- | --- | --- |
| First start, empty caches | `impact "Sales-Post"` | 38.7 s | 2,868 MB |
| Second start | `impact "Sales-Post"` | 50.2 s | 2,834 MB |
| Third start | `impact "Sales-Post"` | 51.9 s | 2,838 MB |
| Second start | `packages` | 1.40 s | 138 MB at the answer, 2,839 MB 60 s later |
| Second start | `source "Sales-Post" --procedure RunWithCheck` | 1.43 s | 182 MB |

The daemon warms the dependency source index and the call graph in the background right after
startup (`crates/al-lsp/src/server/daemon/mod.rs:219`), so every start reaches 2.8 GB within a
minute whatever the first query is.

Phases from the daemon log, first, second and third start:

| Phase | First | Second | Third |
| --- | --- | --- | --- |
| Package symbol load | 1.03 s | 1.34 s | 1.38 s |
| Dependency source index | 27.4 s | 32.9 s | 34.0 s |
| Call graph build | 10.2 s | 15.8 s | 16.6 s |

A second start is no faster than the first. `~/.local/share/al-lsp/<project-hash>/` receives
nothing. `~/.cache/al-lsp/index/` does receive a JSON copy of each package's symbols
(`crates/al-symbols/src/cache.rs`, 33.6 MB for Base Application), and the package phase is no
faster with it than without it.

### Where the time goes

A throwaway harness ran the same steps as the daemon in one process against the same packages,
timing each step, at load average 25. The daemon builds the dependency source index on one
thread, and the harness did the same. The two package rows ran one package after another, where
the daemon runs packages in parallel, so in the daemon Base Application alone sets the length of
the package phase.

| Step | Time | Notes |
| --- | --- | --- |
| Header scan of every package (`source_index::get_or_build`) | 1.33 s | Part of the package phase, Base Application is most of it |
| Reading the JSON symbol cache, all packages | 0.92 s | Also part of the package phase |
| Extracting all embedded AL from the zips | 0.57 s | |
| `parse_quick` on 9,850 files | 26.7 s | The largest single cost |
| `FileIndex::add_file_with_tree` | 8.4 s | Document symbols and name maps nothing reads for dependency files |
| `register_dependency_source_nodes` | 1.7 s | Tree walk per object |
| `populate_workspace_call_edges` on dependency files | 13.8 s | 88,506 procedures resolved, 88,827 edges |
| Of that, extracting call sites and variable types | 3.7 s | 343,457 call sites |

Peak RSS of the harness was 2,863 MB. The daemon's own accounting
(`diag`, `dependencySourceIndexMemory`) tracks 138 MB: 113 MB of source text and 25 MB of index
maps. The text is held twice, once in `files` and once beside each tree in `file_trees`. The rest,
about 2.4 GB, is the 9,743 tree-sitter trees, which no accounting counts.

What reads the trees: only the call graph build (`register_dependency_source_nodes`,
`populate_workspace_call_edges`) and transaction lint (`resolve_all_workspace_call_edges`,
`collect_effects`). Nothing else takes the dependency `FileIndex`.

## 2. Design

### What is cached

The dependency source index, in a smaller form. Today it is a `FileIndex` holding every embedded
file's text twice plus its tree-sitter tree. A tree cannot be written to disk, and the two
consumers read only a little from it. The new index keeps, per package, one summary per embedded
file:

- the file's archive path and its object declarations (kind, id, name) in document order
- per object, how many call suffixes it holds, which ranks it for eager edge resolution
- per procedure or trigger, in document order: name, attributes as written, the `local` modifier,
  call sites, record variable types, object variable types, and the database writes and
  `Commit()` calls with their positions and labels

The summary is what the call graph build and transaction lint read. The build parses each file,
summarizes it and drops the tree, so neither the trees nor the source text stay in memory, and
neither is written to disk. The first start still parses every file, and it now parses and
summarizes them on the rayon pool instead of one thread.

Not cached:

- Call graph edges. Edges out of dependency code depend on the workspace: a workspace codeunit
  implementing a dependency interface receives the edges of interface dispatch, and a workspace
  subscriber is linked from every dependency publisher that raises its event. Edges are resolved
  from the summaries on every start, with the same resolver the tree path uses.
- The package symbol index. It already has a disk cache (`~/.cache/al-lsp/index/`), measured
  above at no gain, and the package phase is 1.4 s against 40 s for the source index and graph.
  The header scan inside it (about 1 s) could use the same key and directory later.

### Format

`serde_json`, which every existing cache in the repository already uses. `Cargo.lock` has no
binary serde format outside dev dependencies (`ciborium` comes from `criterion`). The file layout
follows `crates/al-symbols/src/cache.rs`: a 4-byte little-endian header length, a JSON header,
then the JSON body, so a stale or foreign entry is rejected after parsing only the header. If
decoding JSON turns out to cost more than the step it replaces, a binary format is the first
thing to revisit, with `cargo deny check`.

### Key and location

Per package, one file:
`<user data dir>/al-lsp/<project-hash>/source-index/<app file name>.<key>.summary`.
`<project-hash>` is the FNV-1a hash of the project root that `test-results.json` already uses.
`<key>` is an FNV-1a hash over the schema version constant, a fingerprint of the grammar
(FNV-1a of `tree-sitter-al`'s `NODE_TYPES`) and the SHA-256 of the `.app` bytes. The header
repeats all of these plus the app id, name, version and byte length, and a load checks every one.
A summary schema change bumps the constant. A grammar change changes the fingerprint.

The daemon sets the directory after it finds the project. A workspace without one (the LSP
path, unit tests, no data directory) builds without persistence, exactly as today.

### Invalidation and garbage collection

Per package. A package whose bytes change gets a new key, misses, and is rebuilt alone. The
others load. After a generation is built, files in the project's `source-index/` directory that
no current package uses are deleted, and temporary files older than 60 s are swept, the same rule
as `cache.rs`. A package without embedded source writes no file. Directories of projects that no
longer exist are not collected, which is already true of `test-results.json`.

### Corrupt or stale files

Any failure is a miss: a file that cannot be opened, is over the 256 MB limit, has a mismatched
header, or does not decode. The package is rebuilt from its `.app` and the entry rewritten. A
failed write is logged and ignored. Writes go to a temporary file in the same directory and are
renamed into place, so a reader never sees half a file. No cache failure fails a request.

### Trust and ownership

Loading an entry runs nothing. It decodes plain data structs with `serde_json`. It does not
load analyzers, start `dotnet`, or read repository settings, so it needs no trust decision. The
entries live in the user's data directory, outside every repository, and their input is the
same `.app` bytes the build would parse. `al.packageCachePath` is already a privileged setting,
so an untrusted repository cannot move the package folder either.

Before reading, the loader checks the `source-index/` directory and the entry with
`symlink_metadata`: neither may be a symbolic link, both must be owned by the effective uid, and
neither may be writable by group or other. This is the rule the daemon socket directory uses
(`crates/al-protocol/src/endpoint.rs`), without its exception for root, since an entry root owns
is not one this user wrote. A failed check is a miss, and nothing is written into a directory
another user owns. The directory is created with mode 0700 and entries with 0600.

### Kept as fallback

The lazy build stays the entry point: `get_or_build_dependency_source_index` builds on first
need, and the daemon's startup warm-up calls it. Inside, each package is loaded from its entry or
built from its `.app`.

### Tests

- The key: changes with the schema constant, the grammar fingerprint and one byte of the `.app`.
- Invalidation per package: rewriting one of two packages rebuilds that package and loads the
  other.
- Equality: summaries of a fixture package built from source equal the same summaries loaded
  from disk, and the call graph built from them equals the graph the tree path builds from the
  same sources, node for node and edge for edge. The fixture packs the harness project
  `crates/al-test-harness/data/test_al_project/src` plus a file with interface dispatch, record
  triggers, `Codeunit.Run`, events, subscribers, a `[TryFunction]` with a write, a `Commit()`,
  two objects in one file and overloads.
- A corrupt entry, an entry with a foreign header, and an entry in a directory with open
  permissions each fall back to a rebuild and give the same index.
- Transaction lint gives the same diagnostics from summaries as from trees on the existing
  dependency fixtures in `transaction_lint.rs`.

### Expected effect

A second start loads summaries instead of parsing 9,743 files (27 s) and filling a `FileIndex`
(8 s). Edge resolution (about 10 s of the 14 s graph step) still runs. Resident memory should
fall by the size of the trees, about 2.4 GB. Step 4 measures both.

## 3. Result

### Setup

The same project copy, the same commands and the same environment as section 1: `XDG_DATA_HOME`
and `XDG_CACHE_HOME` in a scratch directory, `AL_REQUEST_TIMEOUT_MS=600000`, wall time around
the `al-explorer --json` process, and peak RSS as `VmHWM` of the daemon.

Before is the campaign branch at `a8bb710e`, which holds everything on this branch except the
summary work. After is this branch at `1c9b3e44`. Both were built in the same session and run
one after the other, before then after, for each row. Each run stops the daemon and waits for it
to exit, runs the query, reads `VmHWM` at the answer, waits for the log line that says the
background warm-up finished, waits another 10 s (60 s for `packages`), and reads `VmHWM` again.

The 1-minute load average was 0.6 to 7.7 during these runs, against 12 to 25 in section 1. At
this load the before binary answers the first start in 17.2 s, where section 1 measured 38.7 s,
so the before column is measured again here instead of copied from section 1.

### Numbers

| Run | Query | Wall, before | Wall, after | Peak RSS, before | Peak RSS, after |
| --- | --- | --- | --- | --- | --- |
| First start, empty caches | `impact "Sales-Post"` | 17.2 s | 4.6 s | 2,857 MB | 526 MB |
| Second start | `impact "Sales-Post"` | 23.6 s | 1.25 s | 2,834 MB | 371 MB |
| Third start | `impact "Sales-Post"` | 25.0 s | 1.55 s | 2,836 MB | 372 MB |
| Fourth start | `packages` | 0.88 s | 1.31 s | 135 MB at the answer, 2,833 MB 60 s later | 135 MB at the answer, 372 MB 60 s later |

Load average at the start and end of each run:

| Run | Before | After |
| --- | --- | --- |
| First start | 0.56 to 0.56 | 0.56 to 1.25 |
| Second start | 1.55 to 2.50 | 2.50 to 2.46 |
| Third start | 2.46 to 5.09 | 5.09 to 4.91 |
| Fourth start | 5.00 to 7.31 | 7.31 to 7.68 |

Phases from the daemon log, with the source index time confirmed by `diag`:

| Phase | Before, first | Before, second | Before, third | After, first | After, second | After, third |
| --- | --- | --- | --- | --- | --- | --- |
| Package symbol load | 0.79 s | 0.74 s | 0.65 s | 0.75 s | 0.62 s | 0.80 s |
| Dependency source index | 11.13 s | 15.62 s | 16.04 s | 3.48 s | 0.31 s | 0.37 s |
| Call graph build | 5.27 s | 7.17 s | 8.27 s | 0.35 s | 0.29 s | 0.34 s |
| Packages read from disk | | | | 0 of 5 | 4 of 5 | 4 of 5 |

The fifth package, Application, embeds no source, so it has no entry and costs nothing
to summarize again. The `packages` query does not wait for the index, and its 0.4 s difference
follows the load: the package phase is the same code in both builds.

In one process, with `crates/al-workspace/examples/dep_profile.rs` at load 6 to 9:

| Run | Package symbols | Source index | Call graph | Call edges | Peak RSS |
| --- | --- | --- | --- | --- | --- |
| No cache directory | 1.06 s | 4.94 s | 0.40 s | 88,827 | 503 MB |
| Empty cache directory, build and write | 1.06 s | 5.29 s | 0.37 s | 88,827 | 515 MB |
| Filled cache directory, load | 1.01 s | 0.32 s | 0.33 s | 88,827 | 384 MB |

The summaries take 71 MB in memory. On disk they take 59 MB for this package set, 57 MB of it
for Base Application.

### Same answers

- `diag` reports the same graph sizes to the byte from both builds: 30,253,681 tracked bytes for
  the insight graph and 28,325,746 for the call graph. The log reports 62,369 nodes and 59,118
  insight edges from both. The harness counts 88,827 call edges, as in section 1.
- `impact "Sales-Post"` and `insight-stats` return the same bytes from both builds.
- `impact "Customer" --scope packages` (760 rows) and `entrypoints --scope packages` (34,420 rows)
  return the same rows from the before build, from the after build building its summaries, and
  from the after build loading them. The before build also returns these rows in a different
  order on each start, so they were compared as sets.

### Against the expected effect

Section 2 expected a second start to skip the parse (27 s) and the `FileIndex` fill (8 s), to
keep about 10 s of edge resolution, and to hold about 2.4 GB less.

- The index step on a second start takes 0.31 to 0.37 s, where the before build takes 15.6 to
  16.0 s at the same load. It hashes each `.app` and decodes four JSON entries.
- The call graph step takes 0.3 s, against 5.3 to 8.3 s before, where section 2 expected about
  10 s to remain. The resolver is the same function with the same inputs and gives the same
  edges. What the step no longer does is read trees: find each procedure's declaration in its
  object and walk its body for calls and variable types. The summary build does that once per
  file, on the rayon pool, and a loaded summary skips it.
- Peak RSS is 371 MB against 2,834 MB, 2.46 GB less, because no trees are kept.
- The first start also gains: 4.6 s instead of 17.2 s, because files are parsed on every core,
  and 526 MB instead of 2,857 MB, because each tree is dropped once its file is summarized.

### What fell short

The second start meets every target in section 2. Three limits remain:

- A first start still parses every embedded file. Here that is 3.5 s on 12 threads at load 0.6.
  It grows with fewer cores or more load: section 1 took 27 s on one thread at load 25.
- The package phase is unchanged at 0.6 to 1.3 s and is now half of a second start.
- The before and after figures come from one session at load 0.6 to 7.7. Section 1 ran at 12 to
  25, and its figures are about twice these.

### What is left

- The key does not cover the code that builds a summary (`SourceFileSummary::from_tree` and
  `file_effect_sites` in `al-insight`). A change there without a bump of `SCHEMA_VERSION` in
  `crates/al-workspace/src/source_cache.rs` leaves every existing entry in use, and the daemon
  answers from summaries the old code built until the package changes. A test that compares
  the summary of a fixture file with a snapshot kept in the repository, and fails with a message
  that names the constant, would catch it.
- Entries are kept per project, so two projects on the same Base Application each store 57 MB. A
  directory keyed by the package hash alone would share them, with the same ownership checks.
- The header scan in the package phase could use the same key and directory, as section 2 says.
- `entrypoints` and `impact` return their rows in a different order on each daemon start. Sorting
  them would let a client compare two answers byte for byte.

### Tests

| Design item | Test |
| --- | --- |
| The key changes with the schema constant, the grammar fingerprint and one byte of the `.app` | `the_key_follows_the_bytes_the_schema_and_the_grammar` |
| Rewriting one of two packages rebuilds that package and loads the other | `a_rewritten_package_is_rebuilt_alone` |
| Summaries built equal summaries loaded, and the graphs built on them match | `a_second_start_loads_every_package_and_equals_the_fresh_build` |
| The graph from summaries equals the graph from trees, node for node and edge for edge | `summary_graph_equals_tree_graph_node_for_node_and_edge_for_edge` |
| A corrupt entry rebuilds to the same index | `a_corrupt_or_truncated_entry_falls_back_to_a_rebuild` |
| An entry with a foreign header rebuilds to the same index | `an_entry_written_for_other_bytes_is_refused` |
| An entry or directory others can write, or a linked entry, is not read | `entries_other_users_could_write_are_not_read` |
| Transaction lint gives the same diagnostics from summaries as from trees | `dependency_summaries_lint_like_dependency_trees` |

The graph test is in `crates/al-insight/src/calls/summary_tests.rs`, the lint test in
`crates/al-analysis/src/queries/transaction_lint.rs` and the rest in
`crates/al-workspace/src/source_cache_tests.rs`. Step 4 added the lint test. It runs the
dependency fixtures through transaction lint once from the summaries and once from a `FileIndex`
over the same embedded files, and compares the diagnostics. It fails when the summary path drops
the effects of one procedure. Step 4 also added two assertions: a directory others can write
rebuilds to the same summaries, and one changed byte of the `.app` names a different entry.

## 4. Follow-ups

### The key covers the summary builder

`PackageKey` holds a third fingerprint beside the schema version and the grammar:
`al_insight::calls::summary_builder_fingerprint`, the FNV-1a hash of the JSON summary that
`SourceFileSummary::from_tree` makes of `SUMMARY_FIXTURE`. The fixture is an AL file in
`crates/al-insight/src/calls/summary_fixture.al` with several objects in one file, interface
dispatch, an event and its subscribers, record triggers, `Codeunit.Run`, overloads, a temporary
record, five kinds of database write and a `Commit()`. The hash is computed once per process, and
it goes into the entry name and the header, so a build whose summary code gives other output for
the fixture misses on every old entry. A change the fixture does not exercise leaves the
fingerprint the same. For that case `fixture_summaries_match_the_snapshot_of_this_schema_version`
builds a fixture package (the fixture file, a second codeunit, a file that does not parse and a
file with no object) and compares its summaries with `crates/al-workspace/testdata/summary_snapshot.json`
byte for byte. The snapshot records `SCHEMA_VERSION`, and `UPDATE_SUMMARY_SNAPSHOT=1` rewrites it
only when it was written under another version, so the snapshot and the constant change together.
`an_entry_written_by_another_summary_builder_is_a_miss` checks that another builder fingerprint
names another entry and that an entry whose header carries one is summarized again, and
`the_builder_fingerprint_hashes_the_fixture_summary` in al-insight checks the hash. Commit
`7d9c97c3`.

### `entrypoints` and `impact` rows in one order

Both queries listed rows in the order the symbol index hands out its entries. That is the
iteration order of a `DashMap`, whose hasher is seeded per map, so two daemon starts over the same
packages gave the same rows in a different order. `find_entry_points` in
`crates/al-insight/src/search.rs` now sorts its rows by object kind, then object name and
procedure name ignoring case, then as written. The impact dedupe in
`crates/al-analysis/src/queries/impact.rs`, now `sort_and_dedupe`, sorts on every field of a row
before it drops repeats, workspace rows first, so it also keeps the same one of two duplicates on
every start. `entry_points_come_back_in_one_order_from_every_build` and
`impact_rows_come_back_in_one_order_from_every_build` build the index several times, from the
entries in order and reversed, and compare the serialized rows. Both fail with the sort removed.
Commit `ffb4c65d`.

### Entries shared across projects

Each project kept its own store, `<user data dir>/al-lsp/<project hash>/source-index/`, and an
entry name began with the package's file name. Two projects on the same packages each stored a
full copy, and the second project summarized every package again on its first start.

A summary depends on the package bytes, the schema version, the grammar and the summary builder,
and the key covers all four. It holds archive paths inside the package and no path of the
project or of the `.app`, and `parse_quick` takes no project setting, so two projects on the same
bytes build the same summary. The per-project directory did two things besides keeping projects
apart. Garbage collection deleted an unused entry when a kept entry had the same package file
name, which is right only while one project uses the directory: in a shared store, two projects
with a package of one name and version but other bytes, such as two localizations of Base
Application, would delete each other's entry on every start. And the 1 GiB limit applied to each
project.

What changed:

- Commit `e4d047a6` names an entry by the package name and version from its manifest, then the
  key hash. The file name is no longer part of it, so one package under two file names reads one
  entry. `load`, `save`, `entry_path` and `entry_name` take the key alone.
  `the_same_bytes_under_another_file_name_read_the_same_entry` covers it.
- Commit `98c818e4` makes `SourceSummaryCache::for_project` return
  `<user data dir>/al-lsp/source-index` for every project. It deletes the entries and temporary
  files of the project's old store, and the directory once it is empty, but only when that
  directory is a real directory that this user alone owns and can write. Garbage collection no
  longer has the same-name rule. An unused entry goes after 30 days without a load or a write,
  or, least recently used first, once the store passes 1 GiB, which is now a limit for the user.
  The 0700 directory, the 0600 entries, the owner checks and the fallback on a corrupt or foreign
  entry are unchanged.

Tests in `crates/al-workspace/src/source_cache_tests.rs`:

| Behaviour | Test |
| --- | --- |
| Two projects on the same packages use one directory, and the second reads the entries the first wrote | `two_projects_on_the_same_packages_share_their_entries` |
| Two projects with other bytes under one package name and version both keep their entry across alternating starts | `projects_with_other_bytes_under_one_package_name_keep_both_entries` |
| An unused entry with the same package name as a kept one stays | `garbage_collection_keeps_entries_another_project_may_use` |
| Past the size limit the least recently used unused entries go, and an entry in use stays | `garbage_collection_past_the_size_limit_drops_the_least_recently_used` |
| The old per-project store is deleted, the project's other files stay | `the_store_a_project_kept_before_is_removed` |
| An old store that is a symbolic link is not followed | `a_linked_project_store_is_not_followed` |

`a_rewritten_package_is_rebuilt_alone` now expects the entry of the old bytes to stay. Each test in
the table except the size limit test fails with its part of the change put back: the
per-project directory, the same-name rule, the removal of the old store, and the owner check
before that removal. The size limit test passes before and after. It pins the existing rule now
that the limit covers every project.

Measured with debug builds of `al-explorer` and `al-lsp` at `e4d047a6` (before) and `98c818e4`
(after), on two copies of the medium benchmark project in a scratch directory, with
`XDG_DATA_HOME`, `XDG_CACHE_HOME` and `XDG_RUNTIME_DIR` in that directory. Each project ran
`impact "Sales-Post"` from a stopped daemon, which waits for the dependency source index, then
stopped the daemon. Sizes are `du -sb` of `<data>/al-lsp` without the log. The upgrade row runs
the before build on both projects, then the after build on the same data directory.

| Run | Store on disk | Second project, packages read from disk |
| --- | --- | --- |
| Before | 123,611,796 bytes, 61,805,898 in each project's store | 0 of 5 |
| After | 61,805,898 bytes in `al-lsp/source-index` | 4 of 5 |
| Upgrade, after build on the before build's data | 61,805,898 bytes, both old stores removed (4 entries each) | 4 of 5 |

On the upgrade, the first project summarizes its packages once more, because the old entries are
deleted rather than moved. Moving them would need the header of each entry read to build its new
name, which saves one build per package set once.

The doc comment on `persist_dependency_source_summaries` in
`crates/al-lsp/src/server/daemon/mod.rs` still says the summaries live in the project's data
directory. That file belongs to another branch in this campaign, so the comment is left for it.

### The package header scan

The scan is contained: `AppSourceIndex::from_app_path` in `crates/al-symbols/src/source_index.rs`
reads the first 256 KiB of every embedded `.al` file and returns plain data, two maps from object
kind with id or name to an archive path, and the list of every `.al` path. Timed in one release
process on the medium package set at load 11 to 15, it takes 0.49 to 0.54 s for Base Application,
which sets the length of the step because packages load in parallel, and SHA-256 of the same
45 MB takes 0.03 s. The same key and store still do not fit. The grammar and builder fingerprints
in the summary key do not describe the scan, which is a byte scanner (`parse_object_headers`)
without tree-sitter, and nothing fingerprints that scanner, so a change to it would keep old
entries in use. The scan also runs where the store cannot reach it: al-symbols runs it while it
loads packages (`prewarm_source_index` in `crates/al-symbols/src/index/loading.rs`), below
al-workspace, which owns the store, and the daemon sets the store after that load
(`crates/al-lsp/src/server/daemon/mod.rs`, lines 212 and 214). A summary cannot replace the scan
either, since it leaves out the 107 embedded files that do not parse cleanly or declare no object,
and the scan still lists every file and reads the object headers of files that do not parse. The
alternative is the symbol cache in `crates/al-symbols/src/cache.rs`. `load_package_via_cache`
reads that cache's entry for the same package just before it runs the scan, and the entry is
checked on modification time and size, the check the in-memory scan cache already uses. Storing
the two maps and the path list in that entry, 2 MB in memory for Base Application, with a bump of
`CACHE_SCHEMA_VERSION` and a snapshot test of the scan like the summary snapshot, would save up
to 0.5 s of a second start that section 3 measured at 1.25 s.

## Follow-ups complete

Done:

- The key covers the summary builder: `7d9c97c3`.
- `entrypoints` and `impact` return their rows in one order: `ffb4c65d`.
- Entries are named by the package and shared by every project: `e4d047a6` and `98c818e4`.

Left:

- The header scan through the al-symbols symbol cache, as the paragraph above describes.
- The doc comment on `persist_dependency_source_summaries` in
  `crates/al-lsp/src/server/daemon/mod.rs`, which still names the project's data directory.
- From section 3: a first start still parses every embedded file, 3.5 s on 12 threads at load
  0.6 and more with fewer cores or more load.
