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

### Iteration 13 (2026-05-16, +360m)

Two commits, two carry-forwards closed, one audit on semantic_tokens / inlay_hints / signature.

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-017 | P3 → fixed | SignalR general event channel switched from unbounded to bounded (4096-cap). On overflow the reader task drops the non-Break message with a `warn!` log. Break events kept on their separate unbounded `bool` channel — losing one would silently stick the debugger, and ~1 MB per 1M entries is acceptable. |
| F-OPEN-027 | P3 → fixed | `MAX_CHAIN_NODES = 10_000` added to `recurse_event` / `recurse_subscriber`. Existing depth cap protected against deep chains; this protects against wide-but-shallow fan-out. Same empty-vec early-return as the depth cap, so no API change. |

New audit: rendering-style LSP queries (`crates/al-core/src/queries/{semantic_tokens,inlay_hints,signature}.rs` + `crates/al-core/src/syntax/tokens.rs`). Two findings flagged as CRITICAL/HIGH by the audit, both **verified as false positives** on direct read:

| ID | Where | Why not a bug |
|---|---|---|
| F-FP-010 | "Multi-line semantic token UTF-16 length uses full line not span" (`syntax/tokens.rs:251`) | `line` is from `current.utf8_text(source).lines()` — i.e. the token's own bytes split by newline, **not** the full source line. So `line.encode_utf16().count()` is exactly the token's span on that row. Verified by hand-walking a 3-line block-comment example. |
| F-FP-011 | "`build_line_starts` allocates per call" (`syntax/tokens.rs:221`) | One O(n) pass producing a ~50K-entry Vec for a 50K-line file is dwarfed by the tree-walk it precedes. Caching across requests would force invalidation on every edit, more complex than the gain. |

One P3 follow-up recorded:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-040 | P3 | `signature::pick_active_signature` falls back to index 0 when no overload has ≥active_param parameters; can highlight the wrong arg at trailing commas. Tighten with documented semantics on the fallback. |

Workspace test count: 1857 unchanged (concurrency / DoS protection, not feature). All gates green.

### Iteration 14 (2026-05-16, +390m)

Three commits, two carry-forwards closed, one new audit on DAP proxy
producing a real **P1** fix.

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-040 | P3 → fixed | `signature::pick_active_signature` now falls back to the **widest** overload (via `max_by_key`) when no overload accommodates `active_param`. Previously fell back to index 0 — the no-arg overload, where the trailing arg slot doesn't exist either. T063's positive-case test still passes; the fallback test was updated to reflect the new contract. |
| F-OPEN-025 | P3 → fixed | `extract_formatted_region` now `debug_assert!`s that `fmt_idx` advanced exactly as often as the non-collapsed-blank orig rows visited. Catches any future formatter rule that drops / duplicates / reorders lines beyond the one documented asymmetry (blank-collapse). Release builds skip the check (no panic), test runs surface the bug. |
| F-FIX-021 | **P1** | (New from DAP-proxy audit.) `AL_DAP_CAPTURE` env-var-gated capture log dumped raw DAP bodies — including `launch` arguments carrying `password` / `accessToken` / `apiKey` / `bearer` — to a file. Developer-shared logs leaked credentials. Added `redact_dap_body_for_log` that walks the JSON tree and replaces matching field values with `<redacted>`. 6 regression tests. |

New audit: DAP framing + EditorServices proxy (`crates/al-core/src/server/dap_mode/`, `crates/al-core/src/dap/{framing,protocol,client,types,config}.rs`). Findings:

- Framing parser correctly bounds Content-Length (≤20 MB) and headers (≤8 KiB) — verified clean.
- Stdout writer lock guards against interleaving between `stdin_to_child` and `child_to_stdout` paths (F-012 already in place).
- Subprocess lifecycle: stderr task pinned, child killed on shutdown; no orphan-process risk.
- Message-ID round-trip Zed → ES → Zed verified.
- Launch-config paths fixed by spec (no path traversal).
- Native DAP cancellation via watch channel works; legacy proxy delegates to EditorServices.Host.

One P3 follow-up:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-041 | P3 | Legacy DAP proxy doesn't implement DAP `cancel` (delegates to EditorServices.Host). Native DAP handles it. Acceptable for legacy adapter — but worth a comment so the cross-mode asymmetry is visible to future maintainers. |

Workspace test count: 1857 → 1864. All gates green.

### Iteration 15 (2026-05-16, +420m)

Three commits, two carry-forwards closed, one new audit on DocumentStore producing one fix + one false-positive verification.

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-032 | P3 → fixed | `LibraryRandom` LCG and `LibraryVariableStorage` queue both live in `thread_local!`. Parallel tests on the same worker thread saw bleed-through (`SetSeed(42)` survived across boundaries). Added `library_random::reset_lcg()` + a single `stubs::reset_thread_local_state()` facade; called from `run_codeunit_interp` before every test method. Regression test primes both pieces of state, resets, asserts queue empty + LCG-deterministic. |
| F-OPEN-041 | P3 → fixed | Added a docstring on `run_dap_proxy` explaining that the legacy DAP path does not implement `cancel` (unlike `native_dap` which uses a watch channel). Subprocess kill on shutdown is the only cancellation primitive. |
| F-FIX-022 | P2 | (New from DocumentStore audit.) `apply_changes` previously silently dropped any TextChange whose range was backward or out-of-bounds. Worst diagnostic shape: client thinks edit landed, server diverges, no log. Restructured into an explicit match with `warn!` + skip on each bad-range branch. 2 regression tests. |

New audit: `DocumentStore` + `parsing::get_or_parse` (`crates/al-core/src/documents.rs` + `parsing.rs`). Verified clean overall:

- DashMap-across-await: not present (all `apply_changes` etc. are sync).
- Lock-ordering: no nested locks.
- No `lsp_types::*` in store types — uses internal `TextRange` / `TextChange`.
- Memmap, panic surfaces: none in production paths (all `.unwrap()` in test code).

One CRITICAL audit claim caught as **false positive**:

| ID | Where | Why not a bug |
|---|---|---|
| F-FP-012 | "TOCTOU race in `get_cached_tree`: read doc.version then trees[uri], not atomic" | Verified: DashMap's `Ref` on `docs[uri]` is a reader-lock on that shard. Concurrent `apply_changes` calls `get_mut(uri)` which needs a writer-lock on the SAME shard, so it blocks until `get_cached_tree`'s Ref drops. The two-step read is internally consistent. |

Two follow-ups recorded:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-042 | P3 | No per-document size cap. A workspace could open a 10 GB file and consume memory unbounded. Add an optional config-driven cap. |
| F-OPEN-043 | P3 | Tree-sitter parse-tree cache has no eviction. Long-running daemon with many opened files accumulates parsed trees forever. LRU or sweep on idle would help. |

Workspace test count: 1864 → 1867. All gates green.

### Iteration 16 (2026-05-16, +450m)

One commit, one audit on the BC REST client surface, two real fixes in five call sites.

New audit: BC server REST client and its peers (`bc_client.rs`, `profiling.rs`, `snapshot.rs`, `test_runner.rs`, `publish.rs`, `http_auth.rs`).

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-023 | P1 | (New.) `bc_client::sanitize_error_body` redacts `Bearer …` / `access_token=…` / `password=…` / etc. and truncates at 512 B. Three sibling modules (`profiling`, `snapshot`, `test_runner`) forwarded raw error bodies into `warn!` logs and RPC error responses — bypass of the existing redaction. Routed all 5 sites through the sanitizer. |
| F-FIX-024 | P3 | (New.) `test_runner.rs::TestRunnerClient::new` constructed an HTTP client with `danger_accept_invalid_certs` from config but did NOT emit the parity `"TLS verification disabled"` warn that the other 4 BC client builders all emit. Operators watching daemon logs now see consistent disclosure regardless of code path. |

The audit also called out two MEDIUM findings that don't justify code change this pass:

- Unbounded JSON response parsing in `profiling.rs:121`, `snapshot.rs:111/148`, `test_runner.rs:144/181` — same surface exists in `bc_client.rs` and was accepted as acceptable given the trusted-server threat model. Add a Content-Length cap as a follow-up if BC server reliability becomes a concern.
- No retry/backoff on transient failures — by design (single-shot endpoints).

Recorded follow-up:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-044 | P3 | Add Content-Length caps on JSON parsing in `profiling`/`snapshot`/`test_runner` for parity with the NuGet metadata cap (F-OPEN-018). Defence-in-depth against a misbehaving BC server. |

Workspace test count: 1867 unchanged (defensive hardening, not feature). All gates green.

### Iteration 17 (2026-05-16, +480m)

Two commits from a combined audit of `xliff.rs` (translation file handling) and `publish.rs` + `bc_client.rs` (.app upload).

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-025 | **P1** | (New from xliff audit.) `parse_xliff::extract_xml_text` truncated `<source>` / `<target>` / `<note>` bodies at the first `<` on the opening line, silently dropping line 2+ of any multi-line translation. Restructured the parser into a multi-line-accumulator state machine that closes on `</tag>` regardless of which line it's on. 3 regression tests (multi-line source, multi-line target, single-line still works). Removed the now-dead `extract_xml_text` helper. |
| F-FIX-026 | P2 | (New from publish audit.) `BcClient::publish_extension` and `rad_publish` slurped the entire `.app` binary into memory via `tokio::fs::read` with no size check. Added `MAX_UPLOADABLE_APP_BYTES = 500 MB` + new error variant `AppFileTooLarge { bytes, limit }` returned BEFORE any read. 2 regression tests including a sparse-file trick for the oversize negative path. |

The audit also raised concerns I marked as **not actionable this pass**:

| ID | Status | Notes |
|---|---|---|
| F-FP-013 | (false pos.) | "XXE / billion-laughs in `parse_xliff`" — verified: the hand-rolled parser only recognises specific named tags (`<trans-unit `, `<source`, `<target`, `<note>`) and ignores `<!DOCTYPE` declarations entirely. `xml_unescape` only knows 5 standard entities, no recursion. Not exploitable. |
| F-OPEN-045 | P3 | `parse_xliff` reads its input via `&str` so a 1 GB `.xlf` consumes 1 GB before parsing. Add a size check at the call sites (`build_xliff`, `refresh_xliff`) — same pattern as `read_app_capped`. |

Workspace test count: 1867 → 1872. All gates green.

### Iteration 18 (2026-05-16, +510m)

Two commits, two carry-forwards closed — both Content-Length / size-cap hardening, parallel to the existing NuGet metadata cap (F-OPEN-018).

| ID | Severity | Resolution |
|---|---|---|
| F-OPEN-044 | P3 → fixed | Five BC dev API JSON parse sites in `profiling`, `snapshot`, `test_runner` ran `resp.json::<T>()` with no bound. Added `bc_client::read_json_body_capped` with the same Content-Length-required + 16 MB cap pattern as NuGet's `fetch_metadata_json`. Routed all 5 sites through it. 3 regression tests. |
| F-OPEN-045 | P3 → fixed | `parse_xliff` consumed `&str` so a 1 GB `.xlf` was loaded into memory before any size check. New `xliff::xlf_exceeds_cap(path)` checks on-disk size against `MAX_XLF_FILE_BYTES = 64 MB`. Three daemon dispatch sites (xlf-refresh, xlf-untranslated, xlf-suggest) now refuse oversize files before reading. 3 regression tests using a sparse-file trick. |

Workspace test count: 1872 → 1878. All gates green.

### Iteration 19 (2026-05-16, +540m)

One commit. Audit of `al-explorer` CLI surface (7K LOC, 77 subcommands) — **verdict: clean**. Dep direction correct (no `al-core` import), no shell injection (no `Command::new` in CLI layer; paths delegated to daemon), TUI has a panic-recovery hook, daemon handshake retries on startup, types-duplication bounded to `ObjectKind` enum (ISSUE-017 contract), edition-2024 used correctly. Two LOW findings recorded but not actioned:

| ID | Severity | Title |
|---|---|---|
| F-OPEN-046 | P3 | TUI daemon-socket reads have no timeout (Ctrl+C is the user's only escape on a stuck query). Per-op timeouts would help but require design (different ops have different latency budgets). |
| F-OPEN-047 | P3 | `crates/al-explorer/src/cli/commands/mod.rs:64` test-only `current_dir().unwrap()` — cosmetic, in a test helper. |

Continued chip on F-OPEN-001: justified 4 more `#[allow(clippy::*)]` attrs (queries/tests.rs `if_same_then_else` on cursor walk, queries/test_coverage.rs same pattern, queries/inlay_hints.rs `lookup_parameter_names` + `lookup_via_receiver`). Eight bare allows remain.

Workspace test count: 1878 unchanged. All gates green.

### Iteration 20 (2026-05-16, +570m)

Two commits, one new audit (al-test-harness library — the E2E test harness itself, 11.7K LOC across `src/` + `tests/`).

Audit context: the harness spawns the real `al-lsp` binary over stdio. Bugs here don't crash users — they produce flaky tests, or worse, silent test passes that don't actually exercise the LSP path under test. So a P1 here is "subtly undermines the regression-detection apparatus", not a user-visible crash.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-031 | P2 | `protocol::semantic_token_data` used `arr.chunks(5)` then indexed `chunk[1..=4]`. A misbehaving server emitting a non-multiple-of-5 `data` array would panic on the trailing partial chunk. Switched to `chunks_exact(5)` (silently drops the malformed tail — LSP spec mandates multiples of 5). 3 regression tests. |
| F-FIX-032 | P1 | `wait_for_diagnostics` swallowed timeouts at WARN level. Tests calling `open_file().await; client.drain_diagnostics();` would silently see an empty map when the 5 s hardcoded cap fired on slow CI. Bumped to ERROR-level logging, made the wait configurable via `AL_TEST_DIAG_TIMEOUT_MS`, and changed the internal return type to `bool` so future tests can opt to fail loudly. |
| F-FIX-033 | P1 | `request()` had a hardcoded 10 s timeout. Under debug-build CI / debugger-attached runs, slow queries (`completion`, `references` on a large workspace, `formatting` on big files) would silently return `None`/`[]` instead of the real response. Now configurable via `AL_TEST_REQUEST_TIMEOUT_MS` (default 10 s). Error message includes the env-var name. |
| F-FIX-034 | P3 | `spawn` unconditionally forced `RUST_LOG=debug` on the child. Now only forces it if the test author hasn't already set `RUST_LOG`. `RUST_LOG=warn cargo test -p al-test-harness` now produces a quiet run. |
| F-FIX-035 | P3 | `scopeguard_remove` doc comment had `id` shell output pasted into it (`uid=1000(braf) gid=1000(braf) groups=…`). Removed. |

False positives caught:

| ID | Where | Why not a bug |
|---|---|---|
| F-FP-014 | "`stderr` inheritance creates backpressure deadlock with `RUST_LOG=debug`" | Audit later acknowledged: `Stdio::inherit()` connects the child's stderr to the parent's stderr fd directly, not via a captured pipe. No kernel backpressure path through the harness. |
| F-FP-015 | "`read_loop` `header_buf` not cleared on `continue`" | Audit self-withdrew on re-read; `header_buf.clear()` at the top of the inner loop covers every iteration. |

Carry-forwards (not fixed this iteration):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-048 | P2 | `mpsc::unbounded_channel()` for notifications. A misbehaving server flooding `$/progress` or `window/logMessage` would grow memory unbounded between `drain_notifications` calls. Long-running tests (`performance.rs`, `integration_full.rs`) the most exposed. Bound to e.g. 10K and warn on overflow. |
| F-OPEN-049 | P2 | `scopeguard_remove` Drop uses `try_lock`. On a contended map (rare — only `read_loop` holds the lock briefly) the cleanup silently no-ops, leaving a stale oneshot `Sender`. Map grows during a single long test, drops at process exit. Switch to `tokio::task::spawn` an async-locked cleanup, or use a different lock. |
| F-OPEN-050 | P2 | `read_loop` only matches numeric response ids (`msg.get("id").and_then(\|v\| v.as_i64())`). LSP allows string ids. tower-lsp and al-lsp use numeric, so this is latent — but a future server change to echo string ids would orphan responses. |
| F-OPEN-051 | P3 | `Lifecycle` enum has one variant (`Stdio(Child)`). Documented as historical (`connect()` was the other variant). Either re-introduce the variant or collapse to `Child` directly. |
| F-OPEN-052 | P3 | `file_uri` uses `path.to_str().unwrap_or("")` for non-UTF-8 paths. Linux/macOS dev environments aren't exposed to this; flagged for completeness. |

Workspace test count: 1878 → 1881 (+3 for the protocol round-trip tests). All gates green.

### Iteration 21 (2026-05-16, +600m)

Two commits, one new audit (LSP textDocument-sync lifecycle: `documents.rs` + `server/{lsp,diagnostics,workspace,handlers}.rs`, ~3.7K LOC).

The hottest LSP path in the codebase — every keystroke flows through here. Audit found one real P1, one stale doc comment, and two P2 follow-ups.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-036 | **P1** | Ghost diagnostics on `did_close` during the 400 ms debounce window. The spawned `schedule_diagnostics` closure had no document-still-open guard, and `did_close` did not abort the task. Sequence: keystroke → task A armed → user closes tab (did_close clears diagnostics) → task A wakes, computes from cached parse tree, publishes diagnostics for the closed doc → ghost squiggles in Zed. Fix: in-task `documents.contains(&uri)` guard PLUS `did_close` now aborts `diag_task`. New E2E regression test in `al-test-harness/tests/regression.rs` reproduces and pins the fix. |
| F-FIX-037 | P3 | Stale doc comment on `syntax_diag_to_lsp` claimed `SyntaxDiagnostic.range` carried byte columns. In fact `ts_range_to_query_range` (queries/diagnostics.rs:134) runs `byte_col_to_utf16_col` at the query layer. The shim is correct; only the comment was misleading. |

False positive caught:

| ID | Where | Why not a bug |
|---|---|---|
| F-FP-016 | "byte vs UTF-16 columns in `syntax_diag_to_lsp`" (P1 candidate) | Audit was misled by the stale doc comment. The actual data flow runs the conversion at the query layer; `SyntaxDiagnostic.range.character` is already UTF-16. Verified at queries/diagnostics.rs:134-150. Doc comment fixed in F-FIX-037. |

Carry-forwards (P2 — known fragility under refactor, not currently exploited):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-053 | P2 | `apply_changes` ignores `DidChangeTextDocumentParams.text_document.version`. LSP requires the client's version to be monotonic; we don't validate. Out-of-order delivery (rare under tower-lsp, but possible) silently corrupts the rope. Adding a version-mismatch warning + skip-with-resync would catch this. |
| F-OPEN-054 | P2 | `apply_changes` + separate `get_text` in `did_change` is not atomic — a concurrent notification could interleave so the text fed into `schedule_diagnostics` is one version ahead of the keystroke that triggered the call. Not data corruption (rope is internally consistent) but the version captured by the spawned diag task doesn't match the keystroke. With the F-FIX-036 close-guard in place this no longer produces ghost diagnostics, but a version check inside the diag task would still be sounder. |
| F-OPEN-055 | P2 | `al.reindex` does not coordinate with the initial `init_task` from `initialized`. Two concurrent `initialize_workspace` runs are possible if the user invokes `al.reindex` before initial init completes. Each component's internal locking saves us today, but a global init-mutex would be defence-in-depth. |
| F-OPEN-056 | P3 | `did_change_watched_files` is not implemented. External-to-Zed file changes (git checkout, generator scripts, sibling editor) don't refresh the workspace index until the user manually invokes `al.reindex` or restarts. Low priority; Zed itself rescans on focus. |

Workspace test count: 1881 → 1882 (+1 for the ghost-squiggle regression). All gates green.

### Iteration 22 (2026-05-16, +630m)

One commit, one P1 closed (compile timeout + kill), one P3 justified inline (hardcoded analyzer names).

Audit focus: the compile-and-ship path — `crates/al-core/src/{toolchain,build,publish}.rs` (1.5K LOC). Subprocess invocation safety, path traversal, error propagation, TLS, cancellation.

**Audit verdict:** mostly clean. Subprocess args always use per-arg passing (no `sh -c`). All public functions return `Result`. No `unwrap`/`panic`/`todo`/`unreachable` in production. `publish.rs` delegates entirely to `bc_client` for TLS/auth (covered by earlier iterations). One real P1 surfaced — child processes were not killable.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-038 | **P1** | `compile_project` ran `cmd.output().await?` with no timeout, no kill on cancel. tokio does NOT propagate task cancellation to child processes; a rapid-cancel sequence (or upstream `$/cancelRequest` dropping the spawning task) would leave the `alc` child running to completion uncollected, ~200 MB of working set each. Fix: `kill_on_drop(true)` + `tokio::time::timeout` (default 600s, configurable via `AL_COMPILE_TIMEOUT_SECS`, 0 disables). New `AlError::BuildTimeout(u64)` variant. 5 regression tests pin the env-var parsing contract. |
| F-FIX-039 | P3 | Justified four hardcoded analyzer DLL identifiers (`CodeCop`/`AppSourceCop`/`UICop`/`PerTenantCop`) inline. These are toolchain-side identifiers (Microsoft's published names for the built-in AL static analysers), not AL *language* values — they don't drift with BC releases, so they're exempt from CLAUDE.md's no-hardcoded-AL-values rule. Comment now explains the distinction. |

Carry-forwards (P2 — known fragility, not currently exploited):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-057 | P2 | `build.rs:86` does not canonicalise `project_root` before interpolating into `/project:{}` / `/out:{}`. A workspace folder containing `..` would be honoured by alc; the LSP/CLI caller controls this path so the attack surface is narrow, but `std::fs::canonicalize` at the entry would be defence-in-depth. |
| F-OPEN-058 | P2 | `build.rs:84-95` writes the `.app` artifact directly into `project_root` via `/out:`. A failed compile that crashes mid-write leaves a partial `.app`; `find_app_file_from_manifest` sorts by mtime as fallback and could pick up the partial. Atomic write would help but requires alc-side cooperation. |
| F-OPEN-059 | P2 | `toolchain.rs:82-90` honours `$AL_TOOL_PATH` without provenance checks. Consistent with how `dotnet` itself works; documented here for the threat model. The realistic attacker would need ENV access to the daemon process — at which point all bets are off anyway. |

Workspace test count: 1882 → 1887 (+5 timeout/error-variant tests). All gates green.

### Iteration 23 (2026-05-16, +660m)

Two commits, two real fixes (P1 + P2), several false positives caught.

Audit focus: `config.rs` (974 LOC settings merge), `project.rs` (304 LOC app.json), `parsing.rs` (117 LOC parse-cache hot path). The "user input gets trusted" surface.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-040 | **P1** | `get_or_parse` had no per-URI synchronisation. A keystroke fires hover + completion + semantic-tokens near-simultaneously; without a lock they all missed the cache, raced into `AlParser::parse_quick`, and each paid the 30-50 ms parse cost on a large file. Fix: `DocumentStore::parse_lock(uri) -> Arc<Mutex<()>>` lazily created; `get_or_parse` keeps the fast-path cache check (no contention on the cache-hit path) then acquires the lock and re-checks before parsing. Correctness preserved by `get_cached_tree`'s version check. Regression test spawns 16 threads on the same URI. |
| F-FIX-041 | P2 | `app.json` was read with `std::fs::read_to_string` and no size check. Pathological inputs (sparse-file or adversarial) would OOM the daemon. Now `metadata().len()` checked against 1 MiB cap before read. Largest legitimate manifest observed is ~20 KB; cap leaves two orders of magnitude headroom. 2 regression tests. |

False positives caught:

| ID | Where | Why not a bug |
|---|---|---|
| F-FP-017 | "Unbounded tree cache" (P1 candidate) | `documents.close()` (documents.rs:68-71) evicts both the doc and the tree entry. Each open file caps at 1 tree; total bounded by Zed's tab count. Not a leak. |
| F-FP-018 | "TOCTOU on version in get_or_parse" (P1 candidate) | `get_cached_tree` (documents.rs:161-169) explicitly checks the stored version against the live `doc.version` and returns None on mismatch. A stale tree cached under an old version is never served. The existing test `apply_changes_version_bump_and_tree_remove_are_consistent` pins this invariant. With F-FIX-040's per-URI lock added, the brief-wasted-parse window is also closed. |
| F-FP-019 | "config.rs::merge has no observer notifications" | Out of audit scope — wiring lives in `workspace.rs`. Verified: symbols cache, NuGet client, semantic bridge all read config at action time, so a post-init merge takes effect on the next operation. Not a bug. |

Carry-forwards (P2/P3 — defensible-in-depth, defer):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-060 | P2 | `config.rs::merge` accepts arbitrary string paths for `editorServicesPath`, `assemblyProbingPaths`, `ruleSetPath`, etc. No canonicalisation; consumers (build/DAP/symbols) trust the path. Settings are user-owned so the realistic exposure is narrow, but a canonicalise-at-boundary pattern would be cleaner. |
| F-OPEN-061 | P3 | `config.rs::AlConfig::load` accepts unknown JSON keys silently (no `unknown_keys` reporting on disk-loaded config). Inconsistent with `merge()`, which collects them. |
| F-OPEN-062 | P3 | `config.rs` enum merges silently retain current value on unrecognised variant (`diagnosticsScope`, `diagnosticsTrigger`, log-level, malformed nuget feed entries). Should surface as `unknown_keys` so the user sees the typo. |
| F-OPEN-063 | P3 | `config.rs` `null` semantics are inconsistent. Optional `PathBuf`/`String` honour `null` as "clear"; booleans/arrays/enums silently ignore it. User can't reset most fields to default without removing the key entirely. |
| F-OPEN-064 | P3 | `project.rs:92` hardcodes `"26.0.0.0"` as fallback for `platform` when `application` is unset. Will go stale with each major BC release. Lift to a `const` with a note that it tracks current BC. |

Workspace test count: 1887 → 1890 (+1 concurrent-parse regression, +2 app.json size-cap tests). All gates green.

### Iteration 24 (2026-05-16, +690m)

One commit. Audit focus: daemon LSP/debug dispatch + workspace init (~2K LOC across `daemon/{mod,lsp_dispatch,debug_dispatch}.rs` and `workspace.rs`).

**Audit verdict:** the daemon main loop is robust — bounded connection semaphore (64), exponential backoff on accept errors, signal-handler cleanup, F-FIX-011 idle-race fix verified intact, no production unwrap/panic. The dispatch layer correctly returns `METHOD_NOT_FOUND -32601` on unknown methods, applies the 64 MB line cap, and the boundary between `queries::*` and `lsp_types::*` is clean. Found three integer-cast issues plus several P2 follow-ups.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-042 | P3 | Three `as_i64() as i32` / `as_u64() as u32` sites silently wrapped on overflow. `dispatch_by_id` (lsp_dispatch.rs:412) would map an object ID >2³¹-1 to a negative i32 and either miss legitimate objects or hit unintended ones. `dispatch_debug` breakpoint `line` defaulted to 0 on missing/bad input — a phantom breakpoint at the top of the file. `objectType`/`objectId` followed the same wrap pattern. Added `daemon::extract_i32(params, key) -> Option<i32>` helper (mirrors existing `extract_position` discipline), routed all three sites through it, and gated `line` behind an `INVALID_PARAMS` error instead of the silent default-to-0. 4 regression tests pin the contract. |

Carry-forwards (P2/P3 — fragility or design decisions, not bugs):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-065 | P2 | No `$/cancelRequest` support in daemon mode. Long-running endpoints (`deadCode`, `impact`, `compile`, `tests.mutate`, `xlf.generate`, `downloadSymbols`) cannot be cancelled; a client that abandons still pays full daemon-side cost. Worse, a buggy/malicious client can pin all 64 connection slots on expensive queries. F-FIX-038 (alc timeout) partly mitigated this for compile; the rest still need plumbed cancel tokens. |
| F-OPEN-066 | P2 | `workspace::on_document_change` (workspace.rs:524-552) invalidates BOTH `insight_graph` AND `call_graph` caches on every keystroke. The next cross-file query (`deadcode`, `impact`, `find references`-via-graph) pays the 100-200 ms rebuild. Most keystrokes don't trigger graph rebuilds, so user-visible impact is bounded, but a typing-then-impact sequence is observable. Debounce or finer-grained invalidation. |
| F-OPEN-067 | P2 | `daemon::run_daemon` idle-timeout `try_lock` defaults to "active" if `debug_session` mutex is held (daemon/mod.rs:101-108). Currently safe — a held debug-session mutex DOES mean we have an active debug session — but if the mutex were ever held permanently by a bug elsewhere, the daemon would never time out. Worth a comment naming the invariant. |
| F-OPEN-068 | P3 | `last_activity` is a `tokio::sync::Mutex<Instant>` taken on every connection accept + every dispatch. Under high RPS this serialises. An `AtomicU64` storing `Instant`-as-millis would remove the bottleneck. |
| F-OPEN-069 | P3 | Daemon dispatchers handle `std::sync::RwLock` poison by returning `INTERNAL_ERROR` rather than the `unwrap_or_else(\|e\| e.into_inner())` poison-recovery pattern used in `workspace.rs`. A single poisoned `package_info`/`project` lock permanently breaks `dispatch_packages`/`dispatch_deps`. |
| F-OPEN-070 | P3 | No graceful in-flight drain on SIGTERM. The accept loop breaks but in-flight connection tasks are not joined — a 64 MB read or compile in-flight is cut off. Socket cleanup runs first so it's just request-loss, not corruption. |
| F-OPEN-071 | P3 | `initialize_core_workspace` swallows package-load errors (workspace.rs:431-435). A corrupt `.app` silently lowers the symbol count; daemon `status` reports the partial count but the user has no other surface. |

False positive caught:

| ID | Where | Why not a bug |
|---|---|---|
| F-FP-020 | F-OPEN-055 / concurrent init blast radius | The audit re-verified: two concurrent `initialize_core_workspace` calls would each run the FS scan and package load — wasted work, but `SymbolIndex` is DashMap-backed (concurrent insertion safe) and `package_info`/`project` writes overwrite atomically (last-writer-wins). Net result: cycles wasted, not data corrupted. Downgraded from "fragile" to "wasteful". |

Workspace test count: 1890 → 1894 (+4 daemon integer-cast tests). All gates green.

### Iteration 25 (2026-05-16, +720m)

Two commits. Audit focus: semantic bridge + launch.rs (~2.3K LOC across `semantic/{bridge,host,lifecycle,cache,mod}.rs` and `launch.rs`).

**Audit verdict:** the CLR bridge is the most defensively-written FFI code in the crate. Every `unsafe` block has an accurate SAFETY comment. `ClrBuf` RAII guard frees CLR buffers even on panic. `c_int::try_from` prevents truncation-on-cast. `Send`/`Sync` impls document why the pointers are safe to share. Function-pointer ABI is locked at load time via `extern "system"` + `unmanaged_callers_only`. No production unwrap/panic anywhere.

Found one P1 (launch.json size cap parity with project.rs) and one P2 (bridge restart counter accumulated forever).

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-043 | P1 | `parse_zed_debug_file` and `parse_vscode_launch_file` (launch.rs:218-238) read with no size check. Pathological inputs would OOM the daemon. Mirror the `project.rs:175-184` 1 MiB cap via a `read_launch_file_capped` helper. 3 regression tests (under-cap parses, oversize VS Code rejected, oversize Zed rejected). |
| F-FIX-044 | P2 | `bridge_restart_count` was monotonically incremented and never reset. After MAX_RESTARTS=3 successful crash-and-recovery cycles, the bridge becomes permanently disabled — even if those 3 restarts were well-spaced over hours. Reset to 0 on the `restart_bridge` success branch. Thrash protection preserved: 3 *consecutive* failed restarts (each crashing before reset) still trip the cap. |

Carry-forwards (P2/P3 — design or low-impact):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-072 | P1 | The serializing mutex on bridge calls (`SemanticBridge::call`) has a 30 s timeout, but the doc comment at `bridge.rs:226-229` admits the timeout doesn't release the Mutex or interrupt the .NET call. A permanently-wedged CLR call blocks every semantic feature forever. The cooldown / `try_lock` mitigation at `bridge.rs:264-302` reduces the blast radius but no path force-aborts the wedged blocking thread. Plumbing a real cancellation signal across FFI is non-trivial — design first. |
| F-OPEN-073 | P3 | `launch.rs:285-295` `parse_environment_type` silently drops a config on unknown env type — user sees only a `warn!` in logs. Consider surfacing as a structured error so the debug-launch path can report "configuration X has bad environmentType=Y" to the UI. |
| F-OPEN-074 | P3 | `launch.rs` `server` field accepts any scheme (e.g. `file:///etc/passwd`). It eventually flows into `bc_client` which has its own validation, but defence-in-depth allowlist at the launch layer would catch malformed configs sooner. |
| F-OPEN-075 | P3 | `bridge.rs:148` `Poisoned` error variant is reused for cooldown short-circuiting at `bridge.rs:262, 288, 300` — three distinct conditions share one error string. Split or rename. |
| F-OPEN-076 | P3 | Cache filename built from `sanitize_version` has no length cap (`cache.rs:31`). A pathological version string creates an oversized filename. Trivial. |

Workspace test count: 1894 → 1897 (+3 launch.json size-cap tests). All gates green.

### Iteration 26 (2026-05-16, +750m)

One commit, one real P1 fix. Two audits this iteration (`file_index.rs` and `insight/{discovery,analysis}.rs`).

**`file_index.rs` audit:** **clean**. No P0/P1. No DashMap-across-await (sync code), iterative walk, no hardcoded AL keywords (object kinds come from `syntax::find_object_declaration`), no production unwrap/panic. `remove_file` evicts all 7 maps cleanly. F-010 deletion sweep and F-040 collision-safe removal are pinned by existing tests. Two P3 hot-path observations recorded as F-OPEN-077/078 below.

**`insight/discovery.rs` + `insight/analysis.rs` audit:** one real P1 + minor housekeeping.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-045 | **P1** | `table_impact` (`insight/analysis.rs:127`) detected cross-table references via `TableRelation`. Comment promised "extract table part (before any dot or WHERE)" but implementation only split on `.`. Filter-clause values like `"Customer" WHERE("Blocked" = CONST(""))` silently fell through — the related table was never recorded as impacted. Common across BC standard tables (Sales Header, Purchase Line, GL Entry). Fix: new `extract_table_relation_table` helper handles all 6 documented shapes (bare, quoted, dot-field, WHERE, quoted+WHERE, multi-word+WHERE). Also chipped two hot-path allocations: `TableExtension extends` uses `eq_ignore_ascii_case` (was `to_lowercase()` per entry); `is_record_of` splits on any whitespace (was literal space — missed tab-separated type strings) and uses `eq_ignore_ascii_case`. 10 new tests. |

False positive caught:

| ID | Where | Why not a bug |
|---|---|---|
| F-FP-021 | "hardcoded AL string `TableRelation`" (P3) | The audit flagged `prop.name.eq_ignore_ascii_case("TableRelation")` as a hardcoded AL value. Verified: this is a property *name* in `.app` metadata that we're reading FROM `al_core::symbols` — i.e. we are consuming the canonical source, not redefining it. Same for the literal `"Record"` prefix in `is_record_of`. Both are reading external AL toolchain output, not redefining AL semantics. Not a CLAUDE.md violation. |

Carry-forwards (P2/P3 — housekeeping):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-077 | P3 | `file_index.rs::index_from_result` is non-atomic across 7 DashMaps — a panic between `file_trees.insert` and `files.insert` leaves the index split. Tree-sitter parse + symbol extraction are unlikely to panic, but a future contributor adding a new map mutation could widen the window. Worth a comment. |
| F-OPEN-078 | P3 | `file_index.rs::index_from_result` allocates a fresh `Vec<String>` for `proc_names` and `al_doc_symbols` on every keystroke; also calls `to_lowercase()` per procedure name. Bench-worthy only on 1K+ procedures-per-file (none observed in real codebases). Defer. |
| F-OPEN-079 | P3 | `insight/analysis.rs::table_impact` allocates a fresh `String` for `table_lower` at the entry. Worth eliminating only if the function becomes very hot — currently only called from CLI `impact` subcommand. Defer. |

Workspace test count: 1897 → 1907 (+10 TableRelation parser + helper tests). All gates green.

### Iteration 27 (2026-05-16, +780m)

One commit. Audit focus: `insight/graph.rs` (1076 LOC) — the InsightGraph constructor powering cross-file queries (deadcode, impact, suggest_event, code_lens references).

**Audit verdict:** structurally sound — iterative construction, no panic surface, no DashMap-across-await (sync code), cycle-tested. The earlier iteration-26 audit flagged "hardcoded attribute name strings" (IntegrationEvent / BusinessEvent / EventSubscriber) appearing at 9 production sites across graph.rs + calls.rs. Re-examined here: these are runtime-ABI strings emitted into `.app` symbol JSON by Microsoft's compiler, not AL language surface — they don't fall under the CLAUDE.md no-hardcoded-AL-values rule. But they are worth centralising for grep-ability and to localise any future rename.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-046 | P3 | Extracted `INTEGRATION_EVENT`/`BUSINESS_EVENT`/`EVENT_SUBSCRIBER` to `insight::attr_names` mod with a docstring distinguishing "BC event-system runtime ABI" from "AL language values". 9 production call sites migrated; test-only literals inside `#[cfg(test)]` left as bare strings (those are the contract under test). Also clarified the `break` at `graph.rs:386` (subscriber kind fallback) — it's intentional kind disambiguation, not a premature exit. Comment updated to match the behaviour. |

Carry-forwards (P2/P3 — design, low-impact):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-080 | P2 | `InsightGraph::add_edge` runs `edges_connecting(from, to)` on every insert for dedup. For an Object node with N method-Contains edges, building those is O(N²). On a 200-method table extension that's ~40K ops — fast in practice; on adversarial 5K-method workspaces it'd be noticeable. Track inserted `(from, to, edge)` in a `HashSet` during `build_from_index` to amortise. Not urgent (build budget is documented at 50-200 ms). |
| F-OPEN-081 | P3 | `InsightGraph` public API returns `petgraph::graph::NodeIndex` from `ensure_node`/`get_node`/`get_nodes`/`add_edge`/`remove_edges_from`. This bakes `petgraph` into the contract; the struct docstring at L104-108 acknowledges. Wrap in a newtype (`pub struct InsightNodeId(NodeIndex)`) if you ever need to swap graph backends. |

Workspace test count: 1907 unchanged (refactor + comment changes only). All gates green.

### Iteration 28 (2026-05-16, +810m)

One commit. Audit focus: `insight/calls.rs` (1809 LOC, second-largest file) — call-graph builder powering `find references`, `code lens`, `deadcode`, `impact`, `breaking_changes`.

**Audit verdict:** structurally sound (iterative traversal everywhere, no `unwrap`/panic, no DashMap-across-await, fully sync). But three real correctness bugs surfaced in the resolution layer.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-047 | **P1** | Attribute-name comparisons were case-sensitive (`name == "EventSubscriber"`) at 4 sites. AL attributes are case-insensitive — `[eventsubscriber(...)]`, `[EVENTSUBSCRIBER(...)]` are both valid. Tree-sitter preserves source case in raw text, so non-canonical spellings silently lost their event/subscriber classification, breaking deadcode/impact analysis on real-world code that doesn't capitalise canonically. Fix: `eq_ignore_ascii_case` everywhere we compare AST-derived attribute names. (graph.rs is unaffected — it operates on `AttributeSymbol` from `.app` metadata, already normalised.) |
| F-FIX-048 | **P1** | `MemberCall` resolution broke after the first successful edge insert. `symbols.get_by_name(object)` returns ALL entries with that name — workspaces with the same name across kinds had calls resolved against only the first hit, dropping the rest. Removed the `break`; downstream consumers (find references / code lens) dedupe as they need. |
| F-FIX-049 | **P1** | `tier1_threshold` doc says "score >= 5 OR top 20%" but the code returned only the percentile cutoff. In a busy workspace where 80th percentile sat at 20, files with scores 5-19 were gated OUT despite crossing the documented `>= 5` floor. Fix: `clamp(1, 5)` on percentile so either condition admits. |
| F-FIX-050 | P3 | Removed unused `_symbols: &SymbolIndex` parameter from `register_procedures_from_tree` (1 production + 1 test call site). |

Carry-forwards (P2/P3 — defer):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-082 | P2 | `RecordOp::from_method_name` (`calls.rs:42-50`) hardcodes the AL built-in record method tokens `insert`/`modify`/`delete`/`validate`. These are stable BC API names since NAV 2.0 — borderline against CLAUDE.md's no-hardcoded-AL-values rule. Source of truth would be `LanguageData::builtin_methods` if/when that surface exists. Defer. |
| F-OPEN-083 | P2 | `record_op_event_names` (`calls.rs:615-626`) hardcodes the BC table-event naming pattern `OnBefore{Op}Event` / `OnAfter{Op}Event`. If Microsoft introduces a new naming convention this silently misses edges. Either derive from symbol-scan of `IntegrationEvent` attributes on Table objects, or document the assumption. |
| F-OPEN-084 | P2 | Member-call object lookup (`calls.rs:386, 564`) uses the *variable name* as a symbol lookup key — only resolves when the variable name happens to equal a real object name. The semantically-correct lookup is variable → declared type → object. `extract_procedure_var_types` already builds the variable-to-Record-type map; extending to all object-typed variables would catch missing edges for codeunit and page variables. The biggest missed-edges class in the file. |
| F-OPEN-085 | P2 | `populate_call_edges_for_procedure` re-walks the tree multiple times per procedure (find proc node, extract call sites, extract var types). A single walk that collects everything would amortise. Hot path on graph build. |
| F-OPEN-086 | P3 | `extract_return_type` doc-comment claims "check if preceded by `:`" but no actual colon check; relies on field order. Document or implement. |
| F-OPEN-087 | P3 | `parse_run_trigger_arg` returns `true` on parse failure (AL default). Correct behaviour but produces false-positive trigger edges when the call expression is complex. Worth a debug-log warning so the false-positive rate is visible. |

False positive caught:

| ID | Where | Why not a bug |
|---|---|---|
| F-FP-022 | "`"workspace"` magic string used as package key" (P0 candidate) | The audit flagged the literal string `"workspace"` at three call sites as fragile stringly-typed coupling. Verified: this is a sentinel for "this object lives in user workspace code, not a .app package". The convention is documented at the SymbolEntry struct level — not AL language data, not an external contract. Worth a constant for grep-ability but not a CLAUDE.md violation. Recording as a possible cleanup. |

Workspace test count: 1907 → 1910 (+3 case-insensitive subscriber + 2 tier1 threshold tests). All gates green.

### Iteration 29 (2026-05-16, +840m)

One commit. Audit focus: `insight/search.rs` (1052 LOC) — the graph-traversal layer powering trace_event, trace_event_chain, find_entry_points, deadcode walks.

**Audit verdict:** cycle safety / depth caps / DashMap discipline / NodeIndex hygiene / `lsp_types::*` isolation all clean. F-OPEN-027's `MAX_CHAIN_NODES = 10_000` cap is still in place. But two determinism gaps in `trace_event_chain` and `trace_from_node` slipped through when the earlier `trace_event` fix landed.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-051 | **P1** | `trace_event_chain` (search.rs:231) and `trace_from_node` (search.rs:130) iterated `insight.index` (HashMap) without sorting. `trace_event` had the matching fix at search.rs:69 with a regression test, but its richer siblings did not. Combined with F-OPEN-066 (graph wiped every keystroke), the unstable order surfaces immediately as flaky diffs in DOT/JSON exports and any test that snapshots trace output. Fix: collect-then-sort by NodeIndex at both sites; collapsed a nested `for/for` pyramid into a single iter-filter-sort matching the trace_event style. New regression test asserts root order is identical across 5 graph rebuilds of the same workspace. |

Carry-forwards (P2/P3 — defer):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-088 | P2 | `trace_from_node` re-scans the entire `graph.index` HashMap to find events for each subscriber's object — O(N·V) on a 50K-node graph with N subscribers. The index is keyed by `NodeKey::Event(_, obj, _)` but there's no obj-only secondary index. Add one when graph size makes this a measurable hot path. |
| F-OPEN-089 | P2 | `recurse_event` / `recurse_subscriber` (search.rs:309, 352) emit children in `cg.subscribers_of(...)` / `cg.callees_of(...)` order. Stability depends on CallGraph internals; not obviously sorted. Worth a determinism test on the wider chain output, not just root order. |
| F-OPEN-090 | P2 | `serde_json::to_value(node).unwrap_or_default()` at search.rs:500 silently emits `{}` if serialization fails. The struct is a plain `Serialize` derive so failure is unreachable in practice, but the silent-default pattern hides a future regression. Log-or-panic-on-error would surface a real bug. |
| F-OPEN-091 | P3 | `match node_type { "event" => ..., "procedure" => ... }` dispatches on stringly-typed `NodeInfo.node_type`. An enum match over `InsightNode` variants would be safer. Borderline against CLAUDE.md (not AL language, but stringly-typed graph-internal dispatch). |

Workspace test count: 1910 → 1911 (+1 trace_event_chain determinism regression). All gates green.

### Iteration 30 (2026-05-16, +870m)

One commit. Closing F-OPEN-089 from iter-29 — the chain-children determinism follow-up.

Iter-29 sorted trace_event_chain root order; this iteration sorts the children under each root. `subscribers_of` / `callees_of` returned NodeIds in CallGraph build order, which tracks DashMap iteration in SymbolIndex — non-deterministic across process restarts. The graph IS rebuilt every keystroke, so the unstable child order surfaces in test output and DOT/JSON exports of any non-trivial chain.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-052 | P1 | F-OPEN-089 closed. `recurse_event` sorts subscribers by NodeId; `recurse_subscriber` clones callees and sorts by `(target NodeId, kind_rank)` with a total-order discriminant so duplicate targets with different edge kinds are also stable. Bounded by MAX_CHAIN_NODES (10K) so per-call sort cost is negligible. Strengthened regression test asserts a flattened subscriber list under the chain root is byte-identical across 5 rebuilds — catches drift in either roots OR children. |

Workspace test count: 1911 → 1912 (+1 chain-children determinism regression). All gates green.

### Iteration 31 (2026-05-16, +900m)

One commit. Audit focus: `test_engine/mutate.rs` (1004 LOC) — mutation testing engine, called from the daemon `tests.mutate` endpoint and `al mutate` CLI.

**Audit verdict:** the mutation generator is structurally sound (iterative traversal, no panics, no DashMap-across-await, no `lsp_types::*`, operators matched on tokens not keywords). But a P0 leaked through the scaffolding: the test-execution layer is a stub that unconditionally reports "survived", and the public `mutation_score()` returned 0.0 instead of signalling "no real signal yet".

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-053 | **P0** | `run_single_variant` (mutate.rs:611) was a scaffolding stub that always returned `killed: false` — `mutation_score()` then dutifully divided 0 by total and returned 0.0. The daemon `tests.mutate` and `al mutate` CLI surfaced this as a definitive-looking report. Added `MutationExecutorPhase { Stub, Interpreter }` to `MutationReport`; `mutation_score()` now returns `Option<f64>` (`None` under stub or no-variants). All three construction sites (in-process driver, daemon dispatch, daemon empty-files early-return) tagged as `Stub`. Consumers can now warn the user that the score is not yet meaningful. Tests updated to the `Option<f64>` API; one new test pins the stub-phase contract. |
| F-FIX-054 | P1 | `collect_mutation_files` (mutate.rs:578) iterated `file_index.files` (DashMap) so `report.variants` was ordered by shard hash — different across process restarts. CI snapshots and human review diffed spuriously. Now `files.sort()` before returning. |

Carry-forwards (P2/P3 — design / future-phase concerns):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-092 | P2 | `apply_variant` (mutate.rs:473) clamps `variant.byte_start` to `source.len()` but not to a char boundary. Stale variants from a between-mutation source edit would panic on `&source[..start]`. Variant byte positions today come from tree-sitter so are char-safe; a future user-supplied variant API needs `floor_char_boundary` or a `Result` return. |
| F-OPEN-093 | P2 | `run_mutation_testing` has no `CancellationToken`. Once the interpreter backend lands, a daemon `$/cancelRequest` mid-run leaves the current variant running to completion. Plumb cancellation through before the stub is replaced. |
| F-OPEN-094 | P3 | `affected_only` mode double-parses every file — once in `collect_mutation_files`, again in `generate_variants_for_file`. Pass `(text, tree)` through. |
| F-OPEN-095 | P3 | `generate_variants` takes `&tree_sitter::Tree` in its public signature — leaks tree-sitter into the API. Consider `pub(crate)` and exposing only the workspace-aware variant. |

Workspace test count: 1912 → 1913 (+1 stub-phase contract test). All gates green.

### Iteration 32 (2026-05-16, +930m)

One commit. Audit focus: `test_runtime/interpreter/eval_stmt.rs` (1200 LOC) — the statement evaluator. Daemon-critical because it runs user test bodies in-process; a panic here kills the daemon.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-055 | **P0** | `eval_stmt` recursed directly for every nested block, branch, loop body. `dispatch.rs::MAX_RECURSION_DEPTH` (100) caps call recursion only — a single procedure body containing thousands of nested `begin/end` or `if … then if …` would blow the Rust stack and kill the daemon. Added `DispatchCtx::ast_depth` counter (cap `MAX_AST_DEPTH = 1024`) incremented around the recursive call. Sized above the cumulative AST levels a 100-deep call chain can produce (~4 levels per frame) so infinite-call tests still trip the cleaner `recursion_depth` error first. Pathological single-procedure nesting now aborts with a clear `Eval::Error("AST nesting depth exceeded …")`. Iterative rewrite deferred — the cap is the load-bearing fix. |
| F-FIX-056 | **P1** | `for ... to` direction detection used `node_text(node).to_ascii_lowercase().contains("downto")` — matched anywhere in the for-statement text, including the body. Any identifier or string literal containing the substring (e.g. `MyDownToValue`) silently flipped direction. Now consults the grammar's `direction` field (`kw_downto` / `kw_to`); substring check kept as fallback for malformed parses. |

Carry-forwards (P1/P2 — design or out of scope for this iteration):

| ID | Severity | Title |
|---|---|---|
| F-OPEN-096 | P1 | Cancellation token (F-OPEN-093) still not plumbed. `eval_stmt` loop bodies check `ctx.deadline_exceeded()` but not a separate `is_cancelled` signal. A daemon `$/cancelRequest` mid-test cannot interrupt the interpreter before the wall-clock deadline. Needs a Notify or atomic bool threaded through. |
| F-OPEN-097 | P1 | `eval_case`'s `case_else` body lookup falls through to `named_child(0)` if the field isn't set — could pick a `kw_else` keyword node instead of the actual body. Similar pattern to F-FIX-056. Verify in a follow-up. |
| F-OPEN-098 | P2 | `apply_variant` UTF-8 boundary risk (mutate.rs): `source.len()` clamp doesn't enforce char boundary. Variants today come from tree-sitter (char-safe) but a future user-supplied variant API would need `floor_char_boundary`. |
| F-OPEN-099 | P2 | `values_equal_for_case` (line 962) compares Integer↔Decimal via `*x as f64 == *y` — lossy above 2^53. Edge case for currency-like values. |
| F-OPEN-100 | P2 | `eval_args_into` absorbs `Eval::Exit` as a value rather than unwinding the enclosing procedure. AL semantics: `exit` in an argument expression should propagate. |

Workspace test count: 1913 → 1916 (+3 ast_depth + downto regression tests). All gates green.

### Iteration 33 (2026-05-16, +960m)

One commit. Closing three iter-32 follow-ups in a single batch — all single-file fixes in `eval_stmt.rs`.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-057 | P1 | F-OPEN-097 closed. `eval_case` matched on `"case_else" \| "else_clause"` — those node kinds don't exist in the AL grammar (`else_body` is a field on `case_statement` directly). The dead arm meant the else branch never ran. Now reads `child_by_field_name("else_body")` once at the top of eval_case. |
| F-FIX-058 | P2 | F-OPEN-099 closed. `values_equal_for_case` did `*x as f64 == *y` for Integer↔Decimal — lossy above 2^53 (e.g. currency-magnitude i64 values would falsely compare equal to a Decimal that lost precision in the cast). Now: round-trip via i64 if the Decimal has zero fractional part AND fits in i64; otherwise the values cannot be equal. |
| F-FIX-059 | P2 | F-OPEN-100 closed. `eval_args_into` absorbed `Eval::Exit(v) => out.push(v)`, silently passing the exit value through as a regular argument. AL semantics: `exit(v)` in argument position should unwind the enclosing procedure. Introduced private `ArgsShort { Error, Exit }` short-circuit enum; the call site maps `ArgsShort::Exit` back to `Eval::Exit`. |

Workspace test count: 1916 → 1920 (+4 batch regression tests). All gates green.

### Iteration 34 (2026-05-16, +990m)

One commit. Two P3 follow-ups closed:

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-060 | P3 | F-OPEN-064 closed. `project.rs` hardcoded `"26.0.0.0"` BC fallback pulled out to `CURRENT_BC_MAJOR_FALLBACK` const with a docstring naming the maintenance contract. |
| F-FIX-061 | P3 | F-OPEN-079 closed. `table_impact` no longer allocates a lowercased `table_lower`; downstream compares already used `eq_ignore_ascii_case`. `is_record_of` parameter renamed for clarity. |

Workspace test count: 1920 unchanged. All gates green.

### Iteration 35 (2026-05-16, +1020m)

One commit. F-OPEN-074 closed.

| ID | Severity | Resolution |
|---|---|---|
| F-FIX-062 | P3 | F-OPEN-074 closed. `BcServerConfig::dev_packages_url` (OnPrem) interpolated the user-supplied `server` field into an HTTP URL handed to the BC dev client unfiltered. Now passes through `is_safe_http_server` which allowlists `http`/`https` explicit schemes and bare hostnames; rejects `file://`, `gopher://`, `javascript:`, `ftp://`, and empty/whitespace inputs with a warn-log. 2 regression tests. |

Workspace test count: 1920 → 1922 (+2 server-scheme allowlist tests). All gates green.



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


