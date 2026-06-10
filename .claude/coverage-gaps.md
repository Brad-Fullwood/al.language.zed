# Coverage-based red/green gaps (updated 2026-06-04)

`scripts/coverage.sh` (cargo-llvm-cov + nextest) over the whole native workspace.

## Status (2026-06-04)

Whole-workspace line coverage **~77%** (up from 68% on 2026-06-02). This session
added ~600 verified red-green tests (each proven to FAIL when the code-under-test
is deliberately broken) across these files, worst-first:

- native_debug 29→56, native_dap 30→42, semantic/host 30→54, signature 38→79
- daemon: build_dispatch 36→59, lsp_dispatch 26→73, debug_dispatch 32→74,
  insight_dispatch 49→96, mod 32→79
- server/workspace 23→61, oauth 50→75, syntax/symbols 61→89, resolution 66→78
- dap/bc_debug 48→62, dap_mode 44→62
- queries: hover 52→75, mod 50→95, completions 60→74, implementation 45→93,
  inlay_hints 73→85, source 72→86
- symbols: nuget 61→85, bc_server 50→87
- interpreter: eval_stmt 68→79, value 61→98
- profiling 67→99, semantic/bridge 67→73, semantic/lifecycle 78→89,
  diagnostics 56→78, al-protocol/client 78→84, al-test-harness/protocol 29→100

The genuine in-process-testable pure-logic surface is now largely at its ceiling.

## How to read remaining "low" coverage — four categories, only one is a real gap

1. **Transport shim / clap dispatcher (NOT a unit gap, ~6,500 lines):**
   `server/{lsp,handlers,formatting,definition,hover,completions}.rs`,
   `bin/al-lsp.rs`, `al-explorer/src/cli/commands/*`, `al-explorer/src/main.rs`.
   These read ~0% because the e2e harness spawns `al-lsp` as a SUBPROCESS and the
   CLI is exercised manually — the code IS run, just uncounted. See "Subprocess
   coverage" below for the proper (approval-gated) fix.

2. **Live-infra (needs mock harness or live BC/.NET, ~3,800 lines):**
   `dap/native_dap.rs` (live DAP socket event loop), `dap/client.rs`,
   `dap/bc_debug.rs` (live SignalR), `server/daemon/build_dispatch.rs` (spawns
   real ALTool compiles), `native_debug.rs`, `semantic/host.rs` (live .NET CLR).
   Pure helpers in these are tested; the rest needs a fake DAP peer / fake ALTool
   / wiremock BC server — a real harness investment, and the tests would assert
   against the mock, not against BC.

3. **Test-support code (covering tests is pointless, ~550 lines):**
   `test_engine/*`, `test_runtime/interpreter/tests_adversarial_wave2.rs`,
   `test_snapshots/*`, `test_runner.rs`.

4. **Genuine pure-logic gap:** mostly closed this session. Re-run
   `scripts/coverage.sh --uncovered` to find any new ones; feed them to the
   coverage-loop workflow's explicit `{files:[...]}` arg.

## Subprocess coverage (the biggest remaining HONEST % lever — approval-gated)

The server-adapter layer (category 1, ~1,500 lines) is already exercised by 2,977
passing e2e tests; it just isn't COUNTED because the harness spawns a
non-instrumented `al-lsp`. cargo-llvm-cov's two-phase script form runs the tests
fine but its split `report` step can't track an externally-built binary, so it
reports 0%. The robust fix needs a `CARGO_BIN_EXE_al-lsp` build-edge, which only
exists in the crate that owns the bin (al-core):

1. Add `al-test-harness` as a **dev-dependency of al-core** (NEW DEP — needs
   approval per CLAUDE.md; al-test-harness must not depend on al-core, so no cycle).
2. Add `crates/al-core/tests/e2e_subprocess.rs` that sets
   `AL_LSP_BIN = env!("CARGO_BIN_EXE_al-lsp")` (override already honored by
   `find_binary()` as of commit f35e3da) and drives a representative e2e subset.
3. Under `cargo llvm-cov nextest`, cargo then builds the al-lsp bin instrumented,
   cargo-llvm-cov tracks its coverage map, and the spawned bin's profraws merge —
   counting the server adapters legitimately.

Estimated effect: +several TOTAL points, all honest (no new assertions, just
counting tests that already run). Gated on the dev-dependency approval.

## Efficient strategy (tiered by cost)

1. **Coverage** (`scripts/coverage.sh`, ~10 min) — finds untested code. Per PR.
2. **coverage-loop workflow** — pass `{files:[...], ceiling:80}` to aim runs at
   specific sub-ceiling files; each agent writes verified red-green tests, gates,
   commits, pushes.
3. **Mutation on diff** (`cargo mutants --in-diff origin/dev`) — checks the tests
   on CHANGED code actually assert.
