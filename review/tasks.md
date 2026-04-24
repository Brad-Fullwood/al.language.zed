# Codebase Audit — Task List

Generated from full codebase audit on 2026-03-31 (commit c086d03).
Organized by priority tier. Each task is scoped for a single agent session.

---

## Tier 0: Critical Architecture (do first — unblocks everything)

### T-001: Remove `tower-lsp` from al-core and al-syntax
**Priority:** CRITICAL | **Effort:** Large (2-3 sessions) | **Crates:** al-core, al-syntax, al-lsp
**Description:**
Remove `tower-lsp` from `al-core/Cargo.toml` and `al-syntax/Cargo.toml`. Replace all LSP wire type usage with the transport-agnostic types already defined in `al-core/src/queries/mod.rs`. Move all `From<tower_lsp::…>` impls from `queries/mod.rs` to `al-lsp`.

Key files to modify:
- `al-core/Cargo.toml` — remove tower-lsp dependency
- `al-core/src/resolution.rs` — replace all `tower_lsp::lsp_types` with `queries::mod` types
- `al-core/src/queries/inlay_hints.rs` — use `AlInlayHint`, `Position`, `Range`
- `al-core/src/queries/symbols.rs` — return `Vec<AlDocumentSymbol>` not `DocumentSymbolResponse`
- `al-core/src/queries/folding.rs` — return `Vec<AlFoldingRange>`
- `al-core/src/queries/search.rs` — use `AlSymbolKind` and `queries::Range`
- `al-core/src/queries/code_actions.rs` — use `queries::Range` in private helpers
- `al-core/src/file_index.rs` — `CachedProcedureInfo::selection_range` → `queries::Range`
- `al-core/src/queries/mod.rs` — move `From<tower_lsp>` impls to al-lsp
- `al-syntax/Cargo.toml` — remove tower-lsp dependency
- `al-syntax/src/formatting.rs` — `format_range` returns custom `TextEdit`
- `al-syntax/src/navigation.rs`, `symbols.rs`, `context.rs`, `folding.rs`, `type_resolver.rs` — replace lsp_types
- `al-lsp/src/server.rs` — add conversion at the boundary
- All 8+ query files that convert `Position` to LSP on entry

**Verification:** `cargo check --workspace --exclude zed-al` passes with no `tower-lsp` in al-core or al-syntax deps.

---

### T-002: Fix UTF-16 position handling
**Priority:** CRITICAL | **Effort:** Medium | **Crates:** al-syntax, al-core
**Description:**
Fix all locations where tree-sitter byte columns are used as UTF-16 character positions:
- `al-syntax/src/navigation.rs:11` — `find_node_at_position` uses `pos.character` as byte column
- `al-syntax/src/type_resolver.rs:198` — `find_enclosing_procedure` same issue
- `al-core/src/queries/inlay_hints.rs:83,541,597` — raw `start_position().column` used as UTF-16

Both `find_node_at_position` and `find_enclosing_procedure` need source text access to perform UTF-16↔byte conversion.

**Verification:** Write test with non-ASCII identifiers (e.g., `Ñame`, `Müller`) verifying correct hover/completion positions.

---

## Tier 1: Critical Safety (deadlocks, panics, data corruption)

### T-003: Fix mutex deadlocks in al-dap-client
**Priority:** CRITICAL | **Effort:** Medium | **Crates:** al-dap-client
**Description:**
- `native_dap.rs:570-647` — `setBreakpoints` handler holds session mutex across `.await`
- `native_dap.rs:436-448` — background event task holds session mutex across `.await`
- `native_dap.rs:432` — background task leaks on reconnect (no cancellation)

Fix pattern: clone session data, drop lock, then await. Add `CancellationToken` for background task lifecycle.

**Verification:** Breakpoint set/clear works without hanging. Launch→disconnect→re-launch doesn't produce duplicate events.

---

### T-004: Convert recursive tree-sitter traversals to iterative
**Priority:** HIGH | **Effort:** Medium-Large | **Crates:** al-core, al-syntax
**Description:**
Convert ALL recursive traversals to iterative with explicit `Vec<Node>` stack.
**12+ instances found** (initial per-crate review found 5; quality audit found 7 more):

al-core (10 instances):
- `queries/dead_code.rs:213-240` — `collect_procedures`
- `queries/dead_code.rs:396-426` — `collect_event_subscribers`
- `queries/inlay_hints.rs:61-116` — `collect_inlay_hints`
- `queries/inlay_hints.rs:515-574` — `collect_return_type_hints`
- `queries/obsolescence.rs:192-193` — `scan_procedures_for_obsolete` (NEW)
- `insight/calls.rs:114-128` — `find_procedure_in_node` (NEW)
- `insight/calls.rs:632-641` — `count_call_suffixes` (NEW)
- `insight/calls.rs:739-757` — `collect_methods_recursive` (NEW)
- `queries/code_actions.rs:1132` — recursive child iteration (NEW)
- `queries/sql_patterns.rs:97` — recursive child iteration (NEW)
- `queries/profiler_hints.rs:297,388` — 2 recursive functions (NEW)

al-syntax (1 instance):
- `symbols.rs:943-963` — `collect_variable_name_nodes`

Also extend `walk_tree_until` in `al-syntax/traversal.rs` to support `TraversalAction { Continue, Skip, Stop }` — needed by 5+ callsites that use subtree skipping (`did_visit = true`).

**Verification:** Run existing tests. Test with deeply nested AL fixture (100+ nesting levels).

---

### T-005: Fix blocking I/O in async contexts (al-lsp)
**Priority:** CRITICAL | **Effort:** Medium | **Crates:** al-lsp
**Description:**
Replace `std::fs` calls with `tokio::fs` in async functions:
- `workspace.rs:700,731,742,745` — `initialize_workspace` settings helpers
- `daemon/build_dispatch.rs:1439,1488,1522,1578,2300,2367` — XLF, sort, organize file ops
- `server.rs:953-969` — `al.reindex` should spawn background task (not block handler)
- `daemon/mod.rs:630-639` — `ensure_document` needs async or spawn_blocking
- `daemon/build_dispatch.rs:986-1060` — replace `block_in_place(block_on(...))` with async

**Verification:** `cargo clippy` passes. LSP remains responsive during reindex.

---

### T-006: Fix panics in library code
**Priority:** HIGH | **Effort:** Small | **Crates:** al-symbols, al-core, al-syntax
**Description:**
Replace `.unwrap()` and `.expect()` in non-test code:
- `al-symbols/src/oauth.rs:511` — `getrandom::expect()` → propagate Result
- `al-symbols/src/language_data.rs:33,41` — LazyLock `expect()` → add test validating JSON
- `al-core/src/parsing.rs:25` — `unwrap()` after is_none check → use `let Some`
- `al-core/src/queries/code_actions.rs:594` — `common_var.unwrap()` → use `?`
- `al-core/src/queries/code_actions.rs:1439` — `chars().next().unwrap()` → use `if let`
- `al-core/src/queries/audit.rs:124` — `stack.pop().unwrap()` → use `if let`
- `al-syntax/src/type_resolver.rs:750-751` — `.unwrap()` → `let Some` guards

**Verification:** `cargo clippy` passes. No `unwrap()` grep matches in non-test `.rs` files (excluding known-safe patterns).

---

### T-007: Fix al-semantic timeout/poison behavior
**Priority:** HIGH | **Effort:** Small | **Crates:** al-semantic
**Description:**
After a timeout fires, mark the bridge as poisoned. All future `call()` invocations should return `SemanticError::Poisoned` immediately instead of blocking on the mutex forever.

**Verification:** Test that after a simulated timeout, subsequent calls return `Poisoned` immediately.

---

## Tier 2: Hardcoded AL Values (CLAUDE.md violations)

### T-008: Remove hardcoded AL values from al-syntax
**Priority:** HIGH | **Effort:** Medium | **Crates:** al-syntax
**Description:**
- `type_resolver.rs:63-68` — `object_kind_to_al_type` hardcodes "table"→"Record" etc. Add `runtime_type` to `object_types.json`
- `type_resolver.rs:532` — `kw_table`/`kw_tableextension` node kinds hardcoded
- `sort.rs:63-88,214-225` — procedure modifier prefixes hardcoded. Add to LanguageData
- `symbols.rs:323-333` — section keyword→SymbolKind mapping. Add to data file
- `symbols.rs:1006` — `"Label "` type keyword hardcoded
- `symbols.rs:976` — `"kw_function"` node kind hardcoded

**Verification:** `cargo test -p al-syntax` passes. Grep for hardcoded AL values returns clean.

---

### T-009: Remove hardcoded AL values from al-core
**Priority:** HIGH | **Effort:** Small | **Crates:** al-core
**Description:**
- `queries/audit.rs:65-69` — `"table" | "tableextension"` string match
- `queries/dead_code.rs:100` — `obj_kind_lower == "table"`
- Related: `file_index.rs` `CachedObjectInfo::kind` should use `ObjectKind` enum (enables exhaustive matching)

**Verification:** No hardcoded object type strings in al-core. `cargo test` passes.

---

## Tier 3: Performance

### T-010: Fix code lens O(N²) reference counting
**Priority:** HIGH | **Effort:** Small | **Crates:** al-core
**Description:**
`code_lens.rs:86-131` — `count_references_by_name()` scans ALL workspace files per procedure. Build a `HashMap<String, usize>` in a single pass over workspace files, then look up each procedure.

**Verification:** Code lens on a 30-procedure codeunit completes in <500ms on a 200-file project.

---

### T-011: Fix `get_cached_parse` String cloning
**Priority:** HIGH | **Effort:** Medium | **Crates:** al-core
**Description:**
`file_index.rs:141-145` — deep-clones file content on every call (20+ callsites). Store files as `Arc<String>` in `FileIndex::files`. Change `get_cached_parse` to return `Arc<String>`.

**Verification:** References/rename on a large workspace shows reduced allocation pressure.

---

### T-012: Fix startup re-parsing
**Priority:** HIGH | **Effort:** Small | **Crates:** al-lsp
**Description:**
`workspace.rs:200-235` — project diagnostics re-parses every file at startup instead of using cached trees from `file_index.get_cached_parse()`.

**Verification:** Startup time improves ~200ms-2s on 200-file projects.

---

### T-013: Fix O(N²) line lookup in al-syntax tokens
**Priority:** HIGH | **Effort:** Small | **Crates:** al-syntax
**Description:**
`tokens.rs:207,227,257,272` — `source.split(|&b| b == b'\n').nth(row)` called per token. Pre-build `Vec<usize>` of line start offsets.

**Verification:** `collect_tokens` on a 2000-line file completes 10x faster.

---

### T-014: Cache NuGet service index per feed
**Priority:** MEDIUM | **Effort:** Small | **Crates:** al-symbols
**Description:**
`nuget.rs:213` — `get_package_base_address` fetched per package. Cache per `feed.index_url`.

**Verification:** Initial download makes 1 service index request per feed, not per package.

---

## Tier 4: HTTP/Network Safety

### T-015: Add HTTP timeouts and streaming size limits
**Priority:** HIGH | **Effort:** Small | **Crates:** al-symbols
**Description:**
- `nuget.rs:163` — `reqwest::Client::new()` has no timeout. Add 300s timeout matching bc_server.rs
- `nuget.rs:220` — check HTTP status before JSON parse on version index
- `nuget.rs:281-293`, `bc_server.rs:118-130` — stream response with byte counting instead of buffering entire body
- `nuget.rs:382-396` — write to temp file, rename on success (atomic extraction)

**Verification:** NuGet download with mock 404 returns clear error. Oversized response rejected without OOM.

---

### T-016: Fix OAuth device code polling
**Priority:** MEDIUM | **Effort:** Small | **Crates:** al-symbols
**Description:**
`oauth.rs:408-450` — handle HTTP 429 with `Retry-After` header. Current code aborts on unexpected status.

**Verification:** Mock 429 response causes retry with backoff.

---

## Tier 5: DAP Protocol Correctness

### T-017: Fix DAP protocol issues
**Priority:** HIGH | **Effort:** Medium | **Crates:** al-dap-client
**Description:**
- `native_dap.rs:102,434` — unify seq counter (non-monotonic seq violates spec)
- `native_dap.rs:1034` — `"requestSeq"` should be `"request_seq"` per DAP spec
- `bc_debug.rs:131` — `launchBrowser`/`validateServerCertificate` need string→bool coercion
- `native_dap.rs:517` — on-prem browser URL omits port
- `bc_debug.rs:162-185` — URL-encode `tenant` and `environment_name`

**Verification:** DAP message log shows monotonic seq. On-prem launch opens correct URL with port.

---

## Tier 6: Test Quality

### T-018: Add `#[ignore]` to live tests in al-zed-test
**Priority:** HIGH | **Effort:** Trivial | **Crates:** al-zed-test
**Description:**
`tests/live_test.rs` lines 93, 110, 135, 158, 182, 226, 247 — all require running Zed but lack `#[ignore]`. Will fail in CI.

**Verification:** `cargo test -p al-zed-test` passes without a running Zed instance.

---

### T-019: Add negative tests
**Priority:** MEDIUM | **Effort:** Medium | **Crates:** al-zed-test, al-test-harness
**Description:**
- al-zed-test: no negative tests at all (test quality gate violation)
- al-test-harness/tests/e2e.rs: entirely happy-path
- al-symbols/virtual_file.rs: untested
- al-symbols/bc_server.rs: trivially thin tests
- al-dap-client/config.rs: zero tests

**Verification:** Each test file has at least one `test_*_invalid_*` or `test_*_error_*` test.

---

### T-020: Fix contradictory test assertions
**Priority:** MEDIUM | **Effort:** Small | **Crates:** al-test-harness
**Description:**
- `edit_lifecycle.rs:736` asserts hover on Unicode identifier `is_some()`
- `regression.rs:259` expects hover on multibyte char returns `None` (ISSUE-024)
- `e2e.rs:180` uses wrong line/column for hover_on_parameter
- `zed_fidelity.rs:394` captures hover result but never asserts on it
- `transport.rs:326` has inverted assert message

**Verification:** No contradictory assertions. Each test verifies correctness, not just existence.

---

## Tier 7: Code Quality & Cleanup

### T-021: Fix TOCTOU races in al-symbols
**Priority:** MEDIUM | **Effort:** Small | **Crates:** al-symbols
**Description:**
- `virtual_file.rs:32-40` — `exists()` then `write()` race. Use `OpenOptions::create_new(true)`
- `source_index.rs:113-127` — double-build race on concurrent `get_or_build`. Use double-checked locking
- `nuget.rs:382-396` — partial .app file left on disk after extraction failure. Write to temp, rename on success

**Verification:** No partial files on interrupted extraction. No duplicate builds in concurrent access.

---

### T-022: Fix visibility (pub vs pub(crate))
**Priority:** LOW | **Effort:** Small | **Crates:** al-symbols, al-lsp, al-core
**Description:**
Multiple functions marked `pub` that should be `pub(crate)`:
- al-symbols: `token_cache_path`, `AppSourceIndex`, `parse_quoted_ident`, `is_ident_start/char`
- al-lsp: `syntax_error_to_diagnostic`, `lint_to_diagnostic`, `run_dap_server/proxy`, `cleanup_socket`
- al-core: transport-agnostic types not re-exported at crate root

**Verification:** `cargo check` passes. No external crate uses the tightened functions.

---

### T-023: Fix concurrency issues
**Priority:** MEDIUM | **Effort:** Small | **Crates:** al-core, al-daemon-client
**Description:**
- `al-core/workspace.rs:128-142` — `get_or_build_insight_graph` lacks double-checked locking (two threads build simultaneously)
- `al-daemon-client/src/socket.rs:75,93,99` — `set_var`/`remove_var` in tests is thread-unsafe (Rust 2024 edition breakage). Refactor `socket_path` to accept runtime dir as parameter
- `al-lsp/workspace.rs:207-236` — config lock held across hundreds of file iterations

**Verification:** Tests pass with `--test-threads=4`. No duplicate graph builds under concurrent requests.

---

### T-024: Eliminate duplicate code
**Priority:** MEDIUM | **Effort:** Medium | **Crates:** al-core, al-syntax, al-lsp
**Description:**
Major duplications found by reuse and quality audits:

**Cross-file patterns (HIGH impact):**
- Workspace file iteration boilerplate repeated 25+ times → add `FileIndex::for_each_parsed_file()`
- `scope_label` in queries/mod.rs is exact duplicate of `VariableScope::Display` → delete, use `.to_string()`
- `has_local_modifier` has 3 different implementations → consolidate in al-syntax
- Quote-aware arg splitting has 4 implementations → promote best one to queries/mod.rs
- `did_visit` cursor loop reimplemented 5× → extend `walk_tree` API (see T-004)

**Within-crate duplications:**
- `al-core/dead_code.rs` and `audit.rs` — duplicate field extraction logic
- `al-syntax/tokens.rs:195-293` — token emission logic duplicated verbatim (50 lines)
- `al-lsp/diagnostics.rs` — Phase 1 diagnostics block duplicated in compute and publish paths
- `al-lsp/build_dispatch.rs` — `try_read ERR_INITIALIZING` boilerplate repeated 11 times
- JSONC comment stripper duplicated between al-lsp and al-dap-client
- Preceding-attribute sibling walker duplicated in dead_code.rs and obsolescence.rs

**Verification:** No near-duplicate code blocks >10 lines.

---

### T-025: Fix merge_json_owned in zed-al
**Priority:** MEDIUM | **Effort:** Small | **Crates:** zed-al (root src/)
**Description:**
- `src/lib.rs:56` — `key_stack` variable declared but never used (dead code)
- `src/lib.rs:62-118` — `AssembleObject` pushed before `Merge` items it depends on (stack ordering may be wrong for nested merges)
- Add unit tests for `merge_json`, `apply_al_settings_to_config`, `detect_platform`

**Verification:** `merge_json` produces correct output for nested object merges. All new tests pass.

---

### T-026: Fix password/secret handling
**Priority:** MEDIUM | **Effort:** Small | **Crates:** al-cli, al-symbols
**Description:**
- `al-cli` — `--password` flag value logged by daemon at DEBUG level
- `al-symbols/oauth.rs:713-760` — `save_cached_token` silently drops write failures (refresh token lost)
- `al-dap-client/bc_debug.rs` — `BcDebugConfig::password` is a plain String with no Debug redaction

**Verification:** DEBUG logs contain no plaintext passwords. Token save failure surfaces to user.

---

### T-027: Add source index eviction
**Priority:** LOW | **Effort:** Small | **Crates:** al-symbols
**Description:**
`source_index.rs:22` — `SOURCE_INDEX_CACHE` is unbounded. Add `clear_source_index_cache()` or LRU eviction.

**Verification:** Cache doesn't grow unbounded across project switches.

---

### T-028: Fix XML attribute unescaping
**Priority:** LOW | **Effort:** Trivial | **Crates:** al-symbols
**Description:**
`manifest.rs:57-69` — uses `std::str::from_utf8(&attr.value)` instead of `attr.unescape_value()`. Attributes with XML entities (`&amp;`, `&lt;`) return raw escaped text.

**Verification:** Manifest with `Name="Test &amp; More"` parses as `"Test & More"`.

---

### T-029: Fix NuGet version sorting
**Priority:** LOW | **Effort:** Small | **Crates:** al-symbols
**Description:**
`nuget.rs:310-317` — `version_prefix` uses string prefix matching and lexicographic sort. Parse version components as integers for correct semver comparison.

**Verification:** Version `"2.0.12345.0"` sorts higher than `"2.0.999.0"`.

---

### T-030: Extract business logic from build_dispatch.rs to al-core
**Priority:** MEDIUM | **Effort:** Large | **Crates:** al-lsp, al-core
**Description:**
`al-lsp/src/daemon/build_dispatch.rs` is 2,420 lines containing significant business logic (XLIFF manipulation, NuGet downloads, test runner invocation, snapshot/profiling coordination). CLAUDE.md: "al-lsp must not contain business logic."

Move dispatch logic into al-core modules: `build.rs`, `xliff.rs`, `generators.rs`, etc. Dispatch functions become 10-15 line wrappers that deserialize params, call al-core, serialize result.

**Verification:** `build_dispatch.rs` < 500 lines. All dispatchers are thin wrappers.

---

### T-031: Cache document symbols per file
**Priority:** MEDIUM | **Effort:** Small | **Crates:** al-core
**Description:**
`extract_document_symbols` is re-run on every hover/completion/signature/code-lens request. Cache alongside parse tree in FileIndex, invalidate on content change.

**Verification:** `extract_document_symbols` called once per file change, not per request.

---

### T-032: Add `#[must_use]` to pure query functions
**Priority:** LOW | **Effort:** Trivial | **Crates:** al-core, al-syntax
**Description:**
All pure query functions (hover, completions, definition, references, format_al, etc.) should have `#[must_use]` to catch accidental result discarding.

**Verification:** Accidentally ignoring a query return value produces a compiler warning.

---
