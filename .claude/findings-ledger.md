# Findings Ledger (authoritative open-issue tracker)

Single source of truth for F-OPEN status. The perfect-loop workflow reads the `open` rows here
instead of re-parsing FINDINGS.md prose. Update this file whenever a finding's status changes.
FINDINGS.md remains the narrative history; this is the index.

## Open (actionable)

| ID | Severity | Title |
|---|---|---|
| F-OPEN-010 | P2 | Token zeroization — `access_token`/`refresh_token` are plain `String`s in a long-lived daemon; adopt `zeroize` |
| F-OPEN-016 | P3 | Hardcoded BC protocol version assumptions in `bc_debug.rs`; needs version-detection / capability probe |
| F-OPEN-065 | P2 | No daemon `$/cancelRequest` support; abandoned long-running endpoints still pay full cost / pin slots |

## Resolved / parked (not actionable — do not re-open)

| ID | Severity | Status | Title |
|---|---|---|---|
| F-OPEN-043 | P3 | fixed | Tree-sitter parse-tree cache has no eviction (LRU / idle sweep) (iteration 96) — bounded `DocumentStore::trees` with an approximate-LRU cap. The leak was real and daemon-specific: `server/daemon/mod.rs` opens every scanned workspace file (`initialize_daemon_workspace`, line 788-792) and lazily loads more from disk on demand (`ensure_document`, line 730-739) **without ever calling `close()`** — only the LSP `did_close` path (`server/lsp.rs:551`) evicts. Every query routes through `parsing::get_or_parse` → `cache_tree`, so `trees` (and `docs`) grew once per file ever touched and never shrank. Prior partial work (F-OPEN-007 era) only fixed the `parse_locks` leak. Fix: `trees` now stores `CachedTree { version, tree, last_access }`; `cache_tree`/`get_cached_tree`/`get_cached_tree_at_version` stamp each entry from a monotonic `tree_access_counter`; `cache_tree` calls `evict_trees_over_cap()` which, when `len > cap`, drops the lowest-stamped (least-recently-used) entries. Cap is an atomic `max_cached_trees` seeded with `DEFAULT_MAX_CACHED_TREES = 256`, retunable via `set_max_cached_trees(Option<usize>)` (`None`/`0` = unbounded, preserving historical behaviour). Trees are a pure derived cache (re-parse ~30-50 ms via `parse_quick`), so eviction only ever forces a future re-parse — correctness is unchanged (version-pinned validation still holds). DashMap reads drop the `docs` ref before touching `trees`, and `evict_trees_over_cap` collects keys into a Vec before removing, so no shard is held across a second access. +4 regression tests (LRU eviction over cap, hot-entry survival, unbounded-when-cleared, default-cap bounds daemon-style growth). A full idle-sweep timer remains unnecessary given the hard count cap. |
| F-OPEN-081 | P3 | wontfix | `InsightGraph` public API leaks `petgraph::NodeIndex`; wrap in a newtype to allow backend swap (iteration 94) — a newtype on the five accessors (`ensure_node`/`get_node`/`get_nodes`/`add_edge`/`remove_edges_from`, `graph.rs:143-192`) buys nothing today and does not enable a backend swap. (1) No external consumer exists: no crate depends on `al-core` as a library (al-explorer/al-protocol/zed-al do not — checked Cargo.toml); the only callers of these methods are al-core's own `insight/calls.rs`, `index.rs`, and `queries/suggest_event.rs`, plus tests. The "leak" is internal-only and the struct already documents the deliberate `pub(crate)` boundary confining petgraph reads to al-core (`graph.rs:104-108`). A `NodeId(pub usize)` newtype already exists at the `CallGraph` layer (`index.rs:48`). (2) A newtype would not decouple petgraph: `search.rs`/`discovery.rs`/`analysis.rs` traverse the raw `DiGraph` directly through the `pub(crate)` field — `graph.graph[idx]`, `edges_directed(.., Direction::Incoming)`, `edges_connecting`, `NodeIndex`-keyed buckets (e.g. `search.rs:94,139,145,166,269,479-559`, `discovery.rs:99,119,174`) — and `queries/suggest_event.rs:197,366,429,464` round-trips `NodeId.0` back into `petgraph::graph::NodeIndex::new(..)` to index `insight.graph[..]`. petgraph is woven through the entire insight subsystem; a true backend swap requires abstracting the whole graph + all traversals, i.e. a subsystem redesign warranting its own finding, not a wrapper. Wrapping only the five methods is a broad, behavior-neutral, speculative API churn ("if you ever need to swap backends") that conflicts with CLAUDE.md scope discipline (no public-API changes unless the task requires it). No code change.
| F-OPEN-046 | P3 | already-resolved | TUI daemon-socket reads have no timeout (only Ctrl+C escapes a stuck query) (iteration 94) — premise is factually false against the code (and has been since the original `v3` commit `6a16700`, which predates this finding's iteration 19). `DaemonClient::from_stream` (`al-protocol/src/client.rs:206-218`) sets a default 30s `set_read_timeout` (`SO_RCVTIMEO`) on the `UnixStream` before wrapping it in the `BufReader`; every read — CLI *and* TUI — flows through `read_response` → `read_bounded_line` on that timed socket, so a wedged daemon surfaces `Err("Failed to read response: …")` after 30s instead of hanging until Ctrl+C. The "different ops have different latency budgets" design concern is already addressed by the public `set_read_timeout()` override (`client.rs:221`), which the debug CLI uses to raise the budget to 120s (`al-explorer/src/cli/commands/debug.rs:14`). The TUI uses the identical `DaemonClient` (`main.rs:30,137,266,570,812`); there is no separate untimed read path. No code change needed; existing `f046_*` tests cover the spawn-lock path |
| F-OPEN-042 | P3 | fixed | No per-document size cap — a 10 GB open file consumes memory unbounded (iteration 94) — added optional config-driven cap `al.maxDocumentSizeBytes` (`AlConfig::max_document_size_bytes`, default `None` = unbounded, `null` resets, non-integer surfaced as unknown). Enforced at the store boundary in `DocumentStore` via an atomic `max_doc_bytes` (`0` = no cap): `open()` and full-document replacements in `apply_changes_and_get()` refuse oversized content (warn + skip, prior text untouched) so the giant payload is never copied into the rope/cache. Wired in `server::lsp` `initialize` + `did_change_configuration` via `set_max_doc_bytes`; +7 documents tests, +1 config merge test. Incremental edits left unguarded (the scenario is opening a huge file); daemon defaults to no cap |
| F-OPEN-137 | P1 | fixed | SignalR negotiate-response version validation (`negotiateVersion=1`, `version:1`) (iteration 94) — added `resolve_negotiate_connection()` in `bc_debug.rs`: reads the server-echoed `negotiateVersion`, uses `connectionToken` as the WebSocket `?id=` for v1, falls back to `connectionId` for v0/missing (previously hard-failed "No connectionToken"), errors on redirect (`url`) responses, and warns on unexpected versions instead of panicking; +8 regression tests. The broader BC capability probe stays separate as F-OPEN-016 |
| F-OPEN-001 | P3 | fixed | All 25 `#[allow(clippy::*)]` justified/test-only (iteration 53) |
| F-OPEN-002 | P3 | wontfix | Split 6 files >1500 LOC — restructuring forbidden by CLAUDE.md scope discipline (iteration 81) |
| F-OPEN-003 | P3 | documented | `build_dispatch.rs` unsafe blocks confirmed test-only / informational (iteration 53) |
| F-OPEN-004 | P3 | deferred | Pin `zed_extension_api` git dep to a SHA at release time (iteration 81) |
| F-OPEN-005 | P2 | fixed | Call graph built outside the data lock (verified addressed, iteration 76) |
| F-OPEN-006 | P2 | documented | OAuth callback `std::sync::Mutex` correct-by-design + SAFETY comment (iterations 2, 76) |
| F-OPEN-007 | P2 | fixed | Daemon numeric params clamped (timeoutMs/minTokens/minSimilarity/topN); closed (iteration 84) |
| F-OPEN-008 | P2 | documented | GitHub release download integrity delegated to Zed TLS; SHA impractical in WASM (iteration 81) |
| F-OPEN-009 | P3 | deferred | Bulk graph-export streaming; endpoint capped at 50K, full streaming deferred (iterations 1, 53) |
| F-OPEN-011 | P2 | fixed | Atomic tempfile+fsync+rename for token cache writes (iteration 3) |
| F-OPEN-012 | P2 | fixed | `invalidate_cached_token` wired into bc_server 401/403 (iteration 3) |
| F-OPEN-013 | P3 | deferred | GitHub release SHA verify — defence-in-depth; Zed API pins TLS (iteration 53) |
| F-OPEN-014 | P2 | fixed | setBreakpoints holds breakpoints mutex across remove→add→store (iteration 4) |
| F-OPEN-015 | P3 | fixed | Per-target BC invoke timeouts (iteration 8) |
| F-OPEN-017 | P3 | fixed | SignalR general event channel bounded (4096-cap) (iteration 13) |
| F-OPEN-018 | P3 | fixed | NuGet metadata JSON capped at 16 MB w/ Content-Length (iteration 5) |
| F-OPEN-019 | P3 | fixed | Per-package-id download serialisation mutex (iteration 6) |
| F-OPEN-020 | P3 | fixed | `build_http_client` emits TLS-disabled warn parity (iteration 12) |
| F-OPEN-021 | P3 | documented | NuGet 401/403 → oauth invalidation theoretical (public feed unauthenticated) |
| F-OPEN-022 | P2 | fixed | `AlBridge.Init` captures `_lastInitError` (iteration 5) |
| F-OPEN-023 | P2 | fixed | `HandleRequest` throws on pre-init; `ping` reports `initialized` (iteration 5) |
| F-OPEN-024 | P3 | fixed | `.alformat.json` warns on unimplemented settings (iteration 7) |
| F-OPEN-025 | P3 | fixed | `extract_formatted_region` debug_assert on line alignment (iteration 14) |
| F-OPEN-026 | P3 | documented | Unterminated-string per-line scanning intentional/adversarial-only (iteration 12) |
| F-OPEN-027 | P3 | fixed | `MAX_CHAIN_NODES = 10_000` cap in recurse_event/subscriber (iteration 13) |
| F-OPEN-028 | P3 | documented | `discover_events` string cloning — low-ROI, cycles already small (iteration 53) |
| F-OPEN-029 | P2 | documented | Unbounded Value::Array etc. — verified no allocation surface exists (F-FP-005, iteration 9) |
| F-OPEN-030 | P3 | documented | `test_runtime` hardcoded builtins are runtime ABI, not AL language (iteration 53) |
| F-OPEN-031 | P3 | fixed | Recursion guard tightened `>` → `>=` (iteration 9) |
| F-OPEN-032 | P3 | fixed | thread-local LCG/LVS reset between tests (iteration 15) |
| F-OPEN-033 | P3 | fixed | Object-ID collision check before scaffold (iteration 10) |
| F-OPEN-034 | P3 | fixed | Scaffold `.al` writes via atomic_write (iteration 10) |
| F-OPEN-035 | P3 | fixed | 14 round-trip parse tests for generators (iteration 11) |
| F-OPEN-036 | P3 | wontfix | `writeln!(String)` `.unwrap()` cosmetic — infallible (iteration 53) |
| F-OPEN-037 | P3 | documented | `add_entries` clone path test-only (iteration 53) |
| F-OPEN-038 | P3 | fixed | Cycle-safety invariant documented on `composition::get_composed` (iteration 12) |
| F-OPEN-039 | P3 | documented | code_actions hardcoded AL property names are generated output, exempt (iteration 49) |
| F-OPEN-040 | P3 | fixed | `pick_active_signature` falls back to widest overload (iteration 14) |
| F-OPEN-041 | P3 | fixed | Legacy DAP-proxy no-cancel documented (iteration 15) |
| F-OPEN-044 | P3 | fixed | `read_json_body_capped` on BC dev API JSON parse sites (iteration 18) |
| F-OPEN-045 | P3 | fixed | `xlf_exceeds_cap` 64 MB pre-read check (iteration 18) |
| F-OPEN-047 | P3 | fixed | Test-only `current_dir().unwrap()` → `expect` (iteration 39) |
| F-OPEN-048 | P2 | fixed | Notification channel bounded at 10K w/ try_send (iteration 46) |
| F-OPEN-049 | P2 | fixed | `scopeguard_remove` two-stage cleanup (iteration 48) |
| F-OPEN-050 | P2 | fixed | `read_loop` warns on non-numeric response id (iteration 45) |
| F-OPEN-051 | P3 | documented | `Lifecycle` single-variant documented as historical (iteration 53) |
| F-OPEN-052 | P3 | fixed | `file_uri` debug_assert on non-UTF-8 paths (iteration 43) |
| F-OPEN-053 | P2 | fixed | `did_change` warns on backwards version delivery (iteration 48) |
| F-OPEN-054 | P2 | fixed | `apply_changes` + `get_text` in `did_change` not atomic; diag task can capture a version-skewed snapshot — added `DocumentStore::apply_changes_and_get` returning the post-change `(text, version)` under the same write lock, `did_change` now feeds that snapshot into `schedule_diagnostics` (closes the TOCTOU window vs a concurrent `did_change`); +2 tests (iteration 92) |
| F-OPEN-055 | P2 | documented | Concurrent init wastes cycles, DashMap-safe (F-FP-020, iteration 53) |
| F-OPEN-056 | P3 | deferred | `didChangeWatchedFiles` unimplemented; Zed rescans on focus (iteration 53) |
| F-OPEN-057 | P2 | fixed | `compile_project` canonicalises project_root (iteration 44) |
| F-OPEN-058 | P2 | fixed | alc `/out:` via per-build tmp dir + rename on success (iteration 55) |
| F-OPEN-059 | P2 | documented | `AL_TOOL_PATH` honoured without provenance — accepted risk (iteration 53) |
| F-OPEN-060 | P2 | wontfix | `config.rs::merge` path fields not canonicalised at boundary (per-consumer canonicalisation still pending) (iteration 92) — config path fields have zero consumers workspace-wide (parsed-but-unwired, config.rs:28-30); canonicalise-in-merge() is wrong (no project-root context, canonicalize() fails on not-yet-existing paths like packageCachePath). The build site already canonicalises at its boundary (F-FIX-079, build.rs:111). Each future consumer canonicalises where existence is meaningful. |
| F-OPEN-061 | P3 | fixed | `AlConfig::load` routes through merge() to report unknown keys (iteration 40) |
| F-OPEN-062 | P3 | fixed | Four config-merge sites push bad input into unknown_keys (iteration 41) |
| F-OPEN-063 | P3 | documented | Inconsistent `config.rs` `null` semantics — most fields can't be reset to default via `null` (iteration 95) — by-design, not a bug. `Option`-typed fields (`merge_optional_path`/`merge_optional_string`/`maxDocumentSizeBytes`) already reset to `None` on `null`. Scalar fields (bools/enums/arrays) intentionally treat `null`/missing as "keep current value" — the correct standard LSP `workspace/configuration` merge semantic (the client sends the full desired config, not deltas; there is no compile-time-default-via-null concept for required scalars). No panic, no silent corruption. No code change. |
| F-OPEN-064 | P3 | fixed | BC fallback `26.0.0.0` lifted to `CURRENT_BC_MAJOR_FALLBACK` const (iteration 34) |
| F-OPEN-066 | P2 | fixed | Selective graph invalidation on body-only edits (iteration 63) |
| F-OPEN-067 | P3 | documented | Daemon idle-timeout `try_lock` invariant documented (iteration 43) |
| F-OPEN-068 | P3 | fixed | `last_activity` → lock-free `AtomicU64` (iteration 42) |
| F-OPEN-069 | P3 | fixed | `dispatch_packages` poison-recovery via into_inner (iteration 47) |
| F-OPEN-070 | P3 | fixed | 10 s graceful in-flight drain on accept-loop break (iteration 47) |
| F-OPEN-071 | P3 | fixed | Workspace init notifies on partial package-load failure (iteration 43) |
| F-OPEN-072 | P1 | documented | Wedged CLR call: interrupting an in-process CLR call across the netcorehost FFI boundary is unsafe (thread-abort corrupts CLR state) — confirmed architectural limitation, the wedged thread is accepted to leak; the *recoverable* path is already mitigated — `SemanticBridge::call` cooldown + `try_lock` probe (`bridge.rs:281-338`) auto-recovers when the stuck call returns and prevents a thundering herd, and `restart_bridge` (`lifecycle.rs:320`) can build a fresh `DotNetHost`/Mutex so features resume; auto-wiring `restart_bridge` into the persistent-failure path (`diagnostics.rs:182-207`) is real design work (restart-counter interaction, thrash-vs-slow heuristics) warranting its own finding (iteration 89) |
| F-OPEN-073 | P3 | fixed | `parse_environment_type` logs ERROR + valid set (iteration 39) |
| F-OPEN-074 | P3 | fixed | `dev_packages_url` server field via `is_safe_http_server` allowlist (iteration 35) |
| F-OPEN-075 | P3 | fixed | Split `Poisoned` vs `Cooldown` SemanticError variants (iteration 37) |
| F-OPEN-076 | P3 | fixed | `sanitize_version` capped at 64 chars (iteration 36) |
| F-OPEN-077 | P3 | documented | `index_from_result` non-atomic split-state window documented (iteration 40) |
| F-OPEN-078 | P3 | documented | `file_index` per-keystroke allocs — low-ROI, hot path is parse (iteration 53) |
| F-OPEN-079 | P3 | fixed | `table_impact` drops lowercased alloc (iteration 34) |
| F-OPEN-080 | P2 | fixed | `add_edge` HashSet dedup O(1) (iteration 44) |
| F-OPEN-082 | P2 | documented | `RecordOp::from_method_name` tokens are record ABI (iteration 39) |
| F-OPEN-083 | P2 | documented | `record_op_event_names` pattern is record-runtime ABI (iteration 52) |
| F-OPEN-084 | P2 | fixed | Var-type-aware member-call resolution (iteration 56) |
| F-OPEN-085 | P2 | fixed | `generate_variants` made `pub(crate)` (iteration 39) |
| F-OPEN-086 | P3 | fixed | `extract_return_type` doc-vs-impl mismatch fixed (iteration 40) |
| F-OPEN-087 | P3 | fixed | `parse_run_trigger_arg` matches exact true/false (iteration 40) |
| F-OPEN-088 | P2 | fixed | `trace_from_node` pre-computes obj→event-index map (iteration 51) |
| F-OPEN-089 | P2 | fixed | recurse_event/subscriber children sorted (iteration 30) |
| F-OPEN-090 | P2 | fixed | `export_json` explicit error-log instead of silent default (iteration 45) |
| F-OPEN-091 | P3 | fixed | NodeInfo.node_type tags centralised into `node_kind` consts (iteration 50) |
| F-OPEN-092 | P3 | fixed | `apply_variant` snaps byte indices to char boundaries (iteration 45) |
| F-OPEN-093 | P1 | fixed | Interpreter cancel token threaded through DispatchCtx (iteration 54) |
| F-OPEN-094 | P3 | fixed | mutation collect_mutation_files reuses fetched parse (iteration 38) |
| F-OPEN-095 | P3 | fixed | `generate_variants` kept out of public API (iteration 39) |
| F-OPEN-096 | P1 | fixed | Same cancel token as F-OPEN-093 from eval_stmt side (iteration 54) |
| F-OPEN-097 | P1 | fixed | `eval_case` reads `else_body` field correctly (iteration 33) |
| F-OPEN-098 | P2 | fixed | mutate UTF-8 boundary clamp (same fix family as F-OPEN-092, iteration 45) |
| F-OPEN-099 | P2 | fixed | `values_equal_for_case` Integer↔Decimal lossless (iteration 33) |
| F-OPEN-100 | P2 | fixed | `eval_args_into` propagates `Eval::Exit` (iteration 33) |
| F-OPEN-101 | P2 | fixed | `extract_dataitem_symbol` inner-identifier selection range (iteration 64) |
| F-OPEN-102 | P2 | fixed | Dataitem body single-pass scan (iteration 70-71) |
| F-OPEN-103 | P3 | fixed | `is_variable_name_node` accepts any kw_* fallback (iteration 66-69) |
| F-OPEN-104 | P2 | fixed | `variables_at` amortises `find_source_table` (iteration 58) |
| F-OPEN-105 | P2 | fixed | `collect_dataitem_vars` early short-circuit (iteration 62) |
| F-OPEN-106 | P2 | documented | CRLF multi-line UTF-16 length confirmed correct (iteration 75) |
| F-OPEN-107 | P1 | fixed | `directive` recurses into children for highlighting (iteration 70-71) |
| F-OPEN-108 | P2 | fixed | semantic_tokens DFS O(n) via cursor children (iteration 60) |
| F-OPEN-109 | P3 | fixed | Duplicate trace! blocks collapsed (iteration 65) |
| F-OPEN-110 | P1 | deferred | Wire up remaining dormant FormatOptions (blank-lines, max-line-length, brace-style, sort-properties) — part 2; each is real feature work requiring structural support the line-based formatter lacks (safe wrapping/brace-moving/property-reorder needs AST-level transforms), warrants its own design + finding; values are parsed/stored and a per-field warn already tells users they're inert (iteration 89) |
| F-OPEN-111 | P1 | documented | Block-keyword text matches are AL Pascal-grammar terminals (iteration 66-69) |
| F-OPEN-112 | P1 | fixed | `format_range` last-line trailing-newline edge case fixed; unselected-line indent documented as by-design at the LSP boundary (iteration 89) |
| F-OPEN-113 | P2 | fixed | Multi-line paren-continuation idempotency tests added (iteration 66-69) |
| F-OPEN-114 | P1 | fixed | dead_code parsed_files sorted for determinism (iteration 72) |
| F-OPEN-115 | P1 | wontfix | Cross-object receiver-scoping needs var-type resolution (F-OPEN-084 territory), out of scope; dead partial infra removed (iteration 86) |
| F-OPEN-116 | P1 | fixed | `extract_text_call_names` tracks quote state for string literals (iteration 75) |
| F-OPEN-117 | P2 | fixed | `find_unused_fields` O(1) via workspace member-access set (iteration 73) |
| F-OPEN-118 | P2 | fixed | dead_code per-file scans parallelised via rayon (iteration 74) |
| F-OPEN-119 | P1 | fixed | BC server in-memory token reset on 401/403 (iteration 78) |
| F-OPEN-120 | P2 | fixed | `read_error_body_capped` 64 KiB cap (iteration 78) |
| F-OPEN-121 | P2 | fixed | `.app` filename path-traversal sanitisation (iteration 78) |
| F-OPEN-122 | P2 | fixed | Folding no duplicate object-body range (iteration 78) |
| F-OPEN-123 | P2 | fixed | `walk_al_files` uses cached DirEntry file_type (iteration 78) |
| F-OPEN-124 | P3 | fixed | `MAX_AL_FILE_BYTES = 50 MiB` per-file cap (iteration 78) |
| F-OPEN-125 | P3 | fixed | `walk_al_files` logs per-entry I/O errors (iteration 78) |
| F-OPEN-126 | P1 | fixed | `dap::config::parse_auth_method` env-type-aware fallback (iteration 79) |
| F-OPEN-127 | P2 | fixed | zed-al `set_nested_value` depth cap 64 (iteration 79) |
| F-OPEN-128 | P2 | deferred | `bc_client::apply_auth` lacks direct unit tests; blocked on serial-test dep (iteration 80) |
| F-OPEN-129 | P2 | fixed | BC server stale env-token override disabled on 401/403 (iteration 85) |
| F-OPEN-130 | P1 | fixed | `type_at`/`completions_at` reject unsaved-text buffers > 16 MiB before JSON serialization (`check_text_size`) (iteration 86) |
| F-OPEN-131 | P1 | fixed | `host::call` caps bridge `response_len` at 256 MiB before `from_raw_parts` (OOB-read guard) (iteration 86) |
| F-OPEN-132 | P2 | fixed | Removed dead `all_qualified_calls` set + `extract_qualified_call_pairs` (unused F-OPEN-115 partial infra) (iteration 86) |
| F-OPEN-133 | P1 | fixed | Direct unit tests for dead_code parsing helpers (extract_text_call_names/extract_member_access_names/split_args/parse_subscriber_args) (iteration 86) |
| F-OPEN-134 | P3 | fixed | Tests for `extract_field_name_from_args` unclosed-quote + quoted-name cases (iteration 86) |
| F-OPEN-136 | P2 | fixed | No regression tests for `signalr_to_bc_event` conversion (private fn, Value-shape dependent) (iteration 92) — added 7 tests covering Break→breakpoint/thread-1, Detached terminate true/false/missing-default-false, internal IsAlive/OnAttachedToConnection dropped, unknown→Other preserved, and missing-target dropped; conversion logic unchanged (the F-OPEN-016 protocol-version pass remains separate) |
| F-OPEN-138 | P3 | fixed | `OnFatalDebuggerException` reports informative message (absent/empty/non-string args distinguished) via shared `fatal_exception_message()` — both call sites; +5 tests (iteration 87) |
| F-OPEN-139 | P2 | fixed | `configuration_done()` propagates the second `DebugAdapterConfigurationDone` failure instead of masking it with `Ok(())` (iteration 87) |
| F-OPEN-140 | P2 | fixed | SignalR negotiate parsing accepts `connectionId`/`ConnectionId`/`connection_id` and warns instead of silently using the auth token as session id (iteration 87) |
| F-OPEN-141 | P2 | fixed | `DocumentStore::close()` evicts the per-URI `parse_locks` entry — stops the lock map leaking over a long-running daemon's lifetime; +1 test (iteration 87) |
| F-OPEN-142 | P1 | fixed | `get_or_parse` TOCTOU: atomic `get_text_and_version` + `get_cached_tree_at_version` (matches live AND captured version) prevent serving a new tree with old text; +1 test (iteration 88) |
| F-OPEN-143 | P1 | fixed | `find_workspace_field` reports UTF-16 columns via `byte_col_to_utf16_col` instead of byte offsets (correct hover/goto/rename for non-ASCII field names) (iteration 88) |
| F-OPEN-144 | P2 | fixed | Added regression tests for `find_workspace_field`/`workspace_field_items` covering ASCII + leading/mid-name multibyte field names (iteration 88) |
| F-OPEN-145 | P2 | fixed | `format_xml_doc` `<param>` extraction capped at 256 to bound O(params·doc_len) on malformed docs; +2 tests (iteration 88) |
| F-OPEN-146 | P2 | fixed | `compose()` deduplicates fields by `(id, lowercased name)` to defend against malformed symbol index; warns on removal; +2 tests (iteration 88) |
| F-OPEN-147 | P0 | fixed | `profiling::stop_profiling` validates `output_dir.is_absolute()` up front (before the network round-trip) — closes a path-traversal hole where a relative path escaped via the daemon cwd; uses the previously-dead `RelativeOutputDir` variant; mirrors `snapshot.rs`; +1 test (iteration 90) |
| F-OPEN-148 | P2 | fixed | `insight::graph::build_from_index` now extracts the `TableRelation` target via `analysis::extract_table_relation_table` instead of naive `trim_matches('"')`, so `WHERE`/`FIELD`/`IF` clauses and dotted refs no longer poison the node key and the `RelatesTo` edge is created for real relations; +1 test (also closes the paired test-gap) (iteration 90) |
| F-OPEN-149 | P1 | fixed | `file_index::incremental_scan` evicts (and reports in `ScanDelta.removed`) a previously-indexed file that has grown past `MAX_AL_FILE_BYTES`, instead of leaving stale content/parse-tree/object mappings in the index forever; +1 test (also closes the size-cap-transition test-gap) (iteration 90) |
| F-OPEN-150 | P2 | fixed | `file_index::remove_procedures_for_file` uses the DashMap `Entry` API to hold the shard lock across retain-then-maybe-remove, closing a race where a concurrent `index_from_result` insert was wiped, causing intermittent go-to-definition misses (iteration 90) |
| F-OPEN-151 | P1 | fixed | `syntax::context::extract_last_identifier` walked bytes and cast each to `char`, misclassifying UTF-8 continuation bytes and truncating multi-byte identifiers ("Café"->"", "Mañana"->"ana"); now iterates via `char_indices()`. `find_call_context` fixed transitively; +2 tests (iteration 91) |
| F-OPEN-152 | P2 | fixed | Added Unicode-identifier regression tests for `extract_last_identifier` and `find_call_context` (Café/Mañana/Città/München); covered the test-gap left by the byte-cast bug (iteration 91) |
| F-OPEN-153 | P2 | fixed | Inlay-hints range filter used `node_end < range.start.line` but tree-sitter's `end_position().row` is exclusive; switched both walker sites to `<=` so subtrees ending on the row before the range are skipped up front; +1 test (iteration 91) |
| F-OPEN-154 | P3 | fixed | `lint::lint_config_has_default` test now calls `LintConfig::default()` to match its documented intent instead of bare unit-struct instantiation (iteration 91) |
| F-OPEN-155 | P1 | fixed | `dispatch_inlay_hints` cast `startLine`/`endLine` via `as u32` without bounds checking — a client value > `u32::MAX` silently wrapped to a nonsensical line number. Now mirrors `extract_position`: out-of-range returns `INVALID_PARAMS`, absent keeps the documented 0/`u32::MAX` default (iteration 93) |
| F-OPEN-156 | P1 | fixed | `dispatch_generate` cast the object `id` via `as i32` without bounds checking — an out-of-range id wrapped to a different in-range id (e.g. `i32::MAX+1`→`i32::MIN`), bypassing the object-ID conflict check against the truncated value. Now uses the `extract_i32` helper, returning `INVALID_PARAMS` on overflow; absent `id` keeps the 50100 default (iteration 93) |
| F-OPEN-157 | P2 | fixed | Added overflow-rejection + absent-default regression tests for `dispatch_inlay_hints` line params, matching `extract_position_rejects_overflow` (closes the paired test-gap for F-OPEN-155) (iteration 93) |
| F-OPEN-158 | P2 | fixed | Added overflow-rejection + absent-default regression tests for `dispatch_generate`'s object id, proving the truncated-id conflict-check bypass cannot recur (closes the paired test-gap for F-OPEN-156) (iteration 93) |
| F-OPEN-159 | P2 | fixed | `apply_keyword_casing` treated the first quote of an AL `''` escape as a string terminator, desyncing the scanner so a following keyword could be mis-cased/skipped; now mirrors `count_net_delimiters`'s `''`-as-content handling; +2 tests (closes the paired test-gap) (iteration 93) |
| F-OPEN-160 | P2 | fixed | `compile_project` returned `AlError::BuildTimeout(0)` for a missing `.app` file name (surfacing as "alc compile timed out after 0 seconds"); now returns an `Io`(`InvalidInput`) error describing the real path failure (iteration 93) |
| F-OPEN-161 | P2 | fixed | `SemanticBridge::analyze()` serialised the caller-supplied `source` into the CLR call without the `check_text_size` guard `type_at`/`completions_at` apply (diagnostics.rs feeds unsanitised editor text); added the guard; +1 test (closes the paired test-gap) (iteration 93) |
| F-OPEN-162 | P2 | fixed | Daemon parse-error context lost on response-write failure (`server/daemon/mod.rs`) — a malformed JSON-RPC line built a `PARSE_ERROR` reply written with `?`, so a broken-pipe I/O failure surfaced only as a generic "connection error" and the malformed-JSON reason vanished. Now logs the parse error before the write and handles write/flush failure explicitly (logs + breaks) so the diagnostic survives either path (iteration 95) |
| F-OPEN-163 | P3 | fixed | Integer truncation in `find_member_line_in_file` (`al-explorer/src/main.rs`) — `i as u32` wrapped modulo 2^32 for a >4-billion-line file, returning a bogus line number; changed to `u32::try_from(i).unwrap_or(u32::MAX)` to saturate (iteration 95) |
| F-OPEN-164 | P2 | fixed | `read_bounded_line`/`read_response` (`al-protocol/src/client.rs`) test-gap — added 3 regression tests: incomplete UTF-8 at EOF → `InvalidData`, incomplete UTF-8 before newline → `InvalidData`, bare blank line yields empty `String` + graceful client parse error (never panic), documenting the daemon-skips/client-trusts asymmetry (iteration 95) |
| F-OPEN-165 | P2 | wontfix | "notify_sink fire-and-forget task silently loses errors" (`server/lsp.rs:68`) — FALSE POSITIVE. `tower_lsp::Client::show_message` returns `()`, not a `Result`; it is a fire-and-forget notification with no error value to capture or log. The spawned task already does the only thing the API permits. No code change (iteration 95) |
| F-OPEN-135 | P2 | fixed | No test for the timeout-cooldown `try_lock` race (T047); needs a wedge-able bridge seam to test deterministically — extracted the cooldown-gate decision out of `SemanticBridge::call` into a pure free fn `cooldown_gate<T>(&AtomicU64, &Mutex<T>, now, cooldown, method)` generic over the locked type, so the `try_lock()` race is exercised with a plain `Mutex<()>` (free / held / poisoned) without loading the CLR; +6 tests covering no-prior-timeout, in-window short-circuit (no probe), elapsed+free resume/clear, elapsed+held extend, poisoned extend, and full held-then-recover sequence (iteration 92) |
