# Performance benchmarks

Criterion-based micro-benchmarks for the project's hot paths. They run via
`cargo bench` (never `cargo test`) and are deterministic: every bench builds its
fixture from a fixed seed (synthetic in-bench data or a committed fixture), so
the same work is measured on every run.

There are three benchmark targets:

| Crate | Bench | Hot paths |
|-------|-------|-----------|
| `al-syntax` | `parser` | `parse`, `parse_incremental`, `format_al` (keystroke path) |
| `al-test` | `interpreter` | arithmetic / string ops / record CRUD / filters / router classify |
| `al-lsp` | `perf` | symbol and insight engine — cold load, warm lookup, completion, impact, trace |

This note focuses on the `al-lsp perf` bench. The other two are documented in their bench file
headers.

## Running

```sh
# Full run (warm-up 3s + measurement 5s per benchmark — a few minutes):
cargo bench -p al-lsp --bench perf

# Quick smoke run (seconds) — enough to see the numbers move:
cargo bench -p al-lsp --bench perf -- --warm-up-time 1 --measurement-time 2

# One group at a time (substring filter on the benchmark id):
cargo bench -p al-lsp --bench perf -- symbols
cargo bench -p al-lsp --bench perf -- insight
cargo bench -p al-lsp --bench perf -- completion

# Compile the benches without running them (CI gate / quick check):
cargo bench -p al-lsp --bench perf --no-run

# The native-only CI audit: short Criterion samples, fixed fixture, measured
# values in the job log. It is intentionally not a cross-machine pass/fail SLA.
cargo bench -p al-lsp --bench perf -- --warm-up-time 0.1 --measurement-time 0.2 --sample-size 10
```

HTML reports (with regression detection vs the previous run) are written to
`target/criterion/` after each run; open `target/criterion/report/index.html`.

## What is measured

Most benchmarks use a **synthetic AL workspace** generated in-bench from the SCALE
constants at the top of `crates/al-lsp/benches/perf.rs` (default ~640 objects:
200 tables, 200 codeunits, 120 pages, 80 enums, 40 interfaces). The codeunits
form an event ring (each publishes one integration event and subscribes to the
previous one) and every table relates to `BenchTable0`, so the `impact` and
`trace` queries against object 0 exercise a realistic fan-in. To grow the
workload, bump the SCALE constants — the generator is a pure function of them.

The second cold-load benchmark uses
`crates/al-lsp/benches/fixtures/representative.app`, a committed NAVX package
produced by the verified native emitter. It measures the production file,
archive, manifest, SymbolReference, and index path without relying on a
proprietary package download in CI.

| Benchmark id | Hot path |
|--------------|----------|
| `symbols/cold_load_parse_index` | Parse scalable synthetic SymbolReference JSON + index it |
| `symbols/cold_load_app_archive` | Read and validate a representative NAVX/ZIP `.app`, parse its manifest/symbols, and index it |
| `symbols/warm_lookup/get_by_name` | Object lookup by name (hash) |
| `symbols/warm_lookup/find_by_name` | First-match lookup by name |
| `symbols/warm_lookup/get_by_id` | Object lookup by `(kind, id)` |
| `symbols/warm_lookup/search_substring` | Substring search over the whole index |
| `insight/build_graph_from_index` | Build the object/event/call graph (cold) |
| `insight/trace_event` | `al trace` — walk subscriber/publisher chains |
| `insight/table_impact` | `al impact` — find every object touching a table |
| `insight/callgraph_from_insight` | Build the call graph from the insight graph |
| `completion/type_position` | Completion in a type position (drives the symbol index) |
| `completion/default` | Completion in the default keystroke context |

## Reading the output

Criterion prints, per benchmark, a line like:

```
symbols/warm_lookup/get_by_name
                        time:   [120.3 ns 121.0 ns 121.8 ns]
```

The middle value is the **median**; the brackets are the confidence interval.
On a re-run Criterion appends `change: [...] (p = ...)` and flags
`Performance has regressed` / `improved` — that is the regression signal.

### The memory metric

These are time benchmarks; Criterion does not measure memory. The bench prints one additional line
to stderr exactly once per run, before the timings:

```
[MEMORY] indexed_symbols=642 symbol_bytes=… lookup_bytes=… package_metadata_bytes=… document_bytes=… file_text_bytes=… file_index_bytes=… insight_bytes=… call_graph_bytes=… insight_nodes=… insight_edges=… rss=external
```

- `symbol_bytes` / `lookup_bytes` — symbol payloads and owned lookup/index keys.
- `package_metadata_bytes` — retained package display metadata.
- `document_bytes`, `file_text_bytes`, `file_index_bytes` — open-document text/keys and the
  workspace file-index text/secondary indexes. Cached tree counts are exposed through daemon
  diagnostics rather than converted into invented byte totals.
- `insight_bytes` / `call_graph_bytes` — retained node, edge, key, and adjacency-list allocations.
- `rss=external` — process RSS is allocator/OS-dependent and must be captured separately when it is
  useful; it is not derivable from owned allocations.

The numbers are deterministic fixture accounting, not a claim about exact process RSS. CI runs the
short native-only Criterion audit and retains its measured values in the job log. It has no threshold
or Microsoft comparison because shared-host timing is not a valid cross-tool performance claim.

Because the fixture is deterministic, these numbers are stable across runs and
move only when the fixture (SCALE constants) or the data model changes.
