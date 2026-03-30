# Bugs — Logic Errors, Data Corruption, Wrong Behavior

## CRITICAL

### BUG-C01: al-dap-client — `make_response` uses `"request_seq"` instead of `"requestSeq"`
- **File:** `crates/al-dap-client/src/native_dap.rs:951-956`
- **Impact:** Every DAP response from al-lsp to Zed has the wrong correlation field name. Zed cannot match responses to requests. Entire DAP debugging session is non-functional.
- **Fix:** Change `"request_seq"` to `"requestSeq"` in the `json!` macro.

### BUG-C02: al-dap-client — `continue` command sends `json!({})` instead of `BreakpointExitReason` integer
- **File:** `crates/al-dap-client/src/native_dap.rs:706`
- **Impact:** BC hub expects a `BreakpointExitReason` integer (0 for continue). Sending `{}` causes a SignalR deserialization error. "Continue" after breakpoint silently fails.
- **Fix:** `s.continue_execution(serde_json::json!(0)).await;`

### BUG-C03: al-cli — `apply_workspace_edit` computes byte offsets from stale `lines` after mutation
- **File:** `crates/al-cli/src/commands/lsp.rs:917-1000`
- **Impact:** Multi-edit rename where replacement length differs from original corrupts subsequent edit offsets. File content mangled.
- **Fix:** Pre-compute all `(start_byte, end_byte, new_text)` tuples before the mutation loop.

### BUG-C04: al-explorer — Terminal not restored on panic
- **File:** `crates/al-explorer/src/main.rs:935-943`
- **Impact:** If any rendering function panics, raw mode + mouse capture + alternate screen remain active. Shell becomes unusable until `reset` command.
- **Fix:** Install a panic hook that calls `disable_raw_mode()` and `LeaveAlternateScreen` before the original hook.

### BUG-C05: al-explorer — `details_items` rebuilt inside the render function every frame
- **File:** `crates/al-explorer/src/main.rs:1660-1893`
- **Impact:** `details_items.clear()` + rebuild inside `render_object_browser` causes stale data between draws. Event handlers reading `details_items` between frames see inconsistent state. Double-click on detail rows opens wrong line.
- **Fix:** Rebuild `details_items` in `update_objects_list()`, not inside the render pass.

### BUG-C06: zed-al — Relative paths used for binary cache check in WASM
- **File:** `src/lib.rs:120-147`
- **Impact:** `fs::metadata(&binary_path)` resolves relative to WASM CWD, not extension work dir. Cache check always fails, causing repeated re-downloads on every LSP start.
- **Fix:** Use absolute paths derived from the extension work directory.

## HIGH

### BUG-H01: al-dap-client — `variables` handler doesn't decode frame+scope encoding
- **File:** `crates/al-dap-client/src/native_dap.rs:771-858`
- **Impact:** `scopes` encodes references as `frame_id * 100 + scope_index`, but `variables` passes the raw encoded value to `get_variables()`. Frame 1 locals (ref=101) calls `get_variables(101)` instead of `get_variables(1)`. All variable lookups return wrong data or fail.
- **Fix:** Decode `vars_ref / 100` for frame_id and `vars_ref % 100` for scope_index before dispatch.

### BUG-H02: al-dap-client — `configurationDone` called before Zed sends it
- **File:** `crates/al-dap-client/src/native_dap.rs:183-189, 406`
- **Impact:** `configuration_done()` fires on BC hub in the `launch` handler, before Zed has set breakpoints. BC may start execution with no breakpoints active.
- **Fix:** Move `configuration_done()` into the `"configurationDone"` handler.

### BUG-H03: al-dap-client — Step/continue errors silently discarded, success always returned
- **File:** `crates/al-dap-client/src/native_dap.rs:644, 657, 670, 702-720`
- **Impact:** If BC rejects a step/continue (e.g., session not paused), Zed receives `"success": true` and UI shows execution as resumed when it's still stopped.
- **Fix:** Check `invoke()` result and return error response when it fails.

### BUG-H04: al-dap-client — DAP hardcoded debug defaults applied unconditionally
- **File:** `src/dap.rs:100-103` (zed-al)
- **Impact:** `launchBrowser: true`, `authentication: "UserPassword"`, `environmentType: "OnPrem"` forced on all scenarios including Attach and SaaS tenants.
- **Fix:** Only insert defaults for Launch; read from config with fallbacks.

### BUG-H05: al-dap-client — DAP binary resolution misses extension-cached binary
- **File:** `src/dap.rs:29-38` (zed-al)
- **Impact:** LSP uses 4-step chain including GitHub download. DAP only checks user-configured + PATH. Common case (downloaded by extension) not found. DAP fails with "al-lsp not found."
- **Fix:** Share the full 4-step resolution between LSP and DAP.

### BUG-H06: al-lsp — `formatting` and `range_formatting` missing `await_ready()`
- **File:** `crates/al-lsp/src/server.rs:640-660`
- **Impact:** Formatting requests before workspace init completes run against uninitialized state, using default config instead of project `.alformat.json`.
- **Fix:** Add `self.await_ready().await;` to both handlers.

### BUG-H07: al-lsp — `prepare_rename` missing `await_ready()`
- **File:** `crates/al-lsp/src/server.rs:771-782`
- **Impact:** Rename preparation before init returns `None` (no rename available) for any rename attempted immediately after server start.
- **Fix:** Add `self.await_ready().await;`

### BUG-H08: al-lsp — Dedup ring buffer writes on deduplicated (skipped) requests
- **File:** `crates/al-lsp/src/daemon/mod.rs:332-338`
- **Impact:** A deduplicated request is written to the ring buffer, causing the next identical request within 50ms to also be deduplicated. Creates a "dedup cascade" where rapid identical requests all return empty results.
- **Fix:** Only write to ring buffer when request is NOT a duplicate.

### BUG-H09: al-syntax — `collect_tokens` is recursive (stack overflow risk)
- **File:** `crates/al-syntax/src/tokens.rs:194-248`
- **Impact:** Deeply nested AL files overflow the stack. Violates CLAUDE.md iterative traversal rule.
- **Fix:** Convert to iterative with explicit `Vec<Node>` stack.

### BUG-H10: al-syntax — `complexity.rs` has 3 recursive traversals
- **File:** `crates/al-syntax/src/complexity.rs:31-59, 69-145`
- **Impact:** `collect_procedure_complexity`, `count_cyclomatic_decisions`, `compute_cognitive` are all recursive. Stack overflow on deeply nested AL.
- **Fix:** Convert all three to iterative traversal.

### BUG-H11: al-syntax — `collect_dataitem_vars` creates ranges with `start_byte: 0, end_byte: 0`
- **File:** `crates/al-syntax/src/type_resolver.rs:637-655`
- **Impact:** Any downstream code using `range.start_byte` or `range.end_byte` for go-to-definition, rename, or highlighting points to byte 0 of the file.
- **Fix:** Compute actual byte offsets from line+col.

### BUG-H12: al-syntax — `sort_members` doesn't recognize `internal procedure` or `protected procedure`
- **File:** `crates/al-syntax/src/sort.rs:205-213`
- **Impact:** Files containing `internal procedure Foo()` won't be split at that boundary. The whole procedure is appended to the previous member's lines, corrupting the sort result.
- **Fix:** Add `internal procedure`, `protected procedure`, `protected local procedure` to `is_member_keyword`.

### BUG-H13: zed-al — `detect_platform` misses native Windows case
- **File:** `src/platform.rs:53-66`
- **Impact:** On native Windows (PowerShell), `OSTYPE` not set, `HOME` not set. Falls through to Linux default. Windows misdetected as Linux.
- **Fix:** Check `USERPROFILE` or `HOMEDRIVE` as additional Windows indicators.

### BUG-H14: al-core — `calls.rs` uses recursive traversal
- **File:** `crates/al-core/src/insight/calls.rs:108-133`
- **Impact:** Stack overflow risk on deeply nested AL. Violates CLAUDE.md rule.
- **Fix:** Convert to iterative stack-based traversal.

### BUG-H15a: al-syntax — `collect_var_symbols_recursive` is recursive (stack overflow risk)
- **File:** `crates/al-syntax/src/symbols.rs:889-934`
- **Impact:** Recursive traversal through tree-sitter nodes. Violates CLAUDE.md iterative traversal rule. Stack overflow on deeply nested AL with many variable sections.
- **Fix:** Convert to iterative with explicit `Vec<Node>` stack.

### BUG-H15b: al-core — `collect_tokens` in duplicates.rs is recursive (stack overflow risk)
- **File:** `crates/al-core/src/queries/duplicates.rs:190-220`
- **Impact:** Same pattern as `tokens.rs`. Recursive DFS over every token in procedure bodies for duplicate detection. Runs across potentially large procedure bodies in the entire workspace.
- **Fix:** Convert to iterative with explicit `Vec<Node>` stack.

### BUG-H15: al-symbols — Decompression-bomb check fires after 512MB already written to disk
- **File:** `crates/al-symbols/src/nuget.rs:372-382`
- **Impact:** File left on disk after limit triggered. No cleanup.
- **Fix:** `let _ = std::fs::remove_file(&out_path);` on error.

### BUG-H16: al-daemon-client — Socket parent directory never created
- **File:** `crates/al-daemon-client/src/socket.rs:20-27`
- **Impact:** On first run, `$XDG_RUNTIME_DIR/al-lsp/` doesn't exist. `bind()` fails with `ENOENT`. Daemon cannot start.
- **Fix:** `create_dir_all(parent)` before bind.

### BUG-H17: al-daemon-client — Zombie child process on daemon spawn
- **File:** `crates/al-daemon-client/src/client.rs:158-170`
- **Impact:** `Child` handle dropped without `wait()`. Creates zombie process entries until parent exits.
- **Fix:** `std::mem::forget(child)` or double-fork.

## MEDIUM

### BUG-M01: zed-al — `dap_config_to_scenario` maps `launch.program` to BC `server` field
- **File:** `src/dap.rs:88-90`
- **Impact:** A file path like `/home/user/MyApp` sent as the BC `server` hostname. Semantically incorrect.

### BUG-M02: al-cli — `cmd_format` single-file returns SUCCESS when `--json --check` detects changes
- **File:** `crates/al-cli/src/commands/lsp.rs:516-537`
- **Impact:** CI scripts using `al --json format --check file.al` get exit 0 even when reformatting needed.

### BUG-M03: al-cli — `cmd_graph` `--format dot` ignores `--json` flag
- **File:** `crates/al-cli/src/commands/insight.rs:63-67`
- **Impact:** Inconsistent with `cmd_deps_graph` which takes JSON first.

### BUG-M04: al-cli — `cmd_init_debug` uses CWD, not project root
- **File:** `crates/al-cli/src/commands/lsp.rs:1293-1403`
- **Impact:** `.zed/debug.json` created in wrong directory when CLI invoked from different directory.

### BUG-M05: al-explorer — When `total == 2` kinds, prev and next labels show the same kind
- **File:** `crates/al-explorer/src/main.rs:1569-1578`
- **Impact:** Tab carousel shows `[PrevKind] [ActiveKind] [PrevKind]` instead of distinct labels.

### BUG-M06: al-explorer — `find_member_line_in_file` false-positives on common names
- **File:** `crates/al-explorer/src/main.rs:2257-2265`
- **Impact:** Substring search for `No`, `Name`, `Type` matches wrong line. Double-click opens at wrong position.

### BUG-M07: al-syntax — `count_net_delimiters` doesn't handle double-quoted identifiers
- **File:** `crates/al-syntax/src/lib.rs:117-135`
- **Impact:** `(` or `)` inside `"Proc (Test)"` corrupts paren depth, causing wrong indentation.

### BUG-M08: al-syntax — `extract_formatted_region` silent truncation on blank-line collapsing
- **File:** `crates/al-syntax/src/formatting.rs:380-408`
- **Impact:** `fmt_idx` overruns when formatted output is shorter than original. Returns truncated result.

### BUG-M09: al-core — `config.rs` atomic write uses `with_extension("tmp")` which strips existing extension
- **File:** `crates/al-core/src/config.rs:278-288`
- **Impact:** `settings.json` → `settings.tmp` (not `settings.json.tmp`). Previous failed writes overwritten silently.

### BUG-M10: al-symbols — `source_index::get_or_build` TOCTOU race on cache invalidation
- **File:** `crates/al-symbols/src/source_index.rs:113-127`
- **Impact:** Two threads both find stale entry, both rebuild. Last writer wins, old mmap dropped while potentially in use.

### BUG-M11: al-lsp — `dispatch_fix` always returns zero edits (dead code path)
- **File:** `crates/al-lsp/src/daemon/build_dispatch.rs:213-258`
- **Impact:** `al fix` parses and lints but discards results. Always returns `"fixes": 0`.
