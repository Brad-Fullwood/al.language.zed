# Full Codebase Audit — Summary

**Date:** 2026-03-31
**Commit:** c086d03d91ff2b892a4b016644340c1bb68d01a5
**Branch:** dev
**Auditors:** 10 parallel review agents + supervisor synthesis

## Scope

Every `.rs` file across all 12 crates (164 files), plus root `src/` (zed-al WASM extension).
Review types: architecture compliance, bugs, safety, performance, code quality, Rust idioms.

## Aggregate Findings

| Severity | Count | Breakdown |
|----------|-------|-----------|
| CRITICAL | 18 | Architecture violations (12), deadlocks (2), panics (2), bugs (2) |
| HIGH | 37 | Architecture (4), bugs (8), safety (6), performance (4), protocol (4), test gaps (4), recursive traversals (7 NEW) |
| MEDIUM | 52 | Bugs (8), architecture (6), performance (6), code quality (10), test gaps (8), duplication (6), param sprawl (4), structure (4) |
| LOW | 45+ | Rust idioms, minor performance, visibility, dead code, stale comments |
| **Total** | **152+** | |

**Additional from simplify agents:**
- 12+ recursive tree-sitter traversals total (was 5 from per-crate review)
- 25+ instances of file-index iteration boilerplate
- 2,420-line `build_dispatch.rs` with business logic in transport layer
- 4 implementations of quote-aware arg splitting
- 3 implementations of `has_local_modifier`

## Top 10 Issues by Impact

### 1. Systemic `tower-lsp` contamination in al-core and al-syntax (CRITICAL)
Both leaf crates import `tower-lsp` and return LSP wire types from public APIs. 13+ query files in al-core, 7+ files in al-syntax. Violates the core architecture rule. Transport-agnostic types already exist in `queries/mod.rs` but are unused.
**Impact:** Prevents transport independence (daemon mode imports LSP types). Blocks future non-LSP transports.
**Fix:** Remove `tower-lsp` from al-core and al-syntax Cargo.toml. Use existing transport-agnostic types. Move `From<tower_lsp>` impls to al-lsp.

### 2. Mutex held across `.await` in al-dap-client (CRITICAL — deadlock)
`native_dap.rs` holds session mutex across async operations in `setBreakpoints` handler and background event task. Causes deadlocks under normal debugger use.
**Impact:** Debugger hangs during breakpoint operations.
**Fix:** Clone session data, drop lock, then await.

### 3. Recursive tree-sitter traversals (HIGH — stack overflow)
6 instances across al-core (dead_code.rs ×2, inlay_hints.rs ×2) and al-syntax (symbols.rs ×1, plus additional recursive patterns). CLAUDE.md mandates iterative traversal with explicit stack.
**Impact:** Stack overflow on deeply nested AL files.
**Fix:** Convert all to iterative traversal with `Vec<Node>` stack.

### 4. UTF-16 position handling bugs (CRITICAL + HIGH)
`find_node_at_position` and `find_enclosing_procedure` in al-syntax use UTF-16 character as byte column. Inlay hints in al-core construct positions from raw byte offsets. All produce wrong results on non-ASCII files.
**Impact:** Wrong hover, completion, inlay hint positions for files with non-ASCII content.
**Fix:** Convert via rope UTF-16 helpers at every boundary.

### 5. Code lens O(N²) workspace scan (HIGH — performance)
`code_lens.rs` scans ALL workspace files per procedure. 30 procedures × 200 files = 6,000 full tree traversals per request.
**Impact:** Multi-second hangs on code lens in large projects.
**Fix:** Single-pass reference counting with HashMap.

### 6. Hardcoded AL language values (HIGH — multiple locations)
al-syntax: object types, section keywords, procedure modifiers, Label keyword. al-core: "table"/"tableextension" strings. al-lsp: "ToolTip" property.
**Impact:** Values become stale when Microsoft updates BC. CLAUDE.md violation.
**Fix:** Move all to LanguageData JSON or runtime symbol queries.

### 7. Blocking I/O in async contexts (CRITICAL — al-lsp)
`std::fs` calls throughout async handlers in workspace.rs, build_dispatch.rs. `al.reindex` blocks LSP handler indefinitely.
**Impact:** LSP responsiveness degradation, potential thread pool starvation.
**Fix:** Replace with `tokio::fs`, spawn background tasks for long operations.

### 8. `get_cached_parse` deep-clones file strings (HIGH — performance)
Every cross-file query clones the entire file content string. 20+ callsites.
**Impact:** ~3MB heap traffic per references query on a 200-file workspace.
**Fix:** Store files as `Arc<String>`, return Arc from getter.

### 9. al-semantic timeout doesn't bound lock hold time (HIGH)
A hung CLR call holds the Mutex forever. All subsequent bridge calls block indefinitely. The `tokio::time::timeout` only gives the caller a timeout, not the actual operation.
**Impact:** Hung .NET bridge freezes entire LSP server permanently.
**Fix:** Mark bridge as poisoned after timeout; all future calls return immediately.

### 10. Background DAP event task leaks on reconnect (CRITICAL)
New background task spawned on each launch/attach without cancelling prior task. Duplicate `stopped` events delivered to Zed.
**Impact:** Ghost breakpoint events, confused debugger state.
**Fix:** Cancel prior task via `CancellationToken` before spawning new one.

## Per-Crate Findings Summary

| Crate | CRITICAL | HIGH | MEDIUM | LOW | Key Issue |
|-------|----------|------|--------|-----|-----------|
| al-core | 10 | 7 | 8 | 8+ | tower-lsp contamination, recursive traversals |
| al-syntax | 1 | 6 | 7 | 6 | UTF-16 bug, hardcoded AL values, O(N²) tokens |
| al-symbols | 4 | 6 | 8 | 7 | Panics in library, TOCTOU, HTTP safety |
| al-lsp | 4 | 4 | 6 | 7 | Blocking I/O, architecture violations |
| al-dap-client | 3 | 6 | 0 | 5+ | Mutex deadlocks, protocol violations |
| al-cli | 0 | 3 | 5 | 4 | Multi-edit corruption, password logging |
| al-explorer | 0 | 2 | 4 | 3 | Blocking TUI, terminal cleanup |
| al-daemon-client | 0 | 1 | 2 | 2 | Thread-unsafe env var tests |
| al-semantic | 0 | 1 | 3 | 2 | Timeout doesn't bound lock |
| al-test-harness | 1 | 0 | 5 | 5 | Missing #[ignore], wrong assertions |
| al-zed-test | 2 | 0 | 3 | 3 | Missing #[ignore], no negative tests |
| zed-al | 0 | 0 | 3 | 2 | merge_json bug, no tests |
