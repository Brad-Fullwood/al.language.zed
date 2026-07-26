# Comparative benchmark status

The repository does not publish a native-versus-Microsoft performance ratio.
No current timing result is presented as a release benchmark.

## Fresh correctness evidence

The current release-binary planted-defect run is recorded in
[`benchmarks/FINDINGS.md`](benchmarks/FINDINGS.md). It uses `alc` 17.0.34.45391
from `ms-dynamics-smb.al` 17.0.2273547 and scores exact-file, ±2-line findings
across 14 isolated defect projects plus a clean control.

The harness reports separate results for:

- Microsoft `alc`;
- LSP `publishDiagnostics`;
- the production native verifier via `al-explorer pack-native --json`; and
- the advisory `nativeCheck` query.

In the current planted corpus, the native build verifier catches all 14
defects with no clean-control diagnostic. This is a bounded corpus result, not
a claim of general Microsoft compiler, analyzer, or path-sensitive control-flow
equivalence. The clean control is required to stay diagnostic-free in every
local arm.

Focused env-gated emitter differentials cover supported package fixtures. They
exercise archive structure, `SymbolReference.json`, manifest normalization,
and the currently covered XLIFF/navigation/resource shapes. They do not
establish general package or Business Central runtime parity; see the emitter
documentation for the remaining compatibility boundary.

The current provisional six-round emitter matrix also passed semantic
comparison for all 60 measured native/`alc` pairs across small, medium, large,
and XL generated projects. Its repository metadata records a dirty worktree, so
the timing samples and speed ratios are deliberately not published. See
[`benchmarks/FINDINGS.md`](benchmarks/FINDINGS.md) for the correctness evidence
and normalization contract.

## Requirements for a publishable performance comparison

A new comparative report must:

1. identify the exact repository commit, Microsoft AL extension/compiler version, hardware, OS, and symbol package set;
2. use release builds and identical project inputs for both implementations;
3. measure process/package-index-cold, warm-unchanged, and one-file-edit builds, state the OS
   page-cache policy, publish native phase telemetry separately, and treat Microsoft `alc` as an
   opaque total because it exposes no phase-equivalent timings;
4. interleave comparable runs or otherwise control machine load;
5. run the current planted-defect corpus and report false positives as well as detections;
6. compare package contents semantically and identify intentional nondeterminism rather than claiming whole-package byte identity; and
7. commit the harness configuration and raw results needed to reproduce the report.

The head-to-head harness and setup notes live in [`benchmarks/README.md`](benchmarks/README.md).
