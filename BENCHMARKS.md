# Comparative benchmark status

The repository does not currently publish a native-versus-Microsoft performance ratio.

Earlier measurements were taken before the current native verification pipeline and no longer
describe the work performed by `al-explorer compile` or `pack-native`. They also predate several
correctness changes, including XLIFF package emission and expanded native declaration/binding checks.
Those results must not be used as current speed, accuracy, or output-fidelity claims.

Raw historical results under `benchmarks/results/`, when present, are retained as engineering data.
They are not release benchmarks.

## Current evidence

- Deterministic native micro-benchmarks cover parsing, formatting, interpreter operations, symbol
  indexing, graph construction, completion, impact, and event tracing. See
  [`Docs/benchmarks.md`](Docs/benchmarks.md).
- The live emitter differential test compares parsed `SymbolReference.json` values across a
  self-contained AL object corpus. Focused golden tests additionally cover selected serialized
  symbol shapes and method identifiers.
- The native emitter writes `TextData/*.xliff` and registers the XLIFF content type when the project
  contains translatable text.
- The Microsoft compiler remains the compatibility authority for expression/type semantics and the
  complete analyzer catalogue. Use `pack-native --validate` or `al.useOfficialCompiler=true` when
  that validation is required.

## Requirements for a publishable comparison

A new comparative report must:

1. identify the exact repository commit, Microsoft AL extension/compiler version, hardware, OS, and
   symbol package set;
2. use release builds and identical project inputs for both implementations;
3. measure cold, warm-unchanged, and one-file-edit builds, separating verification from emission;
4. interleave comparable runs or otherwise control machine load;
5. run the current planted-defect corpus and report false positives as well as detections;
6. compare package contents semantically and identify intentional nondeterminism rather than
   claiming whole-package byte identity; and
7. commit the harness configuration and raw results needed to reproduce the report.

The head-to-head harness and setup notes live in [`benchmarks/README.md`](benchmarks/README.md).
Do not add summary ratios to the README or feature comparison pages until the current pipeline has
been measured under these conditions.
