# Full Codebase Review — Summary

**Date:** 2026-03-30
**Scope:** 90,347 lines across 169 Rust files, 11 crates
**Branch:** `fix/test-safety-hygiene`

---

## Totals by Severity

| Severity | Count |
|----------|-------|
| CRITICAL | 18 |
| HIGH     | 54 |
| MEDIUM   | 51 |
| LOW      | 27 |
| **Total** | **150** |

## Totals by Category

| Category | Count |
|----------|-------|
| Bugs | 35 |
| Security | 12 |
| Error Handling | 14 |
| Performance | 16 |
| Concurrency | 8 |
| Correctness (UTF-16, protocol) | 20 |
| Architecture | 12 |
| Code Quality | 18 |
| Test Quality | 15 |

## Totals by Crate

| Crate | Lines | CRIT | HIGH | MED | LOW | Total |
|-------|-------|------|------|-----|-----|-------|
| al-core | ~32K | 3 | 8 | 4 | 0 | 15 |
| al-syntax | ~9K | 2 | 7 | 7 | 0 | 16 |
| al-lsp | ~7K | 0 | 7 | 6 | 4 | 17 |
| al-symbols | ~6K | 0 | 2 | 8 | 4 | 14 |
| al-dap-client | ~3K | 2 | 5 | 2 | 6 | 15 |
| al-test-harness | ~8K | 3 | 8 | 7 | 5 | 23 |
| al-explorer | ~2K | 2 | 5 | 7 | 1 | 15 |
| al-semantic | ~1K | 1 | 5 | 4 | 1 | 11 |
| al-cli | ~4K | 1 | 3 | 4 | 2 | 10 |
| al-daemon-client | ~450 | 0 | 4 | 4 | 2 | 10 |
| zed-al (WASM) | ~620 | 2 | 5 | 4 | 2 | 13 |
| al-zed-test | ~1K | 0 | 2 | 0 | 0 | 2 |

---

## Top 10 Most Critical Issues

1. **DAP `request_seq` field name wrong** — al-dap-client `make_response` uses `"request_seq"` instead of `"requestSeq"`, breaking DAP response correlation with Zed (CRITICAL)
2. **DAP `continue` sends wrong argument** — `continue_execution(json!({}))` instead of the required `BreakpointExitReason` integer; BC will reject the command (CRITICAL)
3. **DAP variables handler doesn't decode frame+scope encoding** — `get_variables(101)` instead of `get_variables(1)` for frame 1; all variable lookups broken (HIGH)
4. **UTF-16 position used as byte offset in al-syntax** — `find_node_at_position` and `find_enclosing_procedure` pass `position.character` directly to tree-sitter's byte-column field; wrong node returned for any non-ASCII source (CRITICAL)
5. **LSP types leaked into al-core public API** — 8+ query functions return `tower_lsp::lsp_types::*` directly, violating the #1 architecture rule (CRITICAL)
6. **10+ query modules re-parse all files** — `arch_lint`, `duplicates`, `obsolescence`, `sql_patterns`, `tests`, `test_coverage`, `audit`, `profiler_hints` all call `AlParser::parse_quick` instead of using cached trees; 500 parses per invocation on large workspaces (HIGH)
7. **al-cli `apply_workspace_edit` corrupts multi-edit renames** — byte offsets computed from stale `lines` slice after `replace_range` mutates `new_content`; data corruption when replacement differs in length (CRITICAL)
8. **al-explorer terminal not restored on panic** — raw mode + mouse capture left enabled if any render function panics; shell becomes unusable (CRITICAL)
9. **Blocking `std::fs` calls in async al-lsp handlers** — 10+ sites use `std::fs::write`, `read_to_string`, `remove_dir_all` in async daemon dispatch functions, blocking the tokio worker thread (HIGH)
10. **Missing `await_ready()` in formatting/prepare_rename handlers** — requests serviced before workspace init completes, returning wrong results or defaults (HIGH)

---

## Systemic Patterns

### UTF-16/Byte Confusion (8 instances)
The #1 recurring correctness issue. LSP positions use UTF-16 code units; tree-sitter uses byte offsets. Conversion helpers exist (`utf16_col_to_byte_offset`) but are not consistently used. Affects: al-syntax `navigation.rs`, `type_resolver.rs`; al-core `inlay_hints.rs`, `resolution.rs`; al-lsp `diagnostics.rs`; al-syntax `formatting.rs`.

### Recursive Tree Traversal (8 instances)
CLAUDE.md mandates iterative traversal with explicit stack. Violations in: al-syntax `tokens.rs`, `complexity.rs` (3 functions); al-core `calls.rs`; al-cli `mod.rs`; al-test-harness `protocol.rs`; al-symbols `model.rs`.

### Hardcoded AL Values (4 instances)
The most heavily enforced rule. Violations in: al-syntax `symbols.rs` (`PAGE_CONTROL_KEYWORDS`); al-core `xliff.rs` (object type list); al-dap-client `native_dap.rs` (`kind_to_object_type`); al-symbols `virtual_file.rs` (render keywords).

### `unwrap()` in Non-Test Code (12+ instances)
Review gate explicitly checks for this. Found in: al-test-harness `lib.rs` (5 `notify().unwrap()`), al-cli `mod.rs` (`print_json`), al-semantic `build.rs`, al-lsp `dap/mod.rs`, al-daemon-client `client.rs`, al-symbols `oauth.rs`, al-explorer `main.rs`.

---

## Files with the Most Issues

| File | Issues | Crate |
|------|--------|-------|
| `al-core/src/resolution.rs` | 5 | al-core |
| `al-dap-client/src/native_dap.rs` | 8 | al-dap-client |
| `al-syntax/src/type_resolver.rs` | 5 | al-syntax |
| `al-explorer/src/main.rs` | 12 | al-explorer |
| `al-lsp/src/daemon/build_dispatch.rs` | 5 | al-lsp |
| `al-lsp/src/server.rs` | 5 | al-lsp |
| `al-test-harness/src/lib.rs` | 5 | al-test-harness |
| `al-test-harness/tests/edit_lifecycle.rs` | 4 | al-test-harness |

---

## Category Report Files

- [bugs.md](bugs.md) — Logic errors, data corruption, wrong behavior
- [security.md](security.md) — Path traversal, injection, credential handling
- [error-handling.md](error-handling.md) — unwrap/expect, swallowed errors, panics
- [performance.md](performance.md) — Blocking I/O, redundant parsing, O(n^2)
- [concurrency.md](concurrency.md) — Deadlocks, race conditions, lock contention
- [correctness.md](correctness.md) — UTF-16/byte confusion, protocol violations
- [architecture.md](architecture.md) — Dependency rule violations, wrong-layer logic
- [code-quality.md](code-quality.md) — Dead code, duplication, complexity
- [test-quality.md](test-quality.md) — Missing assertions, flaky tests, always-pass
