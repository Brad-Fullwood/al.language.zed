# Code Quality — Dead Code, Duplication, Complexity

## HIGH

### CQ-H01: al-syntax — `collect_action_trigger_vars` uses `line.find(var_name)` for column — matches wrong occurrence
- **File:** `crates/al-syntax/src/type_resolver.rs:751-752`
- **Impact:** `line.find("Type")` matches the first occurrence anywhere on the line (could be in type annotation, comment, or string literal). Wrong column returned.
- **Fix:** Use structured parsing or at least scan only the declaration portion of the line.

## MEDIUM

### CQ-M01: al-explorer — `next_row`/`prev_row` triplicated verbatim across 3 view structs (+ 5 more variants)
- **File:** `crates/al-explorer/src/main.rs:185-217, 327-359, 509-541`
- **Impact:** 8 identical implementations of wrap-around index computation. Any fix must be applied 8 times.
- **Fix:** Extract `fn wrap_next(current: Option<usize>, len: usize) -> usize`.

### CQ-M02: al-explorer — `update_objects_list` double-filters when in global search mode
- **File:** `crates/al-explorer/src/main.rs:660-684`
- **Impact:** `SymbolIndex::search` already filters by query. The outer loop re-filters with the same query. Redundant but harmless.
- **Fix:** Skip outer filter when results already came from `search()`.

### CQ-M03: zed-al — `merge_json` is recursive on user-controlled data (stack risk in WASM)
- **File:** `src/lib.rs:25-40`
- **Impact:** WASM stacks are ~1MB. Deeply nested JSON in user settings can blow the stack. Same anti-pattern CLAUDE.md flags for tree-sitter.
- **Fix:** Add depth guard or convert to iterative.

### CQ-M04: al-cli — `collect_al_files_recursive` recursive with no depth bound, no symlink cycle guard
- **File:** `crates/al-cli/src/commands/mod.rs:104-124`
- **Impact:** Symlink cycle causes infinite recursion. Deep directory tree overflows stack. Violates CLAUDE.md iterative traversal rule.
- **Fix:** Convert to iterative with `VecDeque`. Track visited inodes for cycle detection.

### CQ-M05: al-semantic — `_init_fn` field stored with no documented safety reason
- **File:** `crates/al-semantic/src/host.rs:27-30`
- **Impact:** Init function pointer stored but never called after construction. Leading underscore suppresses warning. No `SAFETY` comment explaining why it's kept alive.
- **Fix:** Document with `// SAFETY: kept alive because ...` or remove if truly unused.

### CQ-M06: al-lsp — `dispatch_fix` parses and lints but always returns zero edits (dead code)
- **File:** `crates/al-lsp/src/daemon/build_dispatch.rs:213-258`
- **Impact:** Full parsing pipeline runs, results discarded. Client always receives `"fixes": 0`. Misleading API contract.
- **Fix:** Return `METHOD_NOT_FOUND` or remove from dispatch table.

### CQ-M07: al-dap-client — `kind_to_object_type` hardcodes AL object type strings
- **File:** `crates/al-dap-client/src/native_dap.rs:55-69`
- **Impact:** Violates spirit of no-hardcoded-AL-values. Leaf crate can't depend on LanguageData, but the match arms will go stale.
- **Fix:** Accept mapping as parameter from caller (al-lsp), which can look up from LanguageData.

### CQ-M08: al-symbols — `virtual_file.rs::render_outline` embeds AL keyword strings
- **File:** `crates/al-symbols/src/virtual_file.rs:163-188`
- **Impact:** Raw strings like `"procedure"`, `"field"`, `"key"`, `"var"` in output formatter. Lower severity than language intelligence hardcoding, but inconsistent.

### CQ-M09: al-core — `Box<dyn Error>` used in library code instead of typed errors
- **Files:** `crates/al-core/src/launch.rs:218,235`, `crates/al-lsp/src/daemon/mod.rs:53,274`
- **Impact:** `parse_zed_debug_file()`, `parse_vscode_launch_file()` return `Box<dyn std::error::Error>`. `al-core` already has `AlError` with `From<std::io::Error>` and `From<serde_json::Error>`. `run_daemon()` returns `Box<dyn Error>` which is not `Send`-safe across async boundaries.
- **Fix:** Return `AlError` / typed error.

### CQ-M10: al-core — `Result<T, String>` in public library APIs
- **Files:** `crates/al-core/src/queries/arch_lint.rs:55`, `crates/al-core/src/scaffold.rs:141`, `crates/al-core/src/queries/profiler_hints.rs`
- **Impact:** Makes it impossible for callers to distinguish error cases programmatically. `from_json` is a public constructor — should return `Result<Self, serde_json::Error>` or `AlError`.
- **Fix:** Use typed errors.

### CQ-M11: al-core — Per-token heap allocation in duplicate detection hot path
- **File:** `crates/al-core/src/queries/duplicates.rs:200-210,252-258`
- **Impact:** `tokens.push("$ID".to_string())` allocates for every identifier token in every procedure body. The `bigrams` function also allocates a `format!("{}|{}", ...)` for every bigram. Hot path called across entire workspace.
- **Fix:** Use `Vec<&'static str>` or `enum NormToken { Id, Str, Num, Kw(&'a str) }`.

### CQ-M12: al-core — Repeated double-`.map()` on same `Option<&_>` in search.rs
- **Files:** `crates/al-core/src/insight/search.rs:306-307,354-355,375-376`
- **Impact:** Works because `Option<&T>` is `Copy`, but intent unclear. Appears 6+ times.
- **Fix:** Destructure once: `let (name, object) = info.map(|i| (i.name.clone(), i.object.clone())).unwrap_or_default();`

## LOW

### CQ-L01: al-cli — Inconsistent return type: `std::process::ExitCode` vs `ExitCode`
- **File:** `crates/al-cli/src/commands/lsp.rs:2096, 2128`
- **Impact:** Two functions use fully qualified form; all others use the import. Visual inconsistency.

### CQ-L02: al-daemon-client — `find_al_lsp_binary` inconsistent file check (`.exists()` vs `.is_file()`)
- **File:** `crates/al-daemon-client/src/client.rs:186-203`
- **Impact:** Symlink to directory passes `.exists()` but fails at spawn time with confusing error.
- **Fix:** Use `.is_file()` consistently.

### CQ-L03: al-explorer — `cleanup` closure defined but duplicated inline for post-run cleanup
- **File:** `crates/al-explorer/src/main.rs:924-943`
- **Impact:** Two separate cleanup paths (closure uses `io::stdout()`, inline uses `terminal.backend_mut()`). `terminal.show_cursor()` only in inline path.

### CQ-L04: al-explorer — `'q'` key behavior inconsistent across views
- **File:** `crates/al-explorer/src/main.rs:1120, 1163`
- **Impact:** In EventChain/CallGraph list mode, `q` refocuses input. In ObjectBrowser, `q` has no binding. Confusing UX.

### CQ-L05: al-test-harness — `assert!(true, ...)` no-op in ignored transport tests
- **File:** `crates/al-test-harness/tests/transport.rs:518-521`
- **Impact:** `assert!(true)` can never fail. Sets bad precedent even in `#[ignore]` tests.

### CQ-L06: zed-al — `_adapter_name` parameter silently discarded in `get_dap_binary`
- **File:** `src/lib.rs:301-303`
- **Impact:** Multiple debug adapters would silently use AL adapter regardless.
- **Fix:** Validate `adapter_name == "al"`.
