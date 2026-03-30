# Test Quality — Missing Assertions, Flaky Tests, Always-Pass Tests

## HIGH

### TQ-H01: al-test-harness — `semantic_token_data` panics on non-multiple-of-5 arrays
- **File:** `crates/al-test-harness/src/protocol.rs:41-53`
- **Impact:** `chunks(5)` last chunk can be 1-4 elements. `chunk[4]` index-out-of-bounds panic instead of useful assertion.
- **Fix:** `.filter(|chunk| chunk.len() == 5)` or assert `arr.len() % 5 == 0` at call sites.

### TQ-H02: al-zed-test — `live_test.rs` tests use early `return` instead of `#[ignore]`
- **File:** `crates/al-zed-test/tests/live_test.rs:112-127, 157-165, 181-184`
- **Impact:** Tests that can't run register as PASSED, not skipped. Same bug that was just fixed in al-test-harness but not in al-zed-test.
- **Fix:** Add `#[ignore = "requires running Zed instance on Hyprland"]` and remove early-return guards.

### TQ-H03: al-test-harness — `test_edit_b01` has vacuously-true assertion
- **File:** `crates/al-test-harness/tests/edit_lifecycle.rs:241-266`
- **Impact:** `diags.contains_key(&uri) || !diags.is_empty()` — the second condition is implied by the first. Diagnostics for a *different* file make this pass. Does NOT verify the edited file got diagnostics.
- **Fix:** Assert specifically that `diags.get(&uri)` contains error diagnostics.

### TQ-H04: al-test-harness — `test_diagnostics_published_on_open` has unnecessary 10s poll loop
- **File:** `crates/al-test-harness/tests/e2e.rs:334-363`
- **Impact:** `open_file()` already buffers the `publishDiagnostics` notification. The 20-iteration × 500ms loop is redundant, adding up to 10s to test runtime for no reason.
- **Fix:** Remove loop. Call `drain_diagnostics()` directly after `open_file()`.

### TQ-H05: al-test-harness — `wait_for_regex` documented as regex but uses literal substring
- **File:** `crates/al-zed-test/src/lsp_log.rs:216-232`
- **Impact:** API name lies. Users passing regex patterns get wrong results. Library docs direct users to this function for regex matching.
- **Fix:** Rename to `wait_for_substring`, or implement actual regex matching.

### TQ-H06: al-test-harness — Init polling leaks pending-map entries on request timeout
- **File:** `crates/al-test-harness/src/lib.rs:264-283`
- **Impact:** Each timed-out `workspace/symbol` probe leaves an entry in the `pending` HashMap. Over 60s of failed polls, 5-6 stale entries accumulate (oneshot senders never cleaned up).
- **Fix:** `self.pending.lock().await.remove(&id)` on timeout.

### TQ-H07: al-test-harness — `lsp_log::since_offset` inconsistent line ending handling
- **File:** `crates/al-zed-test/src/lsp_log.rs:155`
- **Impact:** `trim_end_matches('\n')` misses `\r`. `tail()` uses `content.lines()` which handles both. Inconsistent behavior in same module.
- **Fix:** `line.trim_end_matches(|c| c == '\n' || c == '\r')`.

### TQ-H08: al-core — `queries/references.rs` has zero unit tests
- **File:** `crates/al-core/src/queries/references.rs`
- **Impact:** "Find All References" — iterates workspace `file_index`, re-parses every .al file, applies case-insensitive name matching. Cross-file iteration, `include_declaration` filtering, and URI-to-path conversion all untested. Every other comparable query module has test suites.
- **Fix:** Add tests for: empty on unopened doc, include_declaration=true/false, cross-file refs, unknown identifier returns empty Vec.

### TQ-H09: al-core — `queries/folding.rs` has zero unit tests
- **File:** `crates/al-core/src/queries/folding.rs`
- **Impact:** The al-core wrapper is untested despite `al-syntax` having tests for the underlying function. Also returns LSP types directly (architecture violation).
- **Fix:** Add tests for: returns `None` for unopened URI, non-empty ranges for codeunit with procedures.

### TQ-H10: al-core — `hover()` itself untested; only formatting helpers tested
- **File:** `crates/al-core/src/queries/hover.rs`
- **Impact:** 4 tests exist but cover only private formatting helpers. The public `hover()` function (access_path_at, type resolution, symbol index, fallbacks) has zero direct coverage.
- **Fix:** Add tests for: `None` for unopened doc, `None` for cursor on whitespace, `Some` for known local variable.

### TQ-H11: al-core — `signature_help()` itself untested; only parameter parsing helper tested
- **File:** `crates/al-core/src/queries/signature.rs`
- **Impact:** 4 tests cover only `parse_parameters_from_detail`. The public function is untested.
- **Fix:** Add tests for: `None` for unopened doc, `None` for cursor outside call, `Some` with correct active parameter index.

### TQ-H12: al-core — Insight modules have zero negative tests (4 modules)
- **Files:** `crates/al-core/src/insight/analysis.rs` (13 positive, 0 negative), `insight/graph.rs` (14 positive, 0 negative), `insight/calls.rs` (10 positive, ~1 negative), `insight/index.rs`
- **Impact:** Violates CLAUDE.md Test Quality Gate which requires both positive and negative tests.
- **Fix:** Add per module: `table_impact()` on nonexistent object, `get_node()` for unknown key, call graph traversal for dangling reference.

### TQ-H13: al-core — Query modules have zero negative tests (4 modules)
- **Files:** `queries/dead_code.rs` (7 tests, 0 negative), `queries/duplicates.rs` (5 tests, 0 negative), `queries/sql_patterns.rs` (5 tests, 0 negative), `queries/obsolescence.rs` (3 tests, 0 negative)
- **Impact:** Each needs at least one "clean input produces empty result" assertion to satisfy the Test Quality Gate.

## MEDIUM

### TQ-M01: al-test-harness — `test_workspace_symbol_search` has zero assertions
- **File:** `crates/al-test-harness/tests/e2e.rs:392-406`
- **Impact:** Always passes regardless of server response. Only logs the count. No positive or negative assertion.
- **Fix:** Add assertion or remove (duplicated by integration_full.rs).

### TQ-M02: al-test-harness — `test_regression_inlay_hints` validation skipped when empty
- **File:** `crates/al-test-harness/tests/regression.rs:171-225`
- **Impact:** All validations inside `if !hints.is_empty()`. Server returning no hints → test passes silently.
- **Fix:** Assert `!hints.is_empty()` first.

### TQ-M03: al-test-harness — `test_completeness_a02` diagnostic code assertions never execute
- **File:** `crates/al-test-harness/tests/completeness.rs:201-233`
- **Impact:** Lint rules are disabled. No diagnostics produced. Loop body never entered. Test always passes.
- **Fix:** Use code that produces actual diagnostics, or convert to negative test.

### TQ-M04: al-test-harness — `drain_diagnostics` overwrites first notification with second
- **File:** `crates/al-test-harness/tests/edit_lifecycle.rs:255-265`
- **Impact:** `HashMap::insert` replaces previous diagnostics for same URI. If server sends clear-then-real pattern, first notification lost.
- **Fix:** Accumulate diagnostics or document the behavior.

### TQ-M05: al-test-harness — `protocol.rs` `symbol_names` recursive traversal
- **File:** `crates/al-test-harness/src/protocol.rs:22-33`
- **Impact:** Violates CLAUDE.md iterative traversal rule. Low practical risk (symbol nesting 3-4 levels max) but inconsistent with production code.
- **Fix:** Convert to iterative stack-based traversal.

### TQ-M06: al-test-harness — `test_project_from_env()` doc says "message to stderr" but doesn't write one
- **File:** `crates/al-test-harness/src/lib.rs:47-56`
- **Impact:** `AL_TEST_PROJECT_PATH` set to wrong dir → silent `None` → confusing "must be set" panic from callers.
- **Fix:** Add `eprintln!` warning when path exists but has no `app.json`.

### TQ-M07: al-semantic — Test pollution: parallel tests share real `~/.cache` directory
- **File:** `crates/al-semantic/src/cache.rs:95-141`
- **Impact:** Tests write to real `~/.cache/al-lsp/semantic/` with fixed version strings. Panic before cleanup leaks files. Parallel test races possible.
- **Fix:** Use `tempfile::TempDir` or unique version strings.

### TQ-M08: al-lsp — `integration.rs` has 45 tests, all happy-path; zero negative tests
- **File:** `crates/al-lsp/tests/integration.rs`
- **Impact:** No `is_err()` or `is_none()` assertions. Dispatch error paths never exercised.
- **Fix:** Add tests for: `dispatch_format` on unparseable input, `dispatch_hover` with out-of-range position, `dispatch_definition` on unknown URI.

### TQ-M09: al-syntax — `context.rs` has 24 tests but no UTF-16 edge cases
- **File:** `crates/al-syntax/src/context.rs`
- **Impact:** `resolution.rs` has a dedicated UTF-16 test (`receiver_chain_before_non_ascii_prefix`) that caught a real bug, but `context.rs` has no equivalent.
- **Fix:** Add tests for: `detect_context` with non-ASCII before member-access dot, cursor inside string literal.

### TQ-M10: al-syntax — `formatting.rs` has only 1 negative test out of 15
- **File:** `crates/al-syntax/src/formatting.rs`
- **Impact:** CLAUDE.md Formatter Lessons document tricky edge-case bugs found in production. Untested: empty input, comment-only file, deeply nested if/else (5+ levels), non-ASCII procedure names, split end/else case.

### TQ-M11: al-syntax — `traversal.rs` has zero tests
- **File:** `crates/al-syntax/src/traversal.rs`
- **Impact:** `walk_tree_until` has a non-trivial early-termination contract (returns `false` on early stop, `true` on complete). This return value is never tested.
- **Fix:** Add tests for: always-false visitor returns false, always-true returns true, stop-at-second-node visits exactly 2 nodes.

## LOW

### TQ-L01: al-zed-test — `test_connect_to_zed` and all live tests missing `#[ignore]`
- **File:** `crates/al-zed-test/tests/live_test.rs` (all tests)
- **Impact:** All require running Zed + Hyprland. Will fail on CI.
- **Fix:** Add `#[ignore = "requires running Zed IDE on Hyprland"]` to all tests.

### TQ-L02: al-cli — `write_temp_al` test helper doesn't clean up on panic
- **File:** `crates/al-cli/tests/integration.rs:55-59`
- **Impact:** Fixed-name temp files leaked on panic. Parallel tests with same name race on same path.
- **Fix:** Use RAII wrapper (struct with `Drop`) or unique names.

### TQ-L03: al-daemon-client — Missing negative JSON-RPC deserialization tests
- **File:** `crates/al-daemon-client/src/jsonrpc.rs:76-122`
- **Impact:** No tests for response with both `result` and `error` set, or wrong `id` type.
- **Fix:** Add edge-case deserialization tests.

### TQ-L04: al-test-harness — `test_edit_h03` unicode hover assertion may contradict known ISSUE-024
- **File:** `crates/al-test-harness/tests/edit_lifecycle.rs:719-745`
- **Impact:** UTF-16 position bug (ISSUE-024) documented elsewhere. This test asserts hover works on unicode identifiers — may pass only by coincidence of column alignment.

### TQ-L05: al-test-harness — `transport.rs` test allocates 10MB with no production guard
- **File:** `crates/al-test-harness/tests/transport.rs:231-256`
- **Impact:** Documents known vulnerability in `read_loop` but no corresponding fix or TODO in production code.
