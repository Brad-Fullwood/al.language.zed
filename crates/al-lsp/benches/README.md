# al-core interpreter benchmarks

Criterion-based performance benchmarks for the AL test interpreter.

## Running

```sh
# Full benchmark run (default warm-up 3s, measurement 5s per benchmark):
cargo bench -p al-lsp

# Quick smoke-run with reduced timing (useful for CI or development iteration):
cargo bench -p al-lsp --bench interpreter -- --warm-up-time 1 --measurement-time 2

# Run a single benchmark group by name filter:
cargo bench -p al-lsp --bench interpreter -- string_ops
cargo bench -p al-lsp --bench interpreter -- record_crud
cargo bench -p al-lsp --bench interpreter -- library_assert

# Compile but do not run (check the bench builds cleanly):
cargo bench -p al-lsp --no-run
```

HTML reports are written to `target/criterion/` after each run.

## Benchmark inventory

| Benchmark | Hot path measured |
|-----------|------------------|
| `arithmetic/integer_accumulate_1000` | 1000 integer additions + `Format` dispatch per iteration |
| `arithmetic/decimal_accumulate_1000` | 1000 decimal `f64` additions per iteration |
| `string_ops/StrSubstNo/{small,medium,large}` | `StrSubstNo` with 3 substitution args |
| `string_ops/CopyStr/{small,medium,large}` | `CopyStr` mid-string extraction |
| `string_ops/IndexOf/{small,medium,large}` | `IndexOf` single-character needle scan |
| `string_ops/Format/{small,medium,large}` | `Format` value-to-text conversion |
| `record_crud/insert_find_modify_delete_100` | 100-row Insert → Get → Modify → Delete cycle |
| `filter_apply/range_1000_records` | `SetRange` + `FindSet` + `Next` walk over 1000 rows |
| `filter_apply/filter_expr_wildcard_1000_records` | `SetFilter(">0&<5")` + `FindSet` + `Next` walk over 1000 rows |
| `library_assert/AreEqual/integer_match` | Stub resolve + equality check — success path |
| `library_assert/AreEqual/integer_mismatch_error_path` | Stub resolve + equality check — error path |
| `library_assert/IsTrue/pass` | `Assert.IsTrue` success path |
| `library_assert/IsTrue/fail_error_path` | `Assert.IsTrue` failure path (error allocation) |
| `callgraph_walk/classify_all_medium_codeunit` | Router classification of a ~11-procedure codeunit |

## Baseline numbers (development machine, 2026-04-29)

Measured with `--warm-up-time 1 --measurement-time 2`.

| Benchmark | Median |
|-----------|--------|
| arithmetic/integer_accumulate_1000 | ~22 ms |
| arithmetic/decimal_accumulate_1000 | ~5 µs |
| string_ops/StrSubstNo/small | ~25 µs |
| string_ops/StrSubstNo/large | ~27 µs |
| string_ops/CopyStr/small | ~24 µs |
| string_ops/CopyStr/large | ~34 µs |
| string_ops/IndexOf/small | ~24 µs |
| string_ops/Format/small | ~23 µs |
| record_crud/insert_find_modify_delete_100 | ~170 µs |
| filter_apply/range_1000_records | ~510 µs |
| filter_apply/filter_expr_wildcard_1000_records | ~575 µs |
| library_assert/AreEqual/integer_match | ~88 ns |
| library_assert/AreEqual/integer_mismatch_error_path | ~297 ns |
| library_assert/IsTrue/pass | ~98 ns |
| library_assert/IsTrue/fail_error_path | ~183 ns |
| callgraph_walk/classify_all_medium_codeunit | ~87 µs |

> The `arithmetic/integer_accumulate_1000` benchmark includes 1000 × `Workspace::new()`
> allocations (one per `dispatch_call` inside the loop) which explains the much higher
> median compared to the decimal case. This is intentional — it benchmarks the realistic
> warm-path cost of the dispatch layer, not just raw arithmetic.
