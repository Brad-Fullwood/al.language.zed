# Coverage-based red/green gaps (2026-06-02)

One `scripts/coverage.sh` run (cargo-llvm-cov + nextest, ~2 min, whole workspace).
**Overall: 68.0% line / 67.1% function coverage** — ~2,201 functions have no test
exercising them in-process.

## How to read this

Two kinds of "0%" — only one is a real gap:

- **Transport-adapter artifact (NOT a gap):** `server/*.rs` (hover, definition,
  formatting, handlers, lsp, completions), `bin/al-lsp.rs`. These show 0% because
  the e2e harness exercises them by spawning `al-lsp` as a SUBPROCESS, whose
  coverage isn't counted in-process. The logic they call (`queries/*`) is covered.
  To measure them, thread `LLVM_PROFILE_FILE` into the spawned binary (future).

- **Genuine gaps (real red/green holes):** production logic with thin/no coverage
  that is NOT just a transport shim. Ranked below — these are what the loop should
  target with fast inline unit tests.

## Genuine gaps, worst first

| Line% | File | Notes |
|---|---|---|
| 0%  | `al-core/src/http_auth.rs` | **0 tests anywhere** — TLS/auth helper, real gap, highest priority |
| 0%  | `queries/semantic_tokens.rs` | e2e-only (10 files); add fast inline tests |
| 0%  | `queries/symbols.rs` | e2e-only (12 files); add fast inline tests |
| 8%  | `symbols/source_index.rs` | thin |
| 16% | `dap/native_dap.rs` | DAP server; hard to unit-test, partly subprocess |
| 20% | `queries/test_coverage.rs` | |
| 21% | `snapshot.rs` | |
| 23% | `dap/config.rs` | parsing — easy wins |
| 26% | `queries/search.rs` | |
| 29% | `native_debug.rs` / `semantic/host.rs` | |
| 32% | `publish.rs` | |
| 33% | `symbols/language_data.rs` | |
| 38% | `queries/signature.rs` | |

al-explorer CLI commands (`cli/commands/*`, `main.rs`) are also ~0% — they're
thin clap dispatchers exercised manually; lower priority.

## Efficient strategy (tiered by cost)

1. **Coverage** (`scripts/coverage.sh`, ~2 min) — finds untested code. Run per PR.
2. **Mutation on diff** (`cargo mutants --in-diff origin/dev`, minutes) — checks
   whether the tests on CHANGED code actually assert. Run in the loop/CI.
3. **Full mutation** (the 1am `al-mutation-sweep.timer`) — exhaustive, overnight.
