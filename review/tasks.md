# Review Fix Tasks

Each task is a discrete, independently fixable unit of work. Priority order: CRITICAL first, then HIGH, MEDIUM, LOW. Tasks are grouped by systemic pattern where possible to avoid redundant work.

---

## CRITICAL — Fix Before Merge

### T-001: Fix DAP response field name `request_seq` → `requestSeq`
- **Crate:** al-dap-client
- **File:** `crates/al-dap-client/src/native_dap.rs`
- **What:** `make_response` uses snake_case `"request_seq"` instead of camelCase `"requestSeq"`. Breaks all DAP response correlation with Zed.
- **Details:** [bugs.md → BUG-C01](bugs.md#bug-c01-al-dap-client--make_response-uses-request_seq-instead-of-requestseq)

### T-002: Fix DAP `continue` command argument
- **Crate:** al-dap-client
- **File:** `crates/al-dap-client/src/native_dap.rs`
- **What:** `continue_execution(json!({}))` must pass `BreakpointExitReason` integer (likely `0`), not empty object. BC rejects the command silently.
- **Details:** [bugs.md → BUG-C02](bugs.md#bug-c02-al-dap-client--continue-command-sends-json-instead-of-breakpointexitreason-integer)

### T-003: Fix DAP variables handler — decode frame+scope encoding
- **Crate:** al-dap-client
- **File:** `crates/al-dap-client/src/native_dap.rs`
- **What:** `scopes` encodes as `frame_id * 100 + scope_index` but `variables` passes the raw value. Decode before dispatching to `get_variables` vs `get_globals`.
- **Details:** [bugs.md → BUG-H01](bugs.md#bug-h01-al-dap-client--variables-handler-doesnt-decode-framescope-encoding)

### T-004: Fix UTF-16/byte position conversion in al-syntax navigation and al-core code_actions
- **Crate:** al-syntax, al-core
- **Files:** `crates/al-syntax/src/navigation.rs`, `crates/al-syntax/src/type_resolver.rs`, `crates/al-core/src/queries/code_actions.rs:838,1501`
- **What:** `find_node_at_position`, `find_enclosing_procedure`, `implement_interface_stubs`, and `source_action_make_local` pass `position.character` (UTF-16) directly as tree-sitter byte column. Use `utf16_col_to_byte_offset` before constructing `tree_sitter::Point`. Note: `source_action_if_to_case` at lines 570-573 in the same file already does this correctly.
- **Details:** [correctness.md → CORR-C01, CORR-C02, CORR-C03, CORR-C04](correctness.md#critical)

### T-005: Fix al-cli `apply_workspace_edit` stale offset computation
- **Crate:** al-cli
- **File:** `crates/al-cli/src/commands/lsp.rs`
- **What:** Byte offsets computed from original `lines` slice after `replace_range` mutates `new_content`. Pre-compute all offsets before the mutation loop.
- **Details:** [bugs.md → BUG-C03](bugs.md#bug-c03-al-cli--apply_workspace_edit-computes-byte-offsets-from-stale-lines-after-mutation)

### T-006: Fix al-explorer terminal restore on panic
- **Crate:** al-explorer
- **File:** `crates/al-explorer/src/main.rs`
- **What:** Install a `std::panic::set_hook` that calls `disable_raw_mode()` and `LeaveAlternateScreen` before the original hook. Prevents unusable shell after any rendering panic.
- **Details:** [bugs.md → BUG-C04](bugs.md#bug-c04-al-explorer--terminal-not-restored-on-panic)

### T-007: Fix al-explorer `details_items` rebuild location
- **Crate:** al-explorer
- **File:** `crates/al-explorer/src/main.rs`
- **What:** Move `details_items` rebuild from render function to `update_objects_list()`. Prevents stale data between draws and inconsistent state for event handlers.
- **Details:** [bugs.md → BUG-C05](bugs.md#bug-c05-al-explorer--details_items-rebuilt-inside-the-render-function-every-frame)

### T-008: Fix zed-al WASM relative path resolution for binary cache
- **Crate:** zed-al
- **File:** `src/lib.rs`
- **What:** `fs::metadata(&binary_path)` uses relative paths that resolve against WASM CWD, not extension work dir. Use absolute paths or rely on `zed::download_file` return value.
- **Details:** [bugs.md → BUG-C06](bugs.md#bug-c06-zed-al--relative-paths-used-for-binary-cache-check-in-wasm)

### T-008a: Fix DashMap deadlock in al-lsp workspace initialization
- **Crate:** al-lsp
- **File:** `crates/al-lsp/src/workspace.rs:213-232`
- **What:** DashMap `Ref` guard held across `.await` at line 230. Add `drop(text_entry);` after cloning value on line 214. One-line fix prevents confirmed deadlock.
- **Details:** [concurrency.md → CONC-C02](concurrency.md#conc-c02-al-lsp--dashmap-ref-held-across-await--confirmed-deadlock)

### T-008b: Fix DapClient::spawn expect panics
- **Crate:** al-dap-client
- **File:** `crates/al-dap-client/src/client.rs:47-48`
- **What:** Replace `expect("child stdin/stdout")` with `ok_or_else(|| DapError::SpawnFailed(...))` in pub library function that returns Result.
- **Details:** [error-handling.md → ERR-C03](error-handling.md#err-c03-al-dap-client--dapclientspawn-panics-on-piped-stdio-unavailability)

### T-008c: Fix `unreachable!()` in test_diagnostics.rs
- **Crate:** al-core
- **File:** `crates/al-core/src/queries/test_diagnostics.rs:106`
- **What:** Replace `TestStatus::Pass => unreachable!()` with `continue` or defensive log. Production library code must not panic.
- **Details:** [error-handling.md → ERR-C04](error-handling.md#err-c04-al-core--unreachable-in-production-library-code-results_to_diagnostics)

### T-009: Fix al-semantic Mutex poisoning — don't recover corrupt CLR state
- **Crate:** al-semantic
- **File:** `crates/al-semantic/src/lib.rs`
- **What:** Replace `e.into_inner()` with error return when Mutex is poisoned. After a panic during FFI, the CLR bridge state may be corrupt.
- **Details:** [concurrency.md → CONC-C01](concurrency.md#conc-c01-al-semantic--mutex-poisoning-recovery-allows-unsound-clr-state)

### T-010: Fix al-test-harness `unwrap()` in library code
- **Crate:** al-test-harness
- **Files:** `crates/al-test-harness/src/lib.rs`
- **What:** Replace `stdin.take().unwrap()` (line 139-140) and 5 `notify().unwrap()` calls (lines 307,332,378,390,401) with proper error handling. These are library code, not test code.
- **Details:** [error-handling.md → ERR-C01, ERR-C02](error-handling.md#critical)

---

## HIGH — Systemic: Remove LSP Types from al-core Public API

### T-011: Define transport-agnostic types in al-core queries/mod.rs
- **Crate:** al-core
- **Files:** `crates/al-core/src/queries/mod.rs`, `file_index.rs`
- **What:** Create al-core-owned `DocumentSymbolResponse`, `FoldingRange`, `InlayHint`, `CompletionEntry`, `SymbolKind` types. Remove `From<lsp_types::*>` impls from al-core (move to al-lsp).
- **Details:** [architecture.md → ARCH-C01 through ARCH-C04](architecture.md#critical)

### T-012: Convert al-core query functions to use transport-agnostic types
- **Crate:** al-core, al-lsp
- **Files:** `queries/symbols.rs`, `queries/folding.rs`, `queries/inlay_hints.rs`, `queries/search.rs`, `queries/mod.rs`, `resolution.rs`, `file_index.rs`
- **What:** Change public function signatures to return al-core types instead of `tower_lsp::lsp_types::*`. Add conversion layer in al-lsp.
- **Details:** [architecture.md → ARCH-C01 through ARCH-C04](architecture.md#critical)

---

## HIGH — Systemic: Fix All Recursive Tree Traversals

### T-013: Convert al-syntax recursive traversals to iterative
- **Crate:** al-syntax
- **Files:** `crates/al-syntax/src/tokens.rs`, `crates/al-syntax/src/complexity.rs`, `crates/al-syntax/src/symbols.rs`
- **What:** Convert `collect_tokens`, `collect_procedure_complexity`, `count_cyclomatic_decisions`, `compute_cognitive`, and `collect_var_symbols_recursive` to iterative traversal with explicit `Vec<Node>` stack.
- **Details:** [bugs.md → BUG-H09, BUG-H10, BUG-H15a](bugs.md#bug-h09-al-syntax--collect_tokens-is-recursive-stack-overflow-risk)

### T-014: Convert al-core recursive traversals to iterative
- **Crate:** al-core
- **Files:** `crates/al-core/src/insight/calls.rs`, `crates/al-core/src/queries/duplicates.rs`
- **What:** Convert `collect_procedure_names_from_node` and `collect_tokens` to iterative with explicit stack.
- **Details:** [bugs.md → BUG-H14, BUG-H15b](bugs.md#bug-h14-al-core--callsrs-uses-recursive-traversal)

### T-015: Convert al-symbols namespace recursion to iterative
- **Crate:** al-symbols
- **File:** `crates/al-symbols/src/model.rs`
- **What:** `collect_entries_recursive` recurses into nested namespaces without depth limit. Convert to iterative.
- **Details:** [correctness.md → CORR-H07](correctness.md#corr-h07-al-symbols--symbolreferencejson-namespace-recursion-unbounded)

### T-016: Convert remaining recursive traversals (al-cli, al-test-harness, zed-al)
- **Crates:** al-cli, al-test-harness, zed-al
- **Files:** `crates/al-cli/src/commands/mod.rs` (`collect_al_files_recursive`), `crates/al-test-harness/src/protocol.rs` (`symbol_names`), `src/lib.rs` (`merge_json`)
- **What:** Convert all to iterative. Add symlink cycle detection for `collect_al_files_recursive`.
- **Details:** [code-quality.md → CQ-M04](code-quality.md#cq-m04-al-cli--collect_al_files_recursive-recursive-with-no-depth-bound-no-symlink-cycle-guard), [test-quality.md → TQ-M05](test-quality.md#tq-m05), [code-quality.md → CQ-M03](code-quality.md#cq-m03-zed-al--merge_json-is-recursive-on-user-controlled-data-stack-risk-in-wasm)

---

## HIGH — Systemic: Fix All UTF-16/Byte Confusion

### T-017: Fix UTF-16 conversion in al-core inlay_hints and resolution
- **Crate:** al-core
- **Files:** `crates/al-core/src/queries/inlay_hints.rs`, `crates/al-core/src/resolution.rs`
- **What:** `node.start_position().column` (byte) used as `Position.character` (UTF-16). `line.find()` returns byte index used as character. Convert using UTF-16 counting.
- **Details:** [correctness.md → CORR-H01, CORR-H02](correctness.md#high)

### T-018: Fix UTF-16 conversion in al-syntax formatting
- **Crate:** al-syntax
- **File:** `crates/al-syntax/src/formatting.rs`
- **What:** `format_range` uses `l.len()` (bytes) as LSP end character. Use `byte_col_to_utf16_col`.
- **Details:** [correctness.md → CORR-M01](correctness.md#medium)

---

## HIGH — Systemic: Remove Hardcoded AL Values

### T-019: Replace `PAGE_CONTROL_KEYWORDS` with LanguageData lookup
- **Crate:** al-syntax
- **File:** `crates/al-syntax/src/symbols.rs`
- **What:** Replace hardcoded `PAGE_CONTROL_KEYWORDS` const and `control_keyword_to_symbol_kind` match with `language_data::page_controls()`.
- **Details:** [correctness.md → CORR-H03, CORR-H04](correctness.md#corr-h03-al-syntax--hardcoded-page_control_keywords-list)

### T-020: Replace hardcoded object type list in xliff.rs
- **Crate:** al-core
- **File:** `crates/al-core/src/xliff.rs`
- **What:** Replace hardcoded `object_types` array with `al_syntax::find_object_declaration` or `LanguageData`.
- **Details:** [correctness.md → CORR-H05](correctness.md#corr-h05-al-core--xliffrs-hardcoded-al-object-type-list)

### T-020a: Replace hardcoded `SINGLE_STMT_OPENERS` with LanguageData
- **Crate:** al-syntax
- **File:** `crates/al-syntax/src/formatting.rs`
- **What:** Replace const array of `(if/then, for/do, ...)` tuples with `LanguageData::single_stmt_openers()`.
- **Details:** [correctness.md → CORR-H05a](correctness.md#corr-h05a-al-syntax--hardcoded-single_stmt_openers-in-formattingrs)

### T-020b: Replace hardcoded `permission_for_kind` match with LanguageData
- **Crate:** al-core
- **File:** `crates/al-core/src/permissions.rs`
- **What:** Add `permission_type`/`permission_value` fields to `object_types.json`, query via `LanguageData` instead of hardcoded match.
- **Details:** [correctness.md → CORR-H05b](correctness.md#corr-h05b-al-core--hardcoded-permission_for_kind-match-in-permissionsrs)

### T-020c: Replace hardcoded `al_keywords` array in code_actions.rs
- **Crate:** al-core
- **File:** `crates/al-core/src/queries/code_actions.rs`
- **What:** Use `al_syntax::language_data::is_keyword()` instead of incomplete local array. Keep `//` and `end;` as explicit prefix checks.
- **Details:** [correctness.md → CORR-H05c](correctness.md#corr-h05c-al-core--incomplete-al_keywords-array-in-code_actionsrs)

---

## HIGH — DAP Session Fixes

### T-021: Move `configuration_done` to correct DAP lifecycle point
- **Crate:** al-dap-client
- **File:** `crates/al-dap-client/src/native_dap.rs`
- **What:** `configuration_done()` called in `launch` handler before Zed sets breakpoints. Move to the `"configurationDone"` handler.
- **Details:** [bugs.md → BUG-H02](bugs.md#bug-h02-al-dap-client--configurationdone-called-before-zed-sends-it)

### T-022: Propagate step/continue errors to Zed instead of silent success
- **Crate:** al-dap-client
- **File:** `crates/al-dap-client/src/native_dap.rs`
- **What:** `continue`, `next`, `stepIn`, `stepOut` all discard `invoke()` errors with `let _ =` and return `"success": true`. Check result and return error response on failure.
- **Details:** [bugs.md → BUG-H03](bugs.md#bug-h03-al-dap-client--stepcontinue-errors-silently-discarded-success-always-returned)

### T-023: Fix DAP session mutex contention blocking event forwarding
- **Crate:** al-dap-client
- **File:** `crates/al-dap-client/src/native_dap.rs`
- **What:** Drop the outer `session.lock()` guard before awaiting `invoke()`. Currently blocks background event task for up to 60s per invoke.
- **Details:** [concurrency.md → CONC-H03, CONC-H04](concurrency.md#conc-h03-al-dap-client--session-mutex-held-across-full-invoke-duration-up-to-60s)

---

## HIGH — LSP Server Fixes

### T-024: Add missing `await_ready()` to formatting and prepare_rename handlers
- **Crate:** al-lsp
- **File:** `crates/al-lsp/src/server.rs`
- **What:** `formatting`, `range_formatting`, and `prepare_rename` are the only handlers missing `self.await_ready().await`. Add it.
- **Details:** [bugs.md → BUG-H06, BUG-H07](bugs.md#bug-h06-al-lsp--formatting-and-range_formatting-missing-await_ready)

### T-025: Fix dedup ring buffer — don't write on deduplicated requests
- **Crate:** al-lsp
- **File:** `crates/al-lsp/src/daemon/mod.rs`
- **What:** Deduplicated (skipped) requests are written to the ring buffer, causing cascading dedup of subsequent valid requests. Move insert outside the `is_dup` path.
- **Details:** [bugs.md → BUG-H08](bugs.md#bug-h08-al-lsp--dedup-ring-buffer-writes-on-deduplicated-skipped-requests)

### T-026: Replace blocking `std::fs` with `tokio::fs` in async handlers
- **Crate:** al-lsp
- **Files:** `crates/al-lsp/src/daemon/build_dispatch.rs`, `daemon/mod.rs`, `server.rs`
- **What:** 10+ sites use `std::fs::write`, `read_to_string`, `remove_dir_all` in async functions. Replace with `tokio::fs` equivalents.
- **Details:** [performance.md → PERF-H02](performance.md#perf-h02-al-lsp--blocking-stdfs-calls-in-async-daemon-handlers-10-sites)

### T-027: Fix `block_in_place` + `block_on` in download_symbols
- **Crate:** al-lsp
- **File:** `crates/al-lsp/src/daemon/build_dispatch.rs`
- **What:** Make `dispatch_download_symbols` `async fn` and `.await` downloads directly instead of nesting `block_on`.
- **Details:** [performance.md → PERF-H03](performance.md#perf-h03-al-lsp--block_in_place--block_on-in-async-dispatch_download_symbols)

### T-028: Spawn `al.reindex` into background task
- **Crate:** al-lsp
- **File:** `crates/al-lsp/src/server.rs`
- **What:** `initialize_workspace` runs synchronously in `execute_command`, blocking the entire LSP queue. Spawn into background task like `initialized` does.
- **Details:** [error-handling.md → ERR-H06](error-handling.md#err-h06-al-lsp--alreindex-blocks-the-entire-lsp-request-queue)

---

## HIGH — al-syntax Fixes

### T-029: Fix `collect_dataitem_vars` zero-byte ranges
- **Crate:** al-syntax
- **File:** `crates/al-syntax/src/type_resolver.rs`
- **What:** Synthetic `VariableDecl` ranges have `start_byte: 0, end_byte: 0`. Compute actual byte offsets from line+col information.
- **Details:** [bugs.md → BUG-H11](bugs.md#bug-h11-al-syntax--collect_dataitem_vars-creates-ranges-with-start_byte-0-end_byte-0)

### T-030: Fix `sort_members` to recognize all procedure visibility modifiers
- **Crate:** al-syntax
- **File:** `crates/al-syntax/src/sort.rs`
- **What:** `is_member_keyword` misses `internal procedure`, `protected procedure`, `protected local procedure`. Files with these modifiers get corrupted sort results.
- **Details:** [bugs.md → BUG-H12](bugs.md#bug-h12-al-syntax--sort_members-doesnt-recognize-internal-procedure-or-protected-procedure)

### T-031: Build HashSet/HashMap indexes for LanguageData lookups
- **Crate:** al-syntax
- **File:** `crates/al-syntax/src/language_data.rs`
- **What:** `is_keyword`, `builtin_function_by_name`, `object_type_by_keyword` all do O(n) linear search. Build `LazyLock<HashSet>` and `LazyLock<HashMap>` at init.
- **Details:** [performance.md → PERF-H04, PERF-H05](performance.md#perf-h04-al-syntax--is_keyword-does-on-linear-search-on-every-call)

---

## HIGH — Security Fixes

### T-032: Fix al-daemon-client `/tmp` fallback and socket directory creation
- **Crate:** al-daemon-client
- **File:** `crates/al-daemon-client/src/socket.rs`, `client.rs`
- **What:** (1) Don't fall back to `/tmp` for socket path — use `/run/user/<uid>` or require `XDG_RUNTIME_DIR`. (2) Create `al-lsp/` subdirectory before bind. (3) Fix zombie child process on daemon spawn.
- **Details:** [security.md → SEC-H01](security.md#sec-h01), [bugs.md → BUG-H16, BUG-H17](bugs.md#bug-h16)

### T-032a: Fix unbounded RAM downloads in NuGet and BC server paths
- **Crate:** al-symbols
- **Files:** `crates/al-symbols/src/nuget.rs:279`, `crates/al-symbols/src/bc_server.rs:117-128`
- **What:** Add `Content-Length` check before `.bytes().await?`. Reject responses above 200 MB (matching `.app` limit). Prevents OOM from malicious feeds.
- **Details:** [security.md → SEC-H06, SEC-H07](security.md#sec-h06-al-symbols--nuget-nupkg-download-has-no-size-limit-before-buffering-into-ram)

### T-033: Fix al-symbols OAuth token directory permissions and HTML escaping
- **Crate:** al-symbols
- **Files:** `crates/al-symbols/src/oauth.rs`, `nuget.rs`
- **What:** (1) Check `create_secure_dir` return value before writing token. (2) HTML-escape OAuth error params. (3) Add NUL byte check to zip-slip guard. (4) Clean up temp file on decompression bomb.
- **Details:** [security.md → SEC-H02, SEC-M01, SEC-M02](security.md#high), [bugs.md → BUG-H15](bugs.md#bug-h15)

### T-034: Fix al-dap-client URL injection via connection_token
- **Crate:** al-dap-client
- **File:** `crates/al-dap-client/src/bc_debug.rs`
- **What:** Percent-encode `connection_token` and `conn_id` before embedding in WebSocket and browser URLs.
- **Details:** [security.md → SEC-H03](security.md#sec-h03)

### T-035: Fix al-lsp DAP capture log `expect` and JSON error injection
- **Crate:** al-lsp
- **Files:** `crates/al-lsp/src/dap/mod.rs`, `daemon/mod.rs`
- **What:** (1) Replace `expect` in DAP capture log open with graceful handling. (2) Use `serde_json::json!()` for error responses instead of manual string formatting.
- **Details:** [security.md → SEC-H04, SEC-M03](security.md#sec-h04)

---

## HIGH — Performance: Use Cached Parse Trees

### T-036: Replace `AlParser::parse_quick` with cached trees in 10 query modules
- **Crate:** al-core
- **Files:** `queries/arch_lint.rs`, `duplicates.rs`, `obsolescence.rs`, `sql_patterns.rs`, `tests.rs`, `test_coverage.rs`, `audit.rs`, `profiler_hints.rs`
- **What:** Use `workspace.file_index.get_cached_parse(&path)` instead of `AlParser::parse_quick(&text)`. Follow the pattern established in `dead_code.rs`.
- **Details:** [performance.md → PERF-H01](performance.md#perf-h01-al-core--10-query-modules-re-parse-all-workspace-files-instead-of-using-cache)

---

## HIGH — Remaining Individual Fixes

### T-037: Fix al-symbols `getrandom().expect()` panic in library code
- **Crate:** al-symbols
- **File:** `crates/al-symbols/src/oauth.rs`
- **What:** `random_bytes` panics on failure. Return `Result` and propagate.
- **Details:** [error-handling.md → ERR-H01](error-handling.md#err-h01)

### T-038: Fix zed-al platform detection and DAP binary resolution
- **Crate:** zed-al
- **Files:** `src/platform.rs`, `src/dap.rs`
- **What:** (1) Add `USERPROFILE`/`HOMEDRIVE` checks for native Windows. (2) Share full 4-step binary resolution between LSP and DAP. (3) Only insert DAP defaults for Launch scenarios.
- **Details:** [bugs.md → BUG-H04, BUG-H05, BUG-H13](bugs.md#bug-h04), [error-handling.md → ERR-H05](error-handling.md#err-h05)

### T-039: Fix al-semantic `Sync` on `DotNetHost` and timeout limitations
- **Crate:** al-semantic
- **Files:** `crates/al-semantic/src/host.rs`, `lib.rs`
- **What:** (1) Make `call` take `&mut self` or make `DotNetHost` `pub(crate)`. (2) Make `cache` and `host` modules `pub(crate)`. (3) Document that timeout does not release the Mutex lock.
- **Details:** [concurrency.md → CONC-H01, CONC-H02](concurrency.md#conc-h01), [architecture.md → ARCH-M03](architecture.md#arch-m03)

---

## MEDIUM — Grouped by Crate

### T-040: al-syntax — Fix miscellaneous correctness issues
- **Files:** `type_resolver.rs`, `context.rs`, `lib.rs`, `formatting.rs`
- **What:** (1) Case-insensitive `dataitem(` prefix check. (2) Handle inline `begin` with trigger header. (3) Skip comments in `find_call_context`. (4) Handle double-quoted identifiers in `count_net_delimiters`. (5) Fix `extract_formatted_region` truncation.
- **Details:** [correctness.md → CORR-M02 through CORR-M04](correctness.md#medium), [bugs.md → BUG-M07, BUG-M08](bugs.md#medium)

### T-041: al-cli — Fix format exit code, init-debug path, authenticate validation
- **Files:** `commands/lsp.rs`, `commands/insight.rs`, `main.rs`
- **What:** (1) `--json --check` single-file returns SUCCESS even when changed. (2) `cmd_init_debug` uses CWD not project root. (3) `Authenticate` accepts arbitrary subcommand strings. (4) `--format dot` ignores `--json`.
- **Details:** [bugs.md → BUG-M02 through BUG-M04](bugs.md#medium), [error-handling.md → ERR-M03](error-handling.md#err-m03)

### T-042: al-lsp — Fix daemon correctness issues
- **Files:** `daemon/mod.rs`, `daemon/lsp_dispatch.rs`, `server.rs`, `diagnostics.rs`
- **What:** (1) `require_project_root` conflates lock-busy with no-project. (2) `dispatch_fix` is dead code. (3) Compile diagnostic conversion duplicated. (4) Column unit from .NET bridge unclear.
- **Details:** [error-handling.md → ERR-M04](error-handling.md#err-m04), [code-quality.md → CQ-M06](code-quality.md#cq-m06), [architecture.md → ARCH-M01, ARCH-M02](architecture.md#medium)

### T-043: al-explorer — Fix UI and correctness issues
- **Files:** `crates/al-explorer/src/main.rs`, `types.rs`
- **What:** (1) Mouse layout from `terminal::size()` not last frame. (2) Kind carousel display with 2 kinds. (3) `find_member_line_in_file` substring false positives. (4) Precompute lowercased names for search. (5) Extract `wrap_next` helper for 8 duplicate nav functions.
- **Details:** [correctness.md → CORR-M10](correctness.md#corr-m10), [bugs.md → BUG-M05, BUG-M06](bugs.md#medium), [performance.md → PERF-M03](performance.md#perf-m03), [code-quality.md → CQ-M01](code-quality.md#cq-m01)

### T-044: al-core — Fix config, profiling, and performance issues
- **Files:** `config.rs`, `profiling.rs`, `queries/duplicates.rs`, `queries/obsolescence.rs`
- **What:** (1) Atomic write uses `with_extension("tmp")` — use unique temp path. (2) `stop_profiling` missing path validation. (3) `SymbolIndex::search("", usize::MAX)` clones entire index.
- **Details:** [bugs.md → BUG-M09](bugs.md#medium), [security.md → SEC-M05](security.md#sec-m05), [performance.md → PERF-H06, PERF-M01](performance.md#perf-h06)

### T-045: al-daemon-client — Fix response validation and retry semantics
- **Files:** `client.rs`, `jsonrpc.rs`
- **What:** (1) Validate response ID matches request ID. (2) Reuse `UnixStream` from wait_for_daemon. (3) Use `take()` for `MAX_RESPONSE_LINE` guard. (4) Use `.is_file()` consistently for binary check.
- **Details:** [concurrency.md → CONC-M02 through CONC-M04](concurrency.md#medium), [code-quality.md → CQ-L02](code-quality.md#cq-l02)

### T-046: al-semantic — Fix test pollution and cache issues
- **Files:** `cache.rs`, `host.rs`, `lib.rs`
- **What:** (1) Tests write to real `~/.cache` — use tempdir. (2) Document or remove `_init_fn` field. (3) Add `OnceLock` in-memory cache for `builtin_types`.
- **Details:** [test-quality.md → TQ-M07](test-quality.md#tq-m07), [code-quality.md → CQ-M05](code-quality.md#cq-m05), [performance.md → PERF-M04](performance.md#perf-m04)

### T-047: al-symbols — Fix performance and correctness issues
- **Files:** `source_index.rs`, `app_reader.rs`, `model.rs`, `bc_server.rs`
- **What:** (1) Cache `ZipArchive` per `AppSourceIndex`. (2) Fix double-parse in `parse_symbol_reference_json`. (3) Add `Kind` integer-to-string normalization. (4) Don't silently downgrade TLS.
- **Details:** [performance.md → PERF-M02](performance.md#perf-m02), [correctness.md → CORR-M09, CORR-H06](correctness.md#corr-h06), [security.md → SEC-M07](security.md#sec-m07)

### T-048: zed-al — Fix remaining medium issues
- **Files:** `src/lib.rs`, `src/dap.rs`, `extension.toml`, `Cargo.toml`
- **What:** (1) Validate user-configured binary path. (2) Don't map `launch.program` to BC `server`. (3) Pin `zed_extension_api` to specific rev. (4) Validate `_adapter_name` parameter.
- **Details:** [security.md → SEC-H05](security.md#sec-h05), [bugs.md → BUG-M01](bugs.md#medium), [correctness.md → CORR-H08](correctness.md#corr-h08)

---

## MEDIUM — Test Quality Fixes

### T-049: Fix al-test-harness test assertions and flaky patterns
- **Files:** `tests/e2e.rs`, `tests/edit_lifecycle.rs`, `tests/regression.rs`, `tests/completeness.rs`
- **What:** (1) Remove redundant 10s poll loop in `test_diagnostics_published_on_open`. (2) Fix vacuous assertion in `test_edit_b01`. (3) Assert non-empty hints in inlay hint regression test. (4) Fix or remove zero-assertion `test_workspace_symbol_search`. (5) Fix diagnostic code test that never executes.
- **Details:** [test-quality.md → TQ-H03 through TQ-H06, TQ-M01 through TQ-M04](test-quality.md)

### T-050: Fix al-test-harness pending-map leak on timeout
- **File:** `crates/al-test-harness/src/lib.rs`
- **What:** `initialize()` polling loop doesn't clean up pending map entries when `request()` times out. Add `self.pending.lock().await.remove(&id)` on timeout.
- **Details:** [test-quality.md → TQ-H06](test-quality.md#tq-h06)

### T-051: Fix al-zed-test — `#[ignore]` for live tests and `wait_for_regex` naming
- **Files:** `crates/al-zed-test/tests/live_test.rs`, `src/lsp_log.rs`
- **What:** (1) Add `#[ignore]` to all `live_test.rs` tests — they require running Zed. (2) Replace early-return guards with `#[ignore]`. (3) Rename `wait_for_regex` to `wait_for_substring` or implement actual regex. (4) Fix `\r\n` handling inconsistency.
- **Details:** [test-quality.md → TQ-H02, TQ-H05, TQ-H07, TQ-L01](test-quality.md)

---

## MEDIUM — Test Coverage Gaps

### T-059: Add unit tests for `references.rs` and `folding.rs`
- **Crate:** al-core
- **Files:** `crates/al-core/src/queries/references.rs`, `crates/al-core/src/queries/folding.rs`
- **What:** Both public query functions have zero tests. Add positive and negative tests for each.
- **Details:** [test-quality.md → TQ-H08, TQ-H09](test-quality.md#tq-h08-al-core--queriesreferencesrs-has-zero-unit-tests)

### T-060: Add tests for public `hover()` and `signature_help()` functions
- **Crate:** al-core
- **Files:** `crates/al-core/src/queries/hover.rs`, `crates/al-core/src/queries/signature.rs`
- **What:** Existing tests only cover private formatting helpers. Add direct tests for the public functions.
- **Details:** [test-quality.md → TQ-H10, TQ-H11](test-quality.md#tq-h10-al-core--hover-itself-untested-only-formatting-helpers-tested)

### T-061: Add negative tests to insight and query modules (8 modules)
- **Crate:** al-core
- **Files:** `insight/analysis.rs`, `insight/graph.rs`, `insight/calls.rs`, `insight/index.rs`, `queries/dead_code.rs`, `queries/duplicates.rs`, `queries/sql_patterns.rs`, `queries/obsolescence.rs`
- **What:** All modules have positive tests only. Add "empty/unknown input returns empty result" assertions to satisfy Test Quality Gate.
- **Details:** [test-quality.md → TQ-H12, TQ-H13](test-quality.md#tq-h12-al-core--insight-modules-have-zero-negative-tests-4-modules)

### T-062: Add negative tests to al-lsp integration tests
- **Crate:** al-lsp
- **File:** `crates/al-lsp/tests/integration.rs`
- **What:** 45 tests, all happy-path. Add dispatch error path tests (unparseable input, out-of-range position, unknown URI).
- **Details:** [test-quality.md → TQ-M08](test-quality.md#tq-m08-al-lsp--integrationrs-has-45-tests-all-happy-path-zero-negative-tests)

### T-063: Add UTF-16 edge case and negative tests to al-syntax modules
- **Crate:** al-syntax
- **Files:** `crates/al-syntax/src/context.rs`, `crates/al-syntax/src/formatting.rs`, `crates/al-syntax/src/traversal.rs`
- **What:** context.rs needs non-ASCII/UTF-16 tests; formatting.rs needs edge cases (empty input, comment-only, deep nesting); traversal.rs has zero tests.
- **Details:** [test-quality.md → TQ-M09, TQ-M10, TQ-M11](test-quality.md#tq-m09-al-syntax--contextrs-has-24-tests-but-no-utf-16-edge-cases)

---

## LOW — Optional Cleanup

### T-052: al-cli — Temp file cleanup and style consistency
- **Details:** [code-quality.md → CQ-L01](code-quality.md#low), [test-quality.md → TQ-L02](test-quality.md#low)

### T-053: al-daemon-client — Missing negative tests and retry naming
- **Details:** [test-quality.md → TQ-L03](test-quality.md#low)

### T-054: al-explorer — Cleanup closure duplication, key binding inconsistency
- **Details:** [code-quality.md → CQ-L03, CQ-L04](code-quality.md#low)

### T-055: al-test-harness — Remove `assert!(true)`, fix unicode hover vs ISSUE-024
- **Details:** [code-quality.md → CQ-L05](code-quality.md#low), [test-quality.md → TQ-L04, TQ-L05](test-quality.md#low)

### T-056: al-dap-client — Background task handle, polling latency, minor correctness
- **Details:** [code-quality.md → CQ-M07](code-quality.md#cq-m07)

### T-057: al-semantic — `option_env!` development-only strategy documentation
- **Details:** Covered in al-semantic review notes (LOW)

### T-058: al-cli — Document `--password` security implications
- **Details:** [security.md → SEC-M04](security.md#sec-m04)
