# Published benchmark evidence

The 2026-07-26 benchmark set is a clean-commit, release-binary comparison
against the Microsoft AL toolchain shipped in `ms-dynamics-smb.al`
17.0.2273547 (`alc` 17.0.34.45391). The raw JSON and server logs are committed
under [`benchmarks/results/published/2026-07-26/`](benchmarks/results/published/2026-07-26/).

These measurements were made on a 13th Gen Intel Core i5-1345U with 12 logical
CPUs and 32 GiB RAM, Linux 7.1.4, and a pinned six-package BC 28.1 symbol set.
Accuracy, package emission, and symbol-index results identify clean commit
`d21d0475651bff5597d14ec5974e2f9610ab76aa`; LSP results identify clean commit
`50d3bcc85614ee8fdd87ca2171e834181ad89d3b`.

## Correctness gates

The planted-defect corpus contains 14 isolated projects plus a clean control.
Findings count only when they point to the intended file within ±2 lines.

| Detector | Defects found | Clean-control false positive |
|---|---:|---:|
| Microsoft `alc` | 13/14 | 0 |
| LSP `publishDiagnostics` | 4/14 | 0 |
| Production native verifier | 14/14 | 0 |
| Advisory `nativeCheck` | 2/14 | 0 |

The production verifier result is a bounded corpus result, not a claim of
general Microsoft compiler, analyzer, or path-sensitive control-flow
equivalence.

All 60 measured native/`alc` package pairs were semantically equivalent: four
project sizes, five measured rounds after one discarded warmup, and three build
states per round. The comparator requires the exact archive entry set and
parsed JSON/XML equality, apart from narrowly tested compiler provenance,
random `ControlGUID` values, and Microsoft discovery-order nondeterminism.

## Package-build medians

Times are total wall-clock milliseconds. Each cell shows
`native / alc (alc ÷ native)`.

| Project | Files / lines | Process cold | Warm unchanged | One-file edit |
|---|---:|---:|---:|---:|
| small | 2 / 139 | 445.833 / 4,812.852 (10.795×) | 16.637 / 4,994.220 (300.188×) | 18.363 / 4,842.070 (263.686×) |
| medium | 40 / 2,476 | 454.177 / 4,787.268 (10.541×) | 29.532 / 4,733.969 (160.300×) | 31.071 / 4,556.994 (146.664×) |
| large | 200 / 12,316 | 516.013 / 4,951.148 (9.595×) | 69.169 / 5,158.778 (74.582×) | 68.983 / 4,910.091 (71.178×) |
| XL | 800 / 49,216 | 648.166 / 5,298.640 (8.175×) | 230.612 / 5,884.777 (25.518×) | 248.858 / 5,412.046 (21.748×) |

`process cold` resets all in-process state and package indexes, but does not
claim an OS page-cache flush. Backend order alternates by round, both arms see
the same staged packages and project input, and every sample records load.
Microsoft exposes only total time; native phase telemetry is published
separately rather than compared with that opaque total.

## LSP medians

The same stdio JSON-RPC client drove both servers on the 40-file medium project.
Each request has one discarded warmup and 10 measured, non-empty, error-free
samples.

| Measure | Native `al-lsp` | Microsoft EditorServices |
|---|---:|---:|
| Cold ready | 2,020.928 ms | 5,646.605 ms |
| Completion | 0.567 ms | 10.014 ms |
| Hover | 0.137 ms | 0.365 ms |
| Definition | 0.070 ms | 0.241 ms |
| Document symbols | 0.235 ms | 1.273 ms |
| Workspace symbols | 0.473 ms | 6.840 ms |

Native cold-ready waits for a real semantic `CodeAnalysis` call to finish
before probes; it is not the earlier phase-one diagnostic time. Microsoft
cold-ready includes its required active-workspace handshake, project-ready
event, active-document request, and first project diagnostic. The native server
completed standard shutdown and exited 0. Microsoft completed the shutdown
request; the client then sent `exit` and closed stdin, but the host did not
terminate within three seconds, so the harness records its explicitly scoped
forced termination.

## Symbol-index diagnostic

The native-only symbol benchmark ingested six packages and 11,799 symbols in
692.546 ms cold; seven fresh-daemon warm recalls had a 483.382 ms median.
Six measured searches returned non-empty results with medians from 2.924 ms to
11.488 ms. Microsoft exposes no equivalent isolated index API, so no comparative
ratio is claimed.

Full methodology, qualifications, exact binary/package hashes, samples, and
reproduction commands are in [`benchmarks/FINDINGS.md`](benchmarks/FINDINGS.md),
[`benchmarks/README.md`](benchmarks/README.md), and the published raw results.
