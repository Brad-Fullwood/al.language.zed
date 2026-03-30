# Performance Issues

## HIGH

### PERF-H01: al-core — 10+ query modules re-parse all workspace files instead of using cache
- **Files:** `crates/al-core/src/queries/arch_lint.rs`, `duplicates.rs`, `obsolescence.rs`, `sql_patterns.rs`, `tests.rs`, `test_coverage.rs`, `audit.rs`, `profiler_hints.rs`
- **Impact:** Each invocation triggers N full tree-sitter parses (N = number of .al files). For a 500-file workspace, that's 500 parses per command. `dead_code.rs` correctly uses `file_index.get_cached_parse` — this should be universal.
- **Fix:** Replace `AlParser::parse_quick(&text)` with `workspace.file_index.get_cached_parse(&path)?` in all query modules.

### PERF-H02: al-lsp — Blocking `std::fs` calls in async daemon handlers (10+ sites)
- **Files:** `crates/al-lsp/src/daemon/build_dispatch.rs:190,778,808,850,1448,1458,1484,1518,1574,2296`, `daemon/mod.rs:630`, `server.rs:862`
- **Impact:** `std::fs::write`, `read_to_string`, `remove_dir_all` block the tokio worker thread. All concurrent requests stall during disk I/O.
- **Fix:** Replace with `tokio::fs` equivalents in all async functions.

### PERF-H03: al-lsp — `block_in_place` + `block_on` in async `dispatch_download_symbols`
- **File:** `crates/al-lsp/src/daemon/build_dispatch.rs:985-1056`
- **Impact:** Nests `block_on` inside `block_in_place`. Panics on single-thread scheduler. Blocks tokio worker for entire download duration.
- **Fix:** Make `dispatch_download_symbols` `async fn` and `.await` downloads directly.

### PERF-H04: al-syntax — `is_keyword()` does O(n) linear search on every call
- **File:** `crates/al-syntax/src/language_data.rs:216-226`
- **Impact:** Iterates all keywords across 4 categories. Called on every identifier during completions/parsing. No `HashSet` prebuilt.
- **Fix:** Build `LazyLock<HashSet<String>>` from keyword data.

### PERF-H05: al-syntax — `builtin_function_by_name` and `object_type_by_keyword` do O(n) linear search
- **File:** `crates/al-syntax/src/language_data.rs:204-214`
- **Impact:** Called for every identifier during semantic token extraction and type resolution. Full slice scan each time.
- **Fix:** Build `LazyLock<HashMap<String, &'static T>>` keyed by lowercased name.

### PERF-H06: al-core — `symbols.search("", usize::MAX)` loads entire symbol index into Vec
- **Files:** `crates/al-core/src/insight/graph.rs:193`, `queries/obsolescence.rs:75`, `queries/impact.rs:70`
- **Impact:** 50K-100K `Arc<SymbolEntry>` cloned into a Vec per call. Graph build (once at startup) is acceptable; `impact.rs` and `obsolescence.rs` run on user request.
- **Fix:** Expose `SymbolIndex::all_entries()` iterator that yields refs without Vec allocation.

## MEDIUM

### PERF-M01: al-core — `duplicates.rs` O(n^2) pair comparison with no pre-filtering
- **File:** `crates/al-core/src/queries/duplicates.rs:67-100`
- **Impact:** 1000 procedures = 499,500 comparisons, each running Jaccard similarity. Acceptable for small workspaces but may be slow for enterprise apps.
- **Fix:** Consider hash-based pre-filtering (e.g., MinHash) for large procedure sets.

### PERF-M02: al-symbols — `extract_source_by_path` creates new `ZipArchive` on every call
- **File:** `crates/al-symbols/src/source_index.rs:98-104`
- **Impact:** `ZipArchive::new()` re-parses the entire central directory on every go-to-definition request. O(n) per call for packages with hundreds of AL files.
- **Fix:** Cache `ZipArchive` per `AppSourceIndex`, or store zip entry index positions.

### PERF-M03: al-explorer — `SymbolIndex::search` lowercases every entry's name on every keystroke
- **File:** `crates/al-explorer/src/types.rs:200-211`
- **Impact:** 100K entries × `.to_lowercase()` allocation per keystroke. 100K allocations per frame update.
- **Fix:** Precompute lowercased `name_lower` at load time.

### PERF-M04: al-semantic — No in-memory cache for `builtin_types`/`error_codes`
- **File:** `crates/al-semantic/src/lib.rs:288-320`
- **Impact:** N concurrent first calls before disk cache populated = N × 500ms CLR calls serialized through Mutex. No `OnceLock` to deduplicate.
- **Fix:** Add `OnceLock<Vec<BuiltinType>>` on `SemanticBridge`.

### PERF-M05: al-test-harness — `test_edit_h04_many_sequential_edits` up to 250s for one test
- **File:** `crates/al-test-harness/tests/edit_lifecycle.rs:748-778`
- **Impact:** 50 iterations × 5s timeout per `change_file()`. Dominates test suite runtime.
- **Fix:** Use `change_file_no_wait()` for iterations 0..49, `change_file()` only on last.

### PERF-M06: al-core — `obsolescence.rs` compounds re-parse + full index load in one function
- **File:** `crates/al-core/src/queries/obsolescence.rs:49-75`
- **Impact:** 500 tree-sitter parses + 80K-entry Vec allocation for a single `al obsolescence` command.
