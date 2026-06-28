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
| `al-lsp` | `perf` | **symbol & insight engine** — cold load, warm lookup, completion, impact, trace (C6) |

This note focuses on the `al-lsp perf` bench (gap **C6**). The other two are
documented in their bench file headers.

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
```

HTML reports (with regression detection vs the previous run) are written to
`target/criterion/` after each run; open `target/criterion/report/index.html`.

## What is measured

The fixture is a **synthetic AL workspace** generated in-bench from the SCALE
constants at the top of `crates/al-lsp/benches/perf.rs` (default ~640 objects:
200 tables, 200 codeunits, 120 pages, 80 enums, 40 interfaces). The codeunits
form an event ring (each publishes one integration event and subscribes to the
previous one) and every table relates to `BenchTable0`, so the `impact` and
`trace` queries against object 0 exercise a realistic fan-in. To grow the
workload, bump the SCALE constants — the generator is a pure function of them.

> The bench can instead be pointed at a committed fixture
> (`crates/al-test-harness/data/test_al_project`); the synthetic generator is
> used by default because it is self-contained (no filesystem) and scalable.

| Benchmark id | Hot path |
|--------------|----------|
| `symbols/cold_load_parse_index` | Parse SymbolReference JSON + index it (workspace-open cost) |
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

These are **time** benchmarks; Criterion does not measure memory. To answer the
C6 "+ memory data" ask honestly, the bench prints one extra line to stderr
exactly once per run, before the timings:

```
[C6 MEMORY] indexed_symbols=642 serialized_bytes=… bytes_per_symbol=… insight_nodes=… insight_edges=…
```

- `indexed_symbols` — number of entries in the index (`SymbolIndex::len`).
- `serialized_bytes` / `bytes_per_symbol` — the **approximate** memory metric:
  the serialized-JSON size of the fixture (its on-disk SymbolReference
  footprint), used as a stable proxy for in-memory size. It does **not** count
  `Arc`/`DashMap` overhead, so the live index is somewhat larger — this is a
  byte-budget indicator, not exact RSS accounting.
- `insight_nodes` / `insight_edges` — size of the graph the insight queries walk.

Because the fixture is deterministic, these numbers are stable across runs and
move only when the fixture (SCALE constants) or the data model changes.
