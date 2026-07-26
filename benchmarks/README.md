# Benchmark harness

Reproducible head-to-head measurements against Microsoft's official AL
toolchain — the same `alc` and `Microsoft.Dynamics.Nav.EditorServices.Host`
binaries that ship inside the `ms-dynamics-smb.al` extension used by VS Code
and Cursor. Results are written to `results/` and summarised in
[`../BENCHMARKS.md`](../BENCHMARKS.md).

## Running

```bash
bash scripts/run_sweep.sh          # everything, serialised
python3 scripts/emit_bench.py      # .app emit: pack-native vs alc
python3 scripts/accuracy_bench.py  # planted-defect detection rates
python3 scripts/symbol_bench.py    # symbol index cold ingest vs warm recall
python3 scripts/realworld_bench.py # private production corpus
bash scripts/lsp_headtohead.sh     # al-lsp vs MS EditorServices, one client
```

Requires the release binaries and explicit paths to the external toolchain and
benchmark inputs:

```bash
cargo build --release -p al-lsp -p al-explorer --features al-lsp/semantic
cargo build --release -p al-compile --example build_bench
export AL_MS_EXT=/path/to/ms-dynamics-smb.al-version
export AL_TOOL_PATH="$AL_MS_EXT/bin/linux"
# Keep this immutable source outside benchmarks/projects: emit_bench.py replaces
# each generated project's .alpackages directory while staging the pinned set.
export AL_BENCH_PACKAGES=/path/to/a/coherent/pinned-packages
# Optional emit controls:
export AL_BENCH_CPUSET=8-11
export AL_BENCH_ROUNDS=6
export AL_BENCH_EMIT_RESULT="$PWD/benchmarks/results/emit.json"
# Optional real-world tier:
export AL_BENCH_REAL_ROOT=/path/to/private/corpus
export AL_BENCH_PROJECTS=ProjectA,ProjectB
export AL_BENCH_CORPUS_NAME='private production corpus'
```

## What each script measures

| Script | Measures | Comparable to MS? |
|---|---|---|
| `emit_bench.py` | `.app` build wall-clock in process-cold, warm-unchanged, and one-file-edit states; native phase telemetry; package-semantic equivalence | yes — `alc` total wall time |
| `realworld_bench.py` | same, on a production customer solution | yes — `alc` |
| `accuracy_bench.py` | which of 14 planted defects each toolchain reports | yes — `alc` |
| `lsp_bench.py` / `lsp_headtohead.sh` | LSP request latency, cold-ready time | yes — EditorServices |
| `symbol_bench.py` | symbol index cold ingest, warm recall, fuzzy search | **no** — native-only diagnostic; Microsoft exposes no equivalent isolated index API |
| `gen_projects.py` | builds the synthetic corpus | n/a |
| `gen_accuracy_corpus.py` | builds the planted-defect corpus | n/a |

## Methodology

**Release builds only.** Microsoft ships optimised .NET binaries; a Rust debug
build would understate us by an order of magnitude and is not a meaningful
comparison in either direction.

**Interleaved arms.** `emit_bench.py` alternates backend order by round inside
each scenario. A fresh driver process is used per measured round; its first
build is the `processCold` state, followed by `warmUnchanged` and
`oneFileEdit`. `processCold` resets all in-process state and package indexes,
but deliberately does not claim that the OS page cache was flushed. Optional
`AL_BENCH_CPUSET` pinning narrows scheduler noise. Every sample records the
1-minute load average so the committed result exposes workstation load rather
than hiding it. `realworld_bench.py` also alternates its two tools inside each
round.

**Warmup discarded.** Round 0 of every arm is thrown away; statistics come from
the remaining rounds. Medians are reported, not means.

**Validity and equivalence.** The emit result is publishable only when every
measured native and Microsoft build succeeds without error diagnostics,
produces an artifact, and the paired artifacts are semantically equivalent.
The comparison requires the exact archive entry set, parsed JSON equality,
manifest equality apart from build provenance, and navigation equality apart
from the random `ControlGUID` and sibling `ActionDefinition` discovery order.
The latter is normalized because repeated `alc` builds of identical input emit
those sibling actions in different orders; all action attributes and non-action
element ordering remain significant. A failed or non-equivalent arm invalidates
the entire report instead of becoming a misleading fast sample.

**Comparable timings.** Both arms report total wall-clock time. Native results
also report input parsing, dependency indexing, semantic verification, package
emission, artifact verification, and output-write telemetry. Microsoft `alc`
does not expose equivalent phases, so its total is never split or compared to
one native phase.

**Standardised symbols (synthetic tier).** Every synthetic project's
`.alpackages` is rebuilt to one coherent BC 28.1 set. The customer folder ships
a mix of 27.0 and 28.1 packages, which roughly doubles symbol-load cost for
both tools and adds variance. The real-world tier deliberately keeps each
project’s own packages, because production projects depend on third-party
symbols the Microsoft-only set does not contain. Both the emitter and symbol
benchmarks stage the exact package set themselves from `AL_BENCH_PACKAGES`;
they reject an absent, incomplete, or generated-project-local source before
replacing any staged directory.

**One client, both servers.** `lsp_bench.py` is server-agnostic and drives
al-lsp and Microsoft's EditorServices host over the same stdio JSON-RPC code
path, with the same cursor positions and iteration count. Comparing two
different harnesses would measure the harnesses. Servers needing extra
handshake steps declare them via `--pre-requests` (Microsoft's host will not
load a project until it receives `al/setActiveWorkspace`), and that time is
counted toward cold-ready rather than hidden.

## Customer data

`realworld_bench.py` reads the tree selected by `AL_BENCH_REAL_ROOT`. That tree is
**read-only input**: projects are copied into `work/` before anything runs, and
only aggregate timings, file counts and line counts are ever recorded. No
customer source appears in `results/` or in `BENCHMARKS.md`.

`work/` and `projects/` are generated. Scratch results are ignored; explicitly
published, dated raw result directories are versioned so every quantitative
claim remains reproducible.
