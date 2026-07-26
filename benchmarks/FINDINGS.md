# Benchmark findings — 2026-07-26

This report is backed by clean-commit release runs and the versioned raw results
in [`results/published/2026-07-26/`](results/published/2026-07-26/). It replaces
the superseded dirty-worktree measurements.

| | |
|---|---|
| Host | 13th Gen Intel Core i5-1345U, 12 logical CPUs, 32 GiB RAM, Linux 7.1.4 |
| Microsoft toolchain | `ms-dynamics-smb.al` 17.0.2273547; `alc` 17.0.34.45391 |
| Symbols | six pinned BC 28.1 packages; names, sizes, and SHA-256 hashes are in each raw result |
| Accuracy / emit / symbols commit | `d21d0475651bff5597d14ec5974e2f9610ab76aa`, clean |
| LSP commit | `50d3bcc85614ee8fdd87ca2171e834181ad89d3b`, clean |

## Planted-defect detection

The harness scores an error or warning only when it is in the intended file and
within ±2 lines of the planted defect. Each defect has its own project so parse
failures cannot suppress later cases. The clean control uses `SetLoadFields`
before its record read, making it a valid false-positive control for both the
LSP native-lint configuration and the production verifier.

| Defect class | n | `alc` | LSP | native build | `nativeCheck` | combined local |
|---|--:|--:|--:|--:|--:|--:|
| syntax | 3 | 3 | 3 | 3 | 0 | 3 |
| binding / name resolution | 5 | 5 | 0 | 5 | 0 | 5 |
| type checking | 4 | 3 | 0 | 4 | 0 | 4 |
| project rules | 2 | 2 | 1 | 2 | 2 | 2 |
| **total** | **14** | **13** | **4** | **14** | **2** | **14** |
| false positive on clean control | | none | none | none | none | none |

`native build` is `al-explorer pack-native --json`, which exercises the
production verifier used by native builds. It catches the three syntax/property
cases; undeclared names, unknown Record subtypes, package-backed fields, local
procedures, and Record methods; Record-to-Integer assignment, local argument
count/type, and the elementary missing-return form; plus both project rules.

This does not prove complete AL semantic compatibility. The verifier remains
conservative outside the checked forms and leaves unknown non-literal return
expressions unknown instead of inventing a type error. `alc` does not report
the planted missing-return case in this corpus.

Raw evidence:
[`accuracy.json`](results/published/2026-07-26/accuracy.json) and
[`accuracy_al_lsp.stderr.log`](results/published/2026-07-26/accuracy_al_lsp.stderr.log).

## Package emission

Six rounds ran for every project size, with round zero discarded. Every
measured arm succeeded without error diagnostics or a missing artifact, and
all 60 native/`alc` pairs were semantically equivalent.

| Project | Process cold, native / `alc` | Warm unchanged, native / `alc` | One-file edit, native / `alc` |
|---|---:|---:|---:|
| small | 445.833 / 4,812.852 ms | 16.637 / 4,994.220 ms | 18.363 / 4,842.070 ms |
| medium | 454.177 / 4,787.268 ms | 29.532 / 4,733.969 ms | 31.071 / 4,556.994 ms |
| large | 516.013 / 4,951.148 ms | 69.169 / 5,158.778 ms | 68.983 / 4,910.091 ms |
| XL | 648.166 / 5,298.640 ms | 230.612 / 5,884.777 ms | 248.858 / 5,412.046 ms |

Those medians correspond to `alc ÷ native` ratios of 8.175×–10.795× for
process-cold, 25.518×–300.188× for warm-unchanged, and 21.748×–263.686× for
one-file-edit in this synthetic corpus on this machine. They are measurements,
not a general workload promise.

The comparator parses package content rather than claiming whole-archive byte
identity. It requires the exact entry set, JSON equality, manifest equality
apart from build provenance, and navigation equality apart from random
`ControlGUID` values and sibling `ActionDefinition` discovery order. Repeated
`alc` runs proved that ordering nondeterministic; focused tests prove content
changes still fail comparison. Native input parsing, dependency indexing,
semantic verification, emission, artifact verification, and output-write phase
telemetry are preserved in
[`emit.json`](results/published/2026-07-26/emit.json).

The strict `make microsoft-contracts` profile separately covers the
self-contained 19-object-kind differential, dependency and Base Application
bindings/resources, both `pack-native --validate` contracts, the live semantic
bridge, and receiver-sensitive built-in language services.

## LSP lifecycle and latency

`lsp_bench.py` drives both servers through one stdio client, identical source
and cursor positions, and 11 requests per operation (one discarded warmup and
10 measured samples). All measured probes were non-empty and error-free.

| Measure | Native `al-lsp` | Microsoft EditorServices |
|---|---:|---:|
| Initialize | 3.243 ms | 236.496 ms |
| First diagnostic | 1.290 ms (1 diagnostic) | 292.608 ms (clean project event) |
| Cold ready | 2,020.928 ms | 5,646.605 ms |
| Completion | 0.567 ms (94 items) | 10.014 ms (94 items) |
| Hover | 0.137 ms | 0.365 ms |
| Definition | 0.070 ms | 0.241 ms |
| Document symbols | 0.235 ms | 1.273 ms |
| Workspace symbols | 0.473 ms (40 items) | 6.840 ms (40 items) |
| Probe load average | 3.72 | 3.53 |

The native cold-ready gate waits for `semantic analysis complete` before any
probe. Its 1.290 ms first diagnostic is explicitly only phase one; the
2,020.928 ms metric includes bridge initialization and a successful semantic
`CodeAnalysis` call. Microsoft cold-ready includes its real editor lifecycle:
`al/setActiveWorkspace`, `al/projectReady`, active-document setup, and the
first project diagnostic. Definition uses Microsoft's client-facing
`al/gotodefinition` endpoint, which is recorded in the raw result.

Native shutdown completed the request, sent/received the normal exit sequence,
closed stdio, and exited 0 without a kill. Microsoft's shutdown request also
succeeded and the client sent `exit` then EOF, but the host did not terminate
within three seconds. Only that leg permits and records a forced kill; the
result does not mislabel it as a clean process exit.

Raw evidence:
[`lsp_al_medium.json`](results/published/2026-07-26/lsp_al_medium.json),
[`lsp_al_medium.stderr.log`](results/published/2026-07-26/lsp_al_medium.stderr.log),
[`lsp_ms_medium.json`](results/published/2026-07-26/lsp_ms_medium.json), and
[`lsp_ms_medium.stderr.log`](results/published/2026-07-26/lsp_ms_medium.stderr.log).

## Symbol index

The native diagnostic ingested six packages and 11,799 symbols in 692.546 ms
cold. Seven fresh-daemon warm recalls ranged from 462.373 to 499.995 ms with a
483.382 ms median. Six measured runs for each query returned non-empty results:

| Query | Median | Results |
|---|---:|---:|
| `Customer` | 7.387 ms | 20 |
| `Sales Post` | 4.977 ms | 4 |
| `Item Ledger` | 6.114 ms | 17 |
| `Bench` | 2.924 ms | 20 |
| `Gen. Journal` | 11.488 ms | 20 |

Microsoft exposes no equivalent isolated index API, so this section publishes
no comparative ratio. Raw evidence:
[`symbols.json`](results/published/2026-07-26/symbols.json).
