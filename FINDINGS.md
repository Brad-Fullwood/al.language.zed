# Findings — Review & Test Pass (2026-05-15)

Tracking doc for the multi-phase review and test pass. Plan: `/home/braf/.claude/plans/create-a-plan-to-wild-wolf.md`.

Severity buckets:
- **P0** — crash, data loss, security
- **P1** — user-visible regression or broken happy path
- **P2** — latent bug (unreachable in current code, defensible if reached)
- **P3** — code quality / housekeeping

## Phase 0 — Baseline

### CI Gates

| Gate | Status |
|---|---|
| `cargo fmt --all -- --check` | green |
| `cargo check --workspace --exclude zed-al` | green |
| `cargo clippy --workspace --exclude zed-al -- -D warnings` | green |
| `cargo test --workspace --exclude zed-al` | green — 1777 passed, 86 ignored (al-zed-test live + WASM hooks), 0 failed |
| `cargo build -p zed-al --target wasm32-wasip1 --release` | green |
| `cargo deny check` | green (1 warning: wildcard dep `zed_extension_api` from git, intentional) |
| `cargo audit` | green (0 advisories after rustls-webpki bump; 2 informational `rand` unsound-with-custom-logger warnings remain) |
| `cargo machete` | green (0 unused deps) |

### Numbers (production-only, with proper `#[cfg(...test...)]` stripping)

| Metric | Original claim | Verified actual |
|---|---|---|
| `.unwrap()` calls | 574 | 29 |
| `.expect()` calls | (rolled in) | 17 |
| **Total `unwrap+expect`** | **574** | **46** |
| Untested LSP queries | 19 | 10 (low test refs, all have at least 1 inline) |
| `unsafe` in `build_dispatch.rs` | 3 (production) | 0 production / 3 in test mod |

Most of the prior alarm was test code. Real production hotspots:

| File | unwrap / expect | Verdict |
|---|---|---|
| `crates/al-core/src/permissions.rs` | 21 unwrap | All `writeln!(out, …).unwrap()` to a `String` — infallible, idiomatic |
| `crates/al-core/src/syntax/language_data.rs` | 8 expect | Build-time JSON loading, panics here are programmer errors |
| `crates/al-core/src/syntax/parser.rs` | 3 expect | Tree-sitter API contracts |

10 queries with light/inline-only test coverage (no LSP-layer integration):
`breaking_changes`, `bulk_fix`, `dead_code`, `deps`, `impact`, `obsolescence`, `profiler_hints`, `sql_patterns`, `suggest_event`, `upgrade`.

### Phase 0 Fixes (committed)

| ID | Severity | Title |
|---|---|---|
| F-FIX-001 | P1 | `zed_fidelity_diagnostic_range_is_utf16` failed — fixture parsed cleanly via error-recovery and produced 0 diagnostics. Switched to the known-broken `procedure Broken(` (missing closing paren) used in `queries/diagnostics.rs`. |
| F-FIX-002 | P0 | Env-var test race — `al-zed-test::scale_duration_tests` had a comment promising serial execution but no mutex. Parallel test runner flipped values mid-test. Added `static ENV_SERIAL: Mutex<()>` held for the lifetime of every `EnvGuard`. |
| F-FIX-003 | P0 | 4 `rustls-webpki` RUSTSEC advisories (CRL parsing, name constraints — RUSTSEC-2026-0049/0098/0099/0104). Bumped via `cargo update -p rustls-webpki` to 0.103.13. |
| F-FIX-004 | P3 | 3 unused dependencies (`serde` in `zed-al` and `al-test-harness`, `walkdir` in `al-core`) — pulled in but never imported. Removed. |
| F-FIX-005 | P3 | `deny.toml` allowed `Unicode-DFS-2016` license but no crate uses it; `cargo deny` warned `license-not-encountered`. Removed. |

### Carry-forward into Phase A

- `cargo deny` still warns on the `zed_extension_api` git-branch wildcard. Intentional (Zed's extension API evolves) but worth a note in CLAUDE.md / extension.toml — pin to a SHA at release time.
- `cargo audit` flags `rand 0.9.2` as "unsound with a custom logger" twice. Source: `tungstenite` and `proptest`. Both pulled in by `al-core` (websockets for SignalR push events; proptest for fuzz testing). Not exploitable in our usage — we don't install custom global loggers via `rand`. No action.

## Phase A — Static + Automated Sweep

### Pedantic clippy

3396 warnings under `-W clippy::pedantic -W clippy::nursery`, with the cheap-style allow-list (`module_name_repetitions`, `missing_*_doc`, `must_use_candidate`) filtered out.

Distribution:
- 263 `cast_possible_truncation` — almost entirely `usize → u32` (tree-sitter row → LSP `Position.line`) and `u128 → u64` (microsecond timing for `tracing::debug!`). Both safe for our domain. Not chased — would be churn.
- 30 each `must_use_candidate`, `missing_panics_doc`, `missing_errors_doc` — documentation noise. Not chased.
- 14 `uninlined_format_args` — `format!("{}", x)` vs `format!("{x}")`. Stylistic; not chased.
- 11 `map_unwrap_or`, 8 `option_if_let_else` — stylistic; not chased.
- 8 `too_many_lines` — already known from the LOC hotspot list. Recorded for follow-up.

High-signal lints actioned:

| ID | Severity | Fix |
|---|---|---|
| F-FIX-006 | P3 | `dap/native_dap.rs:169` — `.unwrap_or(serde_json::json!({}))` allocated the empty JSON every call. Replaced with `.unwrap_or_else(...)`. |
| F-FIX-007 | P3 | `bin/al-lsp.rs:191` — redundant `.clone()` on a moved-out `AlToolchain`. Removed. |
| F-FIX-008 | P3 | `insight/search.rs::export_dot` — two `s.push_str(&format!(...))` patterns in node/edge emission loops. Replaced with `writeln!` (avoids intermediate allocations). |

### Other Phase A findings

| ID | Severity | Title |
|---|---|---|
| F-FIX-009 | P2 | `.gitignore` had `**/bin/` (intended for .NET build output) which also matched `crates/al-core/src/bin/` — Rust's binary entry-point directory. Required `git add -f` to add files there. Narrowed to `crates/*/{dotnet,bridge}/{obj,bin}/`. |
| F-OPEN-001 | P3 | 25 `#[allow(...)]` attributes in production code (mostly `too_many_arguments`, `deprecated`, `async_fn_in_trait`). Most are legitimate trade-offs; not all carry a justification comment. Follow-up: add one-line `// Reason: …` next to each in a future pass. |
| F-OPEN-002 | P3 | 6 source files >1500 LOC. `code_actions.rs` (5199), `build_dispatch.rs` (3984), `al-explorer/cli/commands/lsp.rs` (2775), `main.rs` (2669), `insight/calls.rs` (1805), `resolution.rs` (1569). All work; flagged for future split into smaller modules. |
| F-OPEN-003 | P3 | The 3 `unsafe` blocks that the prior exploration flagged in `build_dispatch.rs:3326/3465/3623` are all inside `#[cfg(test)] mod tests`. No production `unsafe` to audit there. |
| F-OPEN-004 | P3 | `zed_extension_api` is pulled in by branch (`branch = "main"`) — `cargo deny` correctly flags this as a wildcard dependency. Intentional during development; pin to a SHA at release time. |

## Phase B — Targeted Test Gap Fill

The plan's "10 lightly-tested queries" finding was based on counting external test-file references; recounting per-query showed each of those 10 actually has 7-47 inline unit tests covering happy paths. The real gap was **negative / degenerate / malformed input coverage** — a future refactor could quietly delete an early-return guard and the inline tests would still pass.

Added `crates/al-core/tests/queries_adversarial.rs` — **21 tests** exercising every one of the 10 queries with at least one degenerate input:
- empty workspace
- workspace with a parse-error file
- nonexistent symbol / empty query
- malformed / missing-fields JSON
- empty baseline vs empty current
- non-existent and empty filesystem paths

No new bugs surfaced — every query already handled these cases gracefully — but the contract is now pinned to commits, not memory. Workspace test count: 1777 → 1798.

## Phase C — Agentic Audits

Five parallel one-shot audits (Concurrency, WASM Security, Daemon Protocol Robustness, Dep Direction, Panic Surface). Findings verified by direct file reads before fixing — three of the agent claims were false positives.

### Fixed (4 commits)

| ID | Severity | Title |
|---|---|---|
| F-FIX-010 | **P0** | `dispatch_tests_run_batch` accepted user-controlled `junitOut`/`coberturaOut` paths and wrote XML wherever they pointed (`build_dispatch.rs:2264-2271`). Added `resolve_output_path_within_project` helper + 5 tests. JSON-RPC INVALID_PARAMS on out-of-bounds. |
| F-FIX-011 | **P0** | Daemon idle-timeout race (`daemon/mod.rs:165-191`): 60s-poll could fire between `accept()` and the spawned task's first dispatch, shutting down a daemon that just got a fresh connection. Bumped `last_activity` immediately on accept. |
| F-FIX-012 | P1 | WASM `merge_json` recursive without depth limit (`src/lib.rs:27-42`). Pathological user settings (deeply nested JSON) could stack-overflow the WASM extension. Added depth cap of 64 + regression test at 200 levels. |
| F-FIX-013 | P1 | WASM GitHub release `version` interpolated into install path without validation (`src/lib.rs:127`). Compromised release tag could escape work dir (`al-lsp-../../tmp/evil/`). Added `is_safe_version` allow-list + 2 tests. |

### Verified clean (no fix needed)

| ID | Topic |
|---|---|
| F-CHECK-001 | Dep-direction audit: **PASS**. All `Cargo.toml`s respect the rules. No `lsp_types::*` in `al_core::queries::*` `pub fn` signatures; conversion helpers live in `queries/mod.rs` and the boundary is `crates/al-core/src/server/lsp.rs`. |
| F-CHECK-002 | DashMap-across-await: clean. (Confirmed by both the earlier exploration and this audit.) |
| F-CHECK-003 | tower-lsp lock poisoning: `.unwrap_or_else(|e| e.into_inner())` pattern is applied consistently. |
| F-CHECK-004 | Send/Sync bounds: no `Rc`/`RefCell`/raw-pointer leaks through tokio tasks. |
| F-CHECK-005 | All `tokio::spawn` tasks retain a `JoinHandle` (`AlServer` holds `diag_task`, `init_task`, `reindex_task`; connection tasks hold a semaphore permit). |
| F-CHECK-006 | Daemon socket is created mode 0600 in `$XDG_RUNTIME_DIR`. |
| F-CHECK-007 | Bounded line reads (`read_bounded_line` 64 MB cap) used on both ends; JSON-RPC parse errors return `id=null, code=-32700` instead of crashing. |
| F-CHECK-008 | The 3 `unsafe` blocks the original exploration flagged in `build_dispatch.rs` are all inside `#[cfg(test)] mod tests` — no production `unsafe` in daemon code. |

### False positives caught

| ID | Where | Why it isn't a bug |
|---|---|---|
| F-FP-001 | `resolution.rs:720, 741, 642` byte-slicing (Panic-Surface audit flagged as "HIGH — panics on malformed XML") | All searches are for ASCII delimiters (`>`, `"`), which always sit at UTF-8 char boundaries. `find()` returns `Option<usize>`; the `?` operator handles missing delimiters. There is an explicit `if content_start > end_pos { return None }` guard before the slice. Code is correct. |
| F-FP-002 | DAP `server`/`browser` argument injection (WASM audit) | `zed::Command` does not invoke a shell, so semicolons/pipes are passed as literal characters to `al-lsp --dap …`, not interpreted. The al-lsp DAP parser also treats them as opaque values. No shell ever sees them. |
| F-FP-003 | `parse_profile` accepting `[]` | The function correctly requires a `{"nodes": []}` object shape — `[]` returning `Err` is the documented contract. Test fixture in the adversarial-tests file was wrong, not the implementation. |

### Carried forward (medium severity, not fixed this pass)

| ID | Severity | Title |
|---|---|---|
| F-OPEN-005 | P2 | `workspace::get_or_build_call_graph` holds a write lock for the duration of an expensive build (`block_in_place`, 100-200ms on large workspaces). Concurrent readers are blocked. Refactor to build outside the lock, then store. |
| F-OPEN-006 | P2 | `std::sync::Mutex` used to collect messages from an OAuth callback (`build_dispatch.rs:935-944`). Currently safe because the callback is synchronous, but a future refactor of `acquire_token` to call the callback from across an await point would deadlock. Document or migrate to `tokio::sync::Mutex`. |
| F-OPEN-007 | P2 | Some numeric request params (`timeoutMs`, `depth`) are not capped at the daemon boundary. Cap them to sane bounds. |
| F-OPEN-008 | P2 | TLS / SHA verification on the GitHub release download is delegated to `zed::download_file`. Verify that Zed's API itself pins TLS / verifies; if not, add a hash check. |
| F-OPEN-009 | P3 | Bulk graph-export responses (`graph`, `deadcode`) serialize into a single `serde_json::Value` before writing — a 100K-symbol workspace could allocate 100MB+ here. Stream or cap. |


