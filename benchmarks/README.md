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
export AL_MS_EXT=/path/to/ms-dynamics-smb.al-version
export AL_BENCH_PACKAGES=/path/to/a/coherent/.alpackages
# Optional real-world tier:
export AL_BENCH_REAL_ROOT=/path/to/private/corpus
export AL_BENCH_PROJECTS=ProjectA,ProjectB
export AL_BENCH_CORPUS_NAME='private production corpus'
```

## What each script measures

| Script | Measures | Comparable to MS? |
|---|---|---|
| `emit_bench.py` | `.app` build wall-clock, synthetic projects at 2/40/200/800 objects | yes — `alc` |
| `realworld_bench.py` | same, on a production customer solution | yes — `alc` |
| `accuracy_bench.py` | which of 14 planted defects each toolchain reports | yes — `alc` |
| `lsp_bench.py` / `lsp_headtohead.sh` | LSP request latency, cold-ready time | yes — EditorServices |
| `symbol_bench.py` | symbol index cold ingest, warm recall, fuzzy search | **no** — recorded as a gap |
| `gen_projects.py` | builds the synthetic corpus | n/a |
| `gen_accuracy_corpus.py` | builds the planted-defect corpus | n/a |

## Methodology

**Release builds only.** Microsoft ships optimised .NET binaries; a Rust debug
build would understate us by an order of magnitude and is not a meaningful
comparison in either direction.

**Interleaved arms.** `emit_bench.py` and `realworld_bench.py` alternate the
two tools inside each round rather than running all of one then all of the
other. This box is a developer workstation, not a dedicated bench rig — load
drifts as editors, VMs and background compiles come and go. Alternating means
any drift is shared by both arms, so the median ratio survives a machine that
is merely *reasonably* quiet. Every sample records the 1-minute load average it
ran under, and `run_sweep.sh` waits for no active `rustc` and load < 5 before
each step.

**Warmup discarded.** Round 0 of every arm is thrown away; statistics come from
the remaining rounds. Medians are reported, not means.

**Validity flag.** A speedup is only meaningful when both arms actually did the
work. Each result carries `valid_*`: false when an arm produced no `.app`.
A tool that exits early with an error is not "fast" — see the emit-only caveat
in `BENCHMARKS.md`.

**Standardised symbols (synthetic tier).** Every synthetic project's
`.alpackages` is rebuilt to one coherent BC 28.1 set. The customer folder ships
a mix of 27.0 and 28.1 packages, which roughly doubles symbol-load cost for
both tools and adds variance. The real-world tier deliberately keeps each
project's own packages, because production projects depend on third-party
symbols the Microsoft-only set does not contain.

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

`work/`, `results/` and `projects/` are generated; see `.gitignore`.
