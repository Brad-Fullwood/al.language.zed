# Findings Ledger (authoritative open-issue tracker)

Single source of truth for F-OPEN status. The perfect-loop workflow reads the `open` rows here
instead of re-parsing FINDINGS.md prose. Update this file whenever a finding's status changes.
FINDINGS.md remains the narrative history; this is the index.

## Open (actionable)

| ID | Severity | Title |
|---|---|---|
| F-OPEN-010 | P2 | Token zeroization — `access_token`/`refresh_token` are plain `String`s in a long-lived daemon; adopt `zeroize` |
| F-OPEN-016 | P3 | Hardcoded BC protocol version assumptions in `bc_debug.rs`; needs version-detection / capability probe |
| F-OPEN-042 | P3 | No per-document size cap — a 10 GB open file consumes memory unbounded |
| F-OPEN-043 | P3 | Tree-sitter parse-tree cache has no eviction (LRU / idle sweep) |
| F-OPEN-046 | P3 | TUI daemon-socket reads have no timeout (only Ctrl+C escapes a stuck query) |
| F-OPEN-054 | P2 | `apply_changes` + `get_text` in `did_change` not atomic; diag task can capture a version-skewed snapshot |
| F-OPEN-060 | P2 | `config.rs::merge` path fields not canonicalised at boundary (per-consumer canonicalisation still pending) |
| F-OPEN-063 | P3 | Inconsistent `config.rs` `null` semantics — most fields can't be reset to default via `null` |
| F-OPEN-065 | P2 | No daemon `$/cancelRequest` support; abandoned long-running endpoints still pay full cost / pin slots |
| F-OPEN-081 | P3 | `InsightGraph` public API leaks `petgraph::NodeIndex`; wrap in a newtype to allow backend swap |
| F-OPEN-135 | P2 | No test for the timeout-cooldown `try_lock` race (T047); needs a wedge-able bridge seam to test deterministically |
| F-OPEN-136 | P2 | No regression tests for `signalr_to_bc_event` conversion (private fn, Value-shape dependent) |
| F-OPEN-137 | P1 | Hardcoded SignalR protocol version (`negotiateVersion=1`, `version:1`) lacks negotiate-response validation — bundle with F-OPEN-016 |

## Resolved / parked (not actionable — do not re-open)

| ID | Severity | Status | Title |
|---|---|---|---|
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
| F-OPEN-055 | P2 | documented | Concurrent init wastes cycles, DashMap-safe (F-FP-020, iteration 53) |
| F-OPEN-056 | P3 | deferred | `didChangeWatchedFiles` unimplemented; Zed rescans on focus (iteration 53) |
| F-OPEN-057 | P2 | fixed | `compile_project` canonicalises project_root (iteration 44) |
| F-OPEN-058 | P2 | fixed | alc `/out:` via per-build tmp dir + rename on success (iteration 55) |
| F-OPEN-059 | P2 | documented | `AL_TOOL_PATH` honoured without provenance — accepted risk (iteration 53) |
| F-OPEN-061 | P3 | fixed | `AlConfig::load` routes through merge() to report unknown keys (iteration 40) |
| F-OPEN-062 | P3 | fixed | Four config-merge sites push bad input into unknown_keys (iteration 41) |
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
| F-OPEN-138 | P3 | fixed | `OnFatalDebuggerException` reports informative message (absent/empty/non-string args distinguished) via shared `fatal_exception_message()` — both call sites; +5 tests (iteration 87) |
| F-OPEN-139 | P2 | fixed | `configuration_done()` propagates the second `DebugAdapterConfigurationDone` failure instead of masking it with `Ok(())` (iteration 87) |
| F-OPEN-140 | P2 | fixed | SignalR negotiate parsing accepts `connectionId`/`ConnectionId`/`connection_id` and warns instead of silently using the auth token as session id (iteration 87) |
| F-OPEN-141 | P2 | fixed | `DocumentStore::close()` evicts the per-URI `parse_locks` entry — stops the lock map leaking over a long-running daemon's lifetime; +1 test (iteration 87) |
| F-OPEN-142 | P1 | fixed | `get_or_parse` TOCTOU: atomic `get_text_and_version` + `get_cached_tree_at_version` (matches live AND captured version) prevent serving a new tree with old text; +1 test (iteration 88) |
| F-OPEN-143 | P1 | fixed | `find_workspace_field` reports UTF-16 columns via `byte_col_to_utf16_col` instead of byte offsets (correct hover/goto/rename for non-ASCII field names) (iteration 88) |
| F-OPEN-144 | P2 | fixed | Added regression tests for `find_workspace_field`/`workspace_field_items` covering ASCII + leading/mid-name multibyte field names (iteration 88) |
| F-OPEN-145 | P2 | fixed | `format_xml_doc` `<param>` extraction capped at 256 to bound O(params·doc_len) on malformed docs; +2 tests (iteration 88) |
| F-OPEN-146 | P2 | fixed | `compose()` deduplicates fields by `(id, lowercased name)` to defend against malformed symbol index; warns on removal; +2 tests (iteration 88) |
