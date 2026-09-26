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
