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
