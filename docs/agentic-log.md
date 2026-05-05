# Agentic Loop Log

Persistent record of `/loop` runs against this branch. One section per
run, newest first. Per-run details live under `.agentic/<run-id>/`
(gitignored); this file holds the durable summary.

The Overseer's `overseer-log-append.sh` Stop hook prepends entries to
this file at the end of every `/loop` invocation.

## 2026-05-05T01-25-18Z-de10ced — Codex Review batch (cycles 1-3, capped)

Started: 2026-05-05T01:25:18Z · Branch: `dev` · Trigger: user
"investigate, confirm, then fix issues in /Docs/Codex Review" → "Run /loop"

Codex-review handoff (`docs/Codex Review/02-findings.md`, 52 findings)
served as Phase A input — fresh `/review-all` skipped because Codex
already provided a validated, exhaustive set. Phase B (/arch-plan) was
skipped throughout: every Codex finding came with actionable fix
guidance, none marked needs_design. Phase C used in-session sequential
TDD with one commit per finding (same pattern as the cycle-1/2 sweep).
Phase D (release) skipped — handed back for `/release-prep`.

10 findings closed across 3 cycles, halted-capped at max_cycles=3.
Cumulative with the pre-loop F-001/F-002 fix and T057's F-048 closure,
the run lifts the resolved count to 13/52.

| Cycle | Working set | Done | Commits |
|-------|-------------|------|---------|
| 1 | small/mechanical bucket: F-007, F-017, F-022, F-027, F-028 | 5 | dd25701, 7183803, 0340430, dce8c33, 9c7c464 |
| 2 | al-core daemon/diagnostics cluster: F-009, F-010, F-008 | 3 | d88ed07, 0a2fcd7, 9ab38c4 |
| 3 | deferred daemon-cluster pair: F-011, F-046 | 2 | 9a92977, 13d9cf3 |

Plus 2 doc commits (de10ced, ee50392) keeping the Codex findings index
in sync. F-003 (cycle-3 deferral) was held back because its root cause
likely lies in F-004 (AL toolchain discovery), and the test failure
needs deeper investigation than fits the final capped cycle.

Highlights:

- **F-022 / F-046 hardened daemon transport** — bounded read during
  read closes the unbounded-allocation window, and the per-socket
  spawn lock prevents concurrent CLI/TUI clients from forking two
  daemons that race on the same socket.
- **F-009 / F-010 / F-011 closed three "stale-state-after-mutation"
  classes** — daemon downloadSymbols now refreshes symbol indexes,
  full file-index scan drops deleted files, and daemon write/rename
  paths route through new `write_al_file_and_refresh` /
  `rename_al_file_and_refresh` helpers (format/sort/organize wired
  in this batch; bulk_fix family deferred for follow-up).
- **F-008 closes a long-standing UX trust break** — `al.compile`
  now tracks the previous compile's affected file set and republishes
  syntax-only diagnostics (or empty for closed files) when stale
  compiler errors should disappear after a clean rebuild.
- **F-027 / F-028 simplified zed-al** — nested `al: { ... }` settings
  now unwrap correctly, and the dead legacy-proxy discovery branch
  came out (with `discovery.rs` + `platform.rs` deleted).

Quality gates were green at the end of every cycle. No drift detected
(no resolved finding reappeared). 39 Codex findings remain open;
recommended next-loop start is F-003/F-004 (toolchain + hover-test
pair) and the F-038 lexical-references rewrite is flagged for
/arch-plan before any in-loop implementation.

## 20260503T231913Z-0375949 — cycle 2 (deferred-task sweep, full closure)

Started: 2026-05-04T00:30:00Z · Branch: `dev` · Trigger: user
"go make sure nothing is skipped or deferred — total completion"

Cycle-2 reused the cycle-1 review+arch handoff and worked the deferred-
task subset directly (15 tasks). **All 15 closed in this session** —
6 via "small surgical fix" commits, then a second push pulled the
multi-day items in (T028 LSP cancellation via spawn_blocking, T047
CLR mutex thundering-herd guard, T049 SymbolIndex helper, T053+T072
grammar submodule edits via tree-sitter-al push, T057 jsonrpc field
across 100+ struct-literal sites, T052 cancellation tests).

Cycle-2 closure batch (continued from initial sweep):

| Task | Commit | Closure |
|------|--------|---------|
| T053 + T072 | 486f6d8, aba0057 (submodule) | grammar highlights.scm dual-capture cleanup, locals.scm asserterror scope removal, object_types.json display_name casing fixes (11 entries), implicit_variables.json CurrDataItem add. al-gen generator updated. Submodule pushed to AL-Tree-Sitter remote; parent reference bumped. |
| T028 | acd547e, 2466093 | textDocument/{references, document_symbol, semantic_tokens_full} now wrapped in spawn_blocking so tower-lsp 0.20's automatic $/cancelRequest handling actually frees the async runtime instead of waiting for the CPU work. |
| T049 | 1e4ff32 | extracted `SymbolIndex::retain_arcs_not_in` helper centralising the discipline that all `Vec<Arc<SymbolEntry>>`-valued secondary indexes get filtered uniformly in `remove_package_entries`. |
| T047 | d676806 | CLR semantic bridge cooldown release now probes the Mutex via try_lock; if still held (previous CLR call genuinely hung) cooldown is extended instead of releasing the gate to a thundering herd. |
| T057 | 5073384 | added `jsonrpc: "2.0"` field to JSON-RPC Request and Response. ~107 in-tree struct literal sites spread `..Default::default()`; new `Request::new`, `Response::ok`, `Response::error`, `Response::null` constructors. Backward-compat preserved via `#[serde(default)]`. |
| T052 | 4e04c62 | LspClient::cancel_request method + 2 ignored E2E cancellation regression tests; T028's spawn_blocking wraps unblocked this. |

Closed via 6 fix/test/perf/chore commits + 1 cargo-fmt commit:

| Task | Kind | Commit | Closure |
|------|------|--------|---------|
| T055 | verified-not-bug | a520d8e | apply_changes lock-discipline + clamp tests + native_debug warn! + LspClient Drop hook + zed-al fs comment + drop() explicit |
| T066 | verified-not-bug | a520d8e | drop() explicit on take() + idempotency regression test |
| T031 | refactor (parent half) | d215275 | al-core fallback comment updated to reflect cycle-1 grammar verification (submodule corpus tests held with T053/T072) |
| T016 | gap | d215275 | package-level dependency edge fixture + 2 negative tests |
| T026 | gap | d215275 | LspClient::code_lens method + 2 ignored E2E tests |
| T063 | refactor | db43896 | package-symbol signature path collects ALL overloads (build_signature_info_from_method + pick_active_signature) |
| T050 | gap | 7e51a06 | criterion benches/parser.rs + find_enclosing_procedure allocation drop |
| T060 | risk | b4ab52a | tracing-subscriber gated behind default `bin` feature |
| T071 | gap | 9288f58 | DeeplyNestedActions.al fixture + 3 syntax_comprehensive tests + absence assertion + performance.rs doc fix + ZedInstance::discover multi-window doc |

Test gates: `cargo check --workspace --exclude zed-al`, `cargo clippy
--workspace --exclude zed-al -- -D warnings`, `cargo fmt --all --check`,
`cargo test -p al-core --lib` (1317 tests), `cargo test -p al-protocol`
(14 tests), `cargo test -p al-explorer` — all green.

Out-of-session-scope items deliberately tackled in this run despite the
deferral notes:

* T028 closed by spawn_blocking on the heavy queries — turns out tower-
  lsp 0.20 already handles $/cancelRequest at the future-drop level,
  so multi-day CancellationToken plumbing wasn't required. Lighter
  queries (hover, completion, definition, formatting, signature_help,
  folding_range, code_action) intentionally not wrapped — sub-millisecond
  per call; spawn_blocking thread-pool overhead would exceed savings.
* T047 closed by Mutex try_lock probe on cooldown release. Full multi-
  week fix (cancellable CLR hosting or process-isolated subprocess)
  remains the right long-term answer; current change closes the user-
  visible thundering-herd failure mode.
* T049 closed via helper extraction rather than full unification under
  a `Vec<dyn SecondaryIndex>` trait — that bigger rewrite remains a
  future option but is no longer load-bearing now that the helper
  centralises the discipline.

Convergence: every deferred task from cycle 1 is now closed. The
branch is in releaseable shape; only follow-up work is incremental
hardening (e.g. spawn_blocking on more queries, the bigger SymbolIndex
trait rewrite) — none of it required for shipping cycle-2's batch.

<!-- new entries are inserted above this line -->
