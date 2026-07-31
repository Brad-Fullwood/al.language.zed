# al-lsp benchmarks

Deterministic Criterion benchmarks for the symbol / insight hot paths.

- `perf.rs` — cold synthetic SymbolReference parse/index, cold committed `.app`
  file/archive load, warm lookup (by name & id, search), completion at a
  position, and the insight engine's graph build / `impact` / `trace`. It also
  prints owned index/workspace/graph byte totals alongside the timings.

## Running

```sh
cargo bench -p al-lsp --bench perf                                  # full run
cargo bench -p al-lsp --bench perf -- --warm-up-time 1 --measurement-time 2  # quick
cargo bench -p al-lsp --bench perf -- symbols                       # one group
cargo bench -p al-lsp --bench perf --no-run                         # compile only
```

Full run + read instructions, fixture description, and how to read the memory
metric live in [`Docs/benchmarks.md`](../../../Docs/benchmarks.md).

> The interpreter benchmarks (arithmetic / string ops / record CRUD / router)
> live in `al-test` (`cargo bench -p al-test --bench interpreter`), not here.
