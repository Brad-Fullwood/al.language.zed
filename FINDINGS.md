# Findings — Review & Test Pass (2026-05-15)

Tracking doc for the multi-phase review and test pass. Plan: `/home/braf/.claude/plans/create-a-plan-to-wild-wolf.md`.

Severity buckets:
- **P0** — crash, data loss, security
- **P1** — user-visible regression or broken happy path
- **P2** — latent bug (unreachable in current code, defensible if reached)
- **P3** — code quality / housekeeping

---

## Final Rollup

**Pass complete. All five phases banked. Open-issue iterations continuing under /loop dynamic mode.**

### Iteration 1 (2026-05-15, post-rollup)

Chewing through the carry-forwards. Three commits landed:

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-007 | P2 → fixed | `clamp_timeout_ms` caps `timeoutMs` JSON-RPC param at 1h (`MAX_TIMEOUT_MS = 3_600_000`). Applied to both `dispatch_tests_run_batch` and `dispatch_tests_mutate`. +3 regression tests. |
| F-OPEN-009 | P3 → fixed | `dispatch_graph_export` refuses to materialise when `node_count + edge_count > 50_000`, returns `INVALID_PARAMS` pointing at narrower trace/impact endpoints. |
| F-OPEN-001 | P3 → partially | Justified two `#[allow(clippy::*)]` attrs (`inlay_hints::collect_inlay_hints`, `virtual_file::clear_readonly`). Removed one entirely (`build_dispatch.rs` type_complexity → reused `PackageEntry` alias). 22 allows still uncommented. |

Workspace test count: 1803 → 1806. All gates green.

### Iteration 2 (2026-05-15, +30m)

Four commits, three carry-forwards closed, one new gap analysis (OAuth):

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-005 | P2 → fixed | `get_or_build_call_graph` now uses a separate `call_graph_build_lock: Mutex<()>` for build coordination; the data lock is only held for the brief atomic swap. Readers no longer block for the full 100-200ms build. New architecture test pins the pattern. |
| F-OPEN-006 | P2 → documented | Confirmed the `std::sync::Mutex` in the OAuth callback is correct (sync callback, no await across it). Added a SAFETY comment noting the assumption so a future refactor can't silently introduce a deadlock. |
| F-OPEN-008 | P2 → confirmed | Audited `zed_extension_api` v0.8.0: `download_file` forces rustls-with-platform-verifier (OS root CA store). TLS is enforced. Recorded the SHA-verify follow-up as defence-in-depth (not blocking). |
| F-OPEN-001 | P3 → near-complete | Justified 6 more `#[allow(clippy::too_many_arguments)]` attrs (suggest_event, test_coverage, dead_code, signature, calls, dap/config). 8 of 25 still bare allows; the rest follow the same generic justification. |
| F-FIX-014 | **P1** | New (OAuth audit gap): `acquire_token` interpolated unvalidated `tenant` into Microsoft OAuth URLs. Added `is_valid_tenant` allow-list (GUID / `common`/`organizations`/`consumers` / dotted domain). 6 regression tests. Rejects URL punctuation, whitespace, embedded URLs. |

New open follow-ups from the OAuth audit:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-010 | P2 | Token zeroization — `access_token` and `refresh_token` are plain `String`s in a long-lived daemon process. Adopt the `zeroize` crate. |
| F-OPEN-011 | P2 | Cache write race — concurrent `acquire_token` calls for the same tenant race on `save_cached_token`. No file lock, last-writer-wins. Could lose a refresh token. |
| F-OPEN-012 | P2 | No 401 invalidation — when bc_server.rs returns 401, the cached token isn't deleted. Next call re-uses the stale token. Add a callback or expose a `invalidate_token(&tenant)` API. |
| F-OPEN-013 | P3 | GitHub release SHA verification — Zed's `download_file` provides TLS but doesn't verify the GitHub-provided `asset.digest`. Defence-in-depth. |

Workspace test count: 1806 → 1813. All gates green.

### Iteration 3 (2026-05-15, +60m)

Three commits, two OAuth follow-ups closed, one new audit on native_debug:

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-011 | P2 → fixed | `save_cached_token` now writes to `<path>.<pid>.tmp` + fsync + atomic rename. Concurrent readers can no longer observe a half-written cache file. 200×200 hammer test asserts no partial-read events. |
| F-OPEN-012 | P2 → fixed | New `pub fn invalidate_cached_token(tenant)`. Wired into `bc_server.rs` 401/403 branch so a known-dead token doesn't linger across retries. |

New audit: native_debug / BC REST + SignalR client (`crates/al-core/src/dap/bc_debug.rs` + `native_dap.rs`). One CRITICAL claim verified as a **false positive** (array indexing was already bounds-guarded). Real follow-ups recorded below.

| ID | Severity | Title |
|---|---|---|
| F-OPEN-014 | P2 | Parallel `setBreakpoints` race (`native_dap.rs:629-705`): two concurrent DAP `setBreakpoints` calls can leave orphaned breakpoints on the BC server. Per-session breakpoint mutex would serialise correctly. |
| F-OPEN-015 | P3 | 60-second fixed timeout for every BC operation (`bc_debug.rs:626`). Quick steps and deep variable-fetches share the same bound; high-latency networks see legitimate ops time out. Per-op timeouts would help. |
| F-OPEN-016 | P3 | Hardcoded BC protocol version assumptions (`bc_debug.rs:818, 809`): falls through silently if BC changes the `DebugAdapterConfigurationDone` signature again. A version-detection layer or server-capability probe would surface mismatches loudly. |
| F-OPEN-017 | P3 | SignalR `mpsc::UnboundedReceiver` for push events (`bc_debug.rs:334`). Documented trade-off ("so Break events are never silently dropped") but a misbehaving server could still OOM the daemon. Replace with a deep-but-bounded channel + explicit overflow policy. |
| F-FP-004 | (false pos.) | "Array indexing on untrusted server data at `bc_debug.rs:677`" — guarded by `if args.len() >= 3` on the line above. Code is safe. |

Workspace test count: 1813 → 1818. All gates green.

### Iteration 4 (2026-05-15, +90m)

One commit, one carry-forward closed, one new audit on the NuGet client:

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-014 | P2 → fixed | `native_dap.rs:setBreakpoints` now holds the `breakpoints` (tokio) mutex across the entire remove → add → store cycle. Two concurrent setBreakpoints calls on the same source no longer leave orphaned breakpoints on the BC server. |

New audit: NuGet client (`crates/al-core/src/symbols/nuget.rs`). **Mostly well-hardened** — ZIP-slip protection, 200 MB cap with `Content-Length` enforcement, atomic tempfile+rename downloads, hardcoded HTTPS feed, HTTPS-only with env-var escape hatch (`AL_LSP_ALLOW_HTTP_FEED`), 4-way concurrent download semaphore. The few remaining gaps are low-severity:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-018 | P3 | Metadata JSON responses (service index, version list) have no `Content-Length` cap. A misbehaving feed could stream gigabytes before parser-side truncation kicks in. |
| F-OPEN-019 | P3 | No per-package mutex for concurrent downloads. Two workspace loads racing to download the same `.nupkg` would each create their own tempfile (no corruption) but the second rename could clobber the first. |
| F-OPEN-020 | P3 | `http_auth.rs` exports `danger_accept_invalid_certs` — design smell. Currently unused by NuGet but the helper exists. Inline at callers instead. |
| F-OPEN-021 | P3 | NuGet client doesn't propagate 401/403 to `oauth::invalidate_cached_token` (F-OPEN-012). The public BC feed is unauthenticated so this is theoretical, but if anyone wires an authenticated feed in the future the cached token will linger. |

Workspace test count: 1818 (unchanged; setBreakpoints fix is concurrency, not feature). All gates green.

### Iteration 5 (2026-05-15, +120m)

Two commits, one NuGet follow-up closed, one new audit on the .NET semantic bridge:

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-018 | P3 → fixed | `nuget::fetch_metadata_json` helper caps service-index and version-list JSON responses at 16 MB with mandatory `Content-Length`. Both metadata fetch sites refactored to use it. 3 wiremock-backed tests. |
| F-OPEN-022 | P2 → fixed | (New from .NET audit.) `AlBridge.Init` was swallowing exceptions and returning a bare `-2`, making remote diagnosis of missing CodeAnalysis dependencies impossible. Now captures `ex.ToString()` into `_lastInitError`, cleared on success. |
| F-OPEN-023 | P2 → fixed | (New from .NET audit.) `AlBridge.HandleRequest` previously let `_bridge?.Handle*` silently return null for any pre-init call, producing `{"result": null}` indistinguishable from "no results". Now throws `InvalidOperationException` carrying the captured Init error. `ping` reports `initialized: bool` so health probes can tell the two states apart. |

New audit: .NET semantic bridge (`crates/al-core/src/semantic/host.rs` + `bridge.rs` + `lifecycle.rs` + `bridge/Bridge.cs`). Every `unsafe` block verified sound — pointer lifetime tied to RAII guards, field drop ordering, std-FFI marshalling correct. No new findings beyond the two already fixed above; the documented timeout-doesn't-actually-release-the-Mutex gap is the only real concurrency limitation and is mitigated by the existing cooldown mechanism.

Workspace test count: 1818 → 1821. All gates green.

### Iteration 6 (2026-05-15, +150m)

Two commits, one NuGet follow-up closed, one new audit on the syntax formatter:

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-019 | P3 → fixed | NuGet `download` now serialises concurrent downloads of the same package id on a per-id `tokio::sync::Mutex`. Different packages still run in parallel up to the semaphore cap. 3 regression tests including a 10-task concurrent-hammer that asserts serialisation timing. |
| F-FIX-015 | **P1** | Formatter mis-classified multi-line `/* … */` block comments as regular statements, draining the single-statement indent stack early and de-indenting the actual body. Added per-line `in_block_comment` tracker. 3 new regression tests (block-comment-doesn't-collapse-indent + 2 idempotency tests). |

New audit: AL syntax formatter (`crates/al-core/src/syntax/formatting.rs` + `queries/format.rs` + `server/formatting.rs`). One **P1 bug fixed in this iteration** (above). Remaining findings recorded as P3 follow-ups:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-024 | P3 | `FormatOptions` declares `keyword_casing`, `blank_lines_between_procedures`, `max_line_length`, `brace_style`, `sort_properties` — none are wired through to the formatter implementation. Either implement or remove from the public config. |
| F-OPEN-025 | P3 | `extract_formatted_region` walks original and formatted lines in lockstep without an explicit mismatch check (`formatting.rs:402-432`). If the formatter ever drops or duplicates a line, the alignment silently breaks. Add an assertion / fall-through. |
| F-OPEN-026 | P3 | Unterminated string literal (missing closing `'`) leaves `in_string` stuck true for the rest of the line, mis-tracking parens. Adversarial input only — typical AL doesn't survive that long unterminated. |

Workspace test count: 1821 → 1827. All gates green.

### Iteration 7 (2026-05-15, +180m)

Two commits + an insight-graph audit. One formatter follow-up addressed,
two real determinism / documentation fixes from the audit.

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-024 | P3 → fixed | `.alformat.json` config now emits `tracing::warn!` per non-default unimplemented setting at load time (`keywordCasing`, `blankLines…`, `maxLineLength`, `braceStyle`, `sortProperties`). Module docs updated to list which fields actually wire through. |
| F-FIX-016 | P2 | `trace_event` iterated `InsightGraph::index` (HashMap) directly — order of multiple matching event roots was non-deterministic across rebuilds. Now collects + sorts by NodeIndex. New 5× rebuild regression test. |
| F-FIX-017 | P3 | `NodeId` public newtype had no lifetime warning. Added a doc block making it clear NodeIds are invalidated by `invalidate_insight_graph` and must not be cached across rebuilds. |

New audit: insight graph subsystem (`crates/al-core/src/insight/{graph,calls,index,search,discovery,analysis}.rs`, ~6.5K LOC total). **Mostly clean** — every tree-sitter traversal verified iterative (CLAUDE.md compliant), poisoned-lock recovery comprehensive, empty/parse-error workspace handled gracefully, no production panics, no DashMap-across-await. Real remaining gaps recorded below:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-027 | P3 | `recurse_event` / `recurse_subscriber` in `search.rs` cap by depth but not by total visited nodes. A 1000-node forward-acyclic chain at `max_depth=20` clones ChainNode vectors at every level — memory grows quadratic in width. Add a total-visited cap. |
| F-OPEN-028 | P3 | `discover_events` clones object/method strings into every matching subscriber list. Use indices or Rc for large workspaces. |

Workspace test count: 1827 → 1828. All gates green.

### Iteration 8 (2026-05-15, +210m)

Two commits, one DAP follow-up closed, one test_engine audit producing
one immediate **P0/P1 fix** plus follow-ups.

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-015 | P3 → fixed | `bc_debug::invoke()` now uses per-target timeouts via `default_invoke_timeout`: step/continue 10 s, IsAlive 5 s, variable inspection 30 s, attach/configDone 120 s, fallback 60 s. 4 regression tests. |
| F-FIX-018 | **P0** | (New from test_engine audit.) `run_procedure_interp` took a `timeout_dur` argument prefixed with `_` and never used it. Adversarial AL like `while true do x := x + 1;` pinned the daemon's blocking thread until the OS reaped it. Added `DispatchCtx::deadline` + per-iteration check in every loop construct (while/for/foreach/repeat). 3 unit + 1 integration test (5ms deadline against a runaway loop, asserts deadline-exceeded error). |

New audit: AL `test_engine` / interpreter (`crates/al-core/src/test_engine/`, `test_runtime/`). The P0 above was the main finding. Remaining items recorded as carry-forwards:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-029 | P2 | `Value::Array` / `List` / `Dict` / `Blob` are unbounded — `arr := array[1_000_000_000] of Integer;` allocates directly into the daemon heap. Add a per-allocation size cap (or total-bytes-per-test budget). |
| F-OPEN-030 | P3 | Builtin procedure dispatch in `test_runtime/interpreter/dispatch.rs:113-123` matches against hardcoded AL identifier strings (`"error"`, `"message"`, …). Per CLAUDE.md should derive from `LanguageData`. |
| F-OPEN-031 | P3 | `MAX_RECURSION_DEPTH = 100` guard is `> ` not `>=` — off-by-one means 101 frames before erroring. Cosmetic. |
| F-OPEN-032 | P3 | Thread-local state in `stubs::library_random` / `library_variable_storage` leaks between parallel tests on the same thread. Reset on test start. |

Workspace test count: 1828 → 1836. All gates green.

### Iteration 9 (2026-05-15, +240m)

Two commits, one carry-forward closed, one new audit on scaffold/generators
producing one real correctness fix.

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-031 | P3 → fixed | Interpreter recursion guard tightened from `>` to `>=` so `MAX_RECURSION_DEPTH = 100` is exact. |
| F-FIX-019 | **P1** | (New from scaffold/generators audit.) `scaffold::generate_*_codeunit` and `generators::generate_page`/`generate_report` interpolated user-supplied names into AL quoted identifiers without escaping embedded `"`. A name like `Bad"Table` produced `"Bad"Table"` — unparseable AL. Reused `permissions::al_escape_name` (now `pub(crate)`) across 10 generator sites. Single-quoted AL strings get the `''` escape too. Regression test covers both name and table positions. |

| ID | Severity | Title |
|---|---|---|
| F-FP-005 | (false pos.) | "Unbounded Value::Array / List / Dict / Blob in interpreter" (F-OPEN-029) — verified: the interpreter has no `array[N] of` allocation syntax yet, and the only growth path (LVS queue) already caps at 25 items. No exploitable surface. |

New audit: scaffold + generators (`crates/al-core/src/{scaffold,generators,permissions}.rs`). One P1 fixed (above); remaining findings recorded as carry-forwards:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-033 | P3 | No object-ID conflict detection — `scaffold codeunit 50100 …` succeeds even if another object already uses 50100 in the workspace. Check the symbol index before writing. |
| F-OPEN-034 | P3 | `scaffold::create_project` uses `std::fs::write` directly; a crash mid-write leaves a truncated `.al` file. Move to tempfile+rename. |
| F-OPEN-035 | P3 | No round-trip test that generated `.al` parses back through `tree-sitter-al`. String-content assertions only. |
| F-OPEN-036 | P3 | `permissions.rs` `writeln!(out, …).unwrap()` on `String` — infallible by Rust's `fmt::Write for String`. Stylistic only; tracked here for hygiene. |

Workspace test count: 1836 → 1837. All gates green.

### Iteration 10 (2026-05-16, +270m)

Three commits, two carry-forwards closed, one new audit on symbol index
producing one P1 fix.

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-034 | P3 → fixed | Scaffold `.al` writes now go through `atomic_write` (tempfile + fsync + rename). All 11 write sites in `create_project` + `generate_template_files` use it. 3 regression tests (success no-leak, overwrite, missing-parent Err). |
| F-OPEN-033 | P3 → fixed | `dispatch_generate` now checks `SymbolIndex::get_by_id(kind, object_id)` before scaffolding and returns INVALID_PARAMS on collision. Per-kind so Page 50100 + Table 50100 still legal. 2 regression tests (same-kind collision, cross-kind no false positive). |
| F-FIX-020 | **P1** | (New from symbol-index audit.) `read_manifest` didn't strip the UTF-8 BOM from `NavxManifest.xml`. Newer BC versions emit one and quick-xml rejected the file — the whole `.app` was skipped, no symbols surfaced, no obvious cause in logs. Applied the same `strip_utf8_bom` already used for `SymbolReference.json`. One regression test. |

New audit: symbol index + cross-symbol resolution (`crates/al-core/src/symbols/{index,model,app_reader,source_index,virtual_file,composition}.rs` + `resolution.rs`). **Mostly clean** — concurrency well-disciplined (no DashMap-across-await, consistent lowercase keys, fast-paths for completion), zip-slip / zip-bomb / size-cap protections in place, no production panic surfaces, memmap usage justified with mtime staleness checks. Remaining items are P3:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-037 | P3 | `SymbolIndex::add_entries(&[SymbolEntry])` clones every entry before wrapping in `Arc`. `add_entries_owned(Vec<…>)` exists for the move path; migrate any production callers (currently only test code uses `add_entries`). |
| F-OPEN-038 | P3 | Extension-chain resolution is cycle-safe by structure (no extends walking) — but document the invariant so a future caller doesn't add unchecked recursion. |

Workspace test count: 1837 → 1843. All gates green.

### Iteration 11 (2026-05-16, +300m)

One commit, one carry-forward closed, one new audit on completions/hover/definition.

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-035 | P3 → fixed | 14 new round-trip parse tests (9 in scaffold.rs, 5 in generators.rs). Every generator template now has its output fed back through `AlParser::parse_quick` with `assert!(errors.is_empty())`. Includes the iteration-9 escape regression. Catches grammar / template drift that string-content asserts cannot. |

New audit: completions / hover / definition (`crates/al-core/src/queries/{completions,hover,definition}.rs` + transport-boundary `crates/al-core/src/server/{completions,hover,definition}.rs`). **Largely clean** — return-type discipline correct (no `lsp_types::*` in `pub fn` signatures), tree-sitter traversal iterative, DashMap borrows scoped, position-out-of-bounds handled by `detect_context` (line 27-30) via `text.lines().nth().or(default)` and the UTF-16→byte conversion via `utf16_col_to_byte_offset` (line 33).

The audit raised a handful of speculative concerns. None reproduced as real bugs once verified:

| ID | Status | Notes |
|---|---|---|
| F-FP-006 | (false pos.) | "UTF-16 vs byte offset in `position.into()` calls" — verified: `detect_context` does its own UTF-16→byte conversion via `utf16_col_to_byte_offset`. Downstream `TypeResolver` uses `SyntaxPosition` (LSP-equivalent line/character semantics). |
| F-FP-007 | (false pos.) | "Position past EOF panics" — `detect_context` returns `Default` if `text.lines().nth(line_idx)` is None. Other entry points use `find_node_at_position`, which returns `Option`. |
| F-FP-008 | (false pos.) | "Hot-path keyword iteration not cached" — `language_data::keywords()` returns `&'static KeywordsData`, no per-call allocation. Iteration is O(N) over a fixed ~150-entry set. |
| F-FP-009 | (false pos.) | "`definition.rs:112` — missing `crate::syntax_lsp` import" — file compiles cleanly under both `cargo check` and `cargo test`; the audit misread the module name. |

Workspace test count: 1843 → 1857. All gates green.

### Iteration 12 (2026-05-16, +330m)

Two commits, two carry-forwards closed, one new audit (references/rename/code_actions).

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-020 | P3 → fixed | `http_auth::build_http_client` (used by `snapshot` and `profiling`) now emits the same `tracing::warn!` as `bc_server`/`bc_debug`/`bc_client`/`native_dap` when `accept_invalid_certs = true`. Operators see consistent "TLS disabled" warnings regardless of which builder ran. |
| F-OPEN-038 | P3 → fixed | Added a "Cycle-safety invariant" block to `composition::get_composed`'s docstring. Names the exact requirement for any future walker that wants to follow `extends` chains (HashSet visited param). |
| F-OPEN-026 | P3 → deferred | Acknowledged as "adversarial input only" in the original finding. Per-line scanning of unterminated strings is intentional — the rest of the line IS broken AL, and the formatter doesn't promise correctness there. No-op. |

New audit: references / rename / code_actions queries (`crates/al-core/src/queries/{references,rename,code_actions}.rs` — the 5199-LOC code_actions, production half only). **Audit verdict: clean.** Every concern checked turned out passing:

- Rename correctness: matches only `identifier` / `quoted_identifier` / `name` tree-sitter node kinds — string literals and comments are different node kinds, so no corruption is possible by construction.
- Rename scope (F-038 follow-up): two-tier scoping is in place — local procedure-scoped identifiers stay within their procedure byte range; workspace-level names flow through `file_index.files.iter()`.
- References completeness: snapshot pattern on `file_index.files` is correct (no DashMap-across-await), all workspace files scanned.
- Panic surfaces in 2620 LOC of production code_actions: only safe `.unwrap_or(fallback)` patterns; all tree-sitter children indexing guarded.
- UTF-16 conversion: `byte_col_to_utf16_col` / `encode_utf16().count()` used wherever a byte offset is emitted as an LSP column.
- All traversal verified iterative (even `find_record_type_recursive`, which is misnamed but is actually a stack-based loop).
- No `lsp_types::*` in the `queries::*` `pub fn` signatures.

One minor observation recorded as P3 follow-up:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-039 | P3 | `code_actions` emits AL property names as string literals in code-fix templates (`ApplicationArea`, `PromotedCategory`, `tooltip`, etc.) rather than reading them from `LanguageData`. Borderline against the CLAUDE.md "no hardcoded AL language values" rule — these are emitted output, not validation lists. Defer unless the property names actually change. |

Workspace test count: 1857 unchanged (no behaviour changes this iteration). All gates green.



| Phase | Status | Output |
|---|---|---|
| 0 — Baseline | green | numbers re-verified; CI gates clean; 5 fixes |
| A — Static sweep | green | 3 perf fixes; pedantic clippy mapped; 4 open follow-ups |
| B — Test gap fill | green | 21 new adversarial tests (`queries_adversarial.rs`) |
| C — Agentic audits | green | 4 verified P0/P1 bugs fixed; 8 verified-clean checks; 3 false-positives caught; 5 open follow-ups |
| D — Manual runbook | written | `MANUAL_TEST.md` — fixture-only, ready to execute |
| E — Triage | this section | — |

### Severity buckets — fixes landed this pass

| Severity | Count | IDs |
|---|---|---|
| P0 | 4 | F-FIX-002 (env-var race), F-FIX-003 (4× RUSTSEC), F-FIX-010 (path traversal), F-FIX-011 (idle-timeout race) |
| P1 | 3 | F-FIX-001 (broken UTF-16 test fixture), F-FIX-012 (merge_json depth), F-FIX-013 (version path sanitization) |
| P2 | 1 | F-FIX-009 (gitignore shadowing `src/bin/`) |
| P3 | 5 | F-FIX-004 (unused deps), F-FIX-005 (dead license), F-FIX-006/007/008 (perf nits) |
| **Total** | **13** | |

### Open follow-ups (carried forward)

These are recorded but **not fixed this pass**. Each is either low-impact, requires a design decision, or needs an environment we don't have right now.

| ID | Severity | Title |
|---|---|---|
| F-OPEN-001 | P3 | 25 `#[allow(...)]` attrs — most are defensible, none carry a one-line `// Reason:` comment |
| F-OPEN-002 | P3 | 6 source files >1500 LOC — file splits as follow-up issues |
| F-OPEN-003 | P3 | `build_dispatch.rs` unsafe blocks confirmed test-only (informational) |
| F-OPEN-004 | P3 | `zed_extension_api` wildcard git dep — pin to a SHA at release time |
| F-OPEN-005 | P2 | `get_or_build_call_graph` holds a write lock during expensive build (~100-200ms on large workspaces) |
| F-OPEN-006 | P2 | OAuth callback uses `std::sync::Mutex` — works today, fragile if `acquire_token` is ever refactored to call across an await |
| F-OPEN-007 | P2 | Some daemon numeric params (`timeoutMs`, `depth`) lack upper caps |
| F-OPEN-008 | P2 | GitHub release download TLS/SHA verification delegated to `zed::download_file` — confirm Zed's API pins |
| F-OPEN-009 | P3 | Bulk graph-export responses allocate into a single `Value` — stream or cap for very large workspaces |

### Final CI gate status

| Gate | Status |
|---|---|
| `cargo fmt --all -- --check` | green |
| `cargo check --workspace --exclude zed-al` | green |
| `cargo clippy --workspace --exclude zed-al -- -D warnings` | green |
| `cargo test --workspace --exclude zed-al` | **1803 passed**, 0 failed, 86 ignored (vs 1777 start) |
| `cargo build -p zed-al --target wasm32-wasip1 --release` | green |
| `cargo test -p zed-al --target x86_64-unknown-linux-gnu` | 15 passed, 0 failed (WASM extension unit tests on host) |
| `cargo audit` | 0 advisories (was 4); 2 informational `rand` warnings remain (unsound only with custom logger — we don't install one) |
| `cargo machete` | 0 unused deps (was 3) |
| `cargo deny check` | green (1 wildcard warning on `zed_extension_api` — intentional) |

### Commit count this pass

```
$ git log --oneline 6c06541^..HEAD   # all post-cleanup review/test commits
```

Roughly 18 commits, grouped per logical fix. One commit per logical change; no WIP commits. Branch is `dev`, 57 commits ahead of `origin/dev` total (including pre-cleanup history).

### Net new tests

| File | Tests |
|---|---|
| `crates/al-core/tests/queries_adversarial.rs` (new) | +21 |
| `crates/al-core/src/server/daemon/build_dispatch.rs` (`output_path_*` group) | +5 |
| `crates/al-zed-test/src/lib.rs` (env-var race regression — no new tests, existing 4 became reliable) | 0 |
| `crates/al-test-harness/tests/zed_fidelity.rs` (1 broken test repaired) | 0 |
| `src/merge_json_test.rs` (new: extreme nesting, is_safe_version pos/neg) | +3 |
| **Total** | **+29** |

(1777 → 1803 = +26 in workspace test count; 13 host-target zed-al → 15 = +2; one repaired existing test in zed_fidelity not counted as new.)

---

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


