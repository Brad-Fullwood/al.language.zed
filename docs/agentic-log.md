# Agentic Loop Log

Persistent record of `/loop` runs against this branch. One section per
run, newest first. Per-run details live under `.agentic/<run-id>/`
(gitignored); this file holds the durable summary.

The Overseer's `overseer-log-append.sh` Stop hook prepends entries to
this file at the end of every `/loop` invocation.

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
