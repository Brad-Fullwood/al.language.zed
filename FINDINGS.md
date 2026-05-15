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

