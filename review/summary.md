# Full Codebase Review — Summary

**Date:** 2026-03-30 (updated: 2026-03-30 second pass)
**Scope:** 90,347 lines across 169 Rust files, 11 crates
**Branch:** `dev`
**Method:** First pass: 4 Opus agents. Second pass: 8 Sonnet agents (full codebase, all categories).

---

## Totals by Severity

| Severity | Count |
|----------|-------|
| CRITICAL | 21 |
| HIGH     | 60 |
| MEDIUM   | 65 |
| LOW      | 27 |
| **Total** | **173** |

## Totals by Category

| Category | Count | New in 2nd pass |
|----------|-------|-----------------|
| Bugs | 36 | +2 (recursive traversals) |
| Security | 18 | +6 (unbounded downloads, daemon confinement, TLS, unsafe, bridge) |
| Error Handling | 15 | +2 (DapClient spawn, unreachable) |
| Performance | 12 | — |
| Concurrency | 10 | +1 (DashMap deadlock) |
| Correctness (UTF-16, protocol) | 25 | +5 (code_actions UTF-16, hardcoded values) |
| Architecture | 11 | +3 (al-syntax tower-lsp, workspace logic, dev-deps) |
| Code Quality | 19 | +4 (Box dyn Error, Result String, alloc, double-map) |
| Test Quality | 29 | +10 (references, folding, hover, signature, negatives) |

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

## Top 12 Most Critical Issues

1. **DAP `request_seq` field name wrong** — al-dap-client `make_response` uses `"request_seq"` instead of `"requestSeq"`, breaking DAP response correlation with Zed (CRITICAL)
2. **DAP `continue` sends wrong argument** — `continue_execution(json!({}))` instead of the required `BreakpointExitReason` integer; BC will reject the command (CRITICAL)
3. **DashMap deadlock in workspace init** — *(NEW)* DashMap `Ref` guard held across `.await` in `workspace.rs:213-232`. One-line fix: `drop(text_entry)` after clone. Confirmed deadlock. (CRITICAL)
4. **UTF-16 position used as byte offset** — `find_node_at_position`, `find_enclosing_procedure`, `source_action_make_local`, `implement_interface_stubs` all pass UTF-16 `position.character` directly as byte column. 4 locations, 2 crates. (CRITICAL)
5. **LSP types leaked into al-core and al-syntax** — 8+ query functions return `tower_lsp::lsp_types::*` directly. `al-syntax` also depends on `tower-lsp` as a leaf crate. (CRITICAL)
6. **Unbounded RAM downloads** — *(NEW)* NuGet `.nupkg` and BC server downloads have no size limit before `.bytes().await`. OOM from malicious feeds. (CRITICAL)
7. **al-cli `apply_workspace_edit` corrupts multi-edit renames** — byte offsets computed from stale `lines` slice after mutation (CRITICAL)
8. **al-explorer terminal not restored on panic** — raw mode + mouse capture left active (CRITICAL)
9. **DAP variables handler doesn't decode frame+scope encoding** — all variable lookups broken (HIGH)
10. **10+ query modules re-parse all files** — 500 parses per command on large workspaces (HIGH)
11. **Blocking `std::fs` calls in async al-lsp handlers** — 10+ sites block the tokio worker (HIGH)
12. **Missing `await_ready()` in formatting/prepare_rename handlers** — wrong results before init (HIGH)

---

## Systemic Patterns

### UTF-16/Byte Confusion (8 instances)
The #1 recurring correctness issue. LSP positions use UTF-16 code units; tree-sitter uses byte offsets. Conversion helpers exist (`utf16_col_to_byte_offset`) but are not consistently used. Affects: al-syntax `navigation.rs`, `type_resolver.rs`; al-core `inlay_hints.rs`, `resolution.rs`; al-lsp `diagnostics.rs`; al-syntax `formatting.rs`.

### Recursive Tree Traversal (8 instances)
CLAUDE.md mandates iterative traversal with explicit stack. Violations in: al-syntax `tokens.rs`, `complexity.rs` (3 functions); al-core `calls.rs`; al-cli `mod.rs`; al-test-harness `protocol.rs`; al-symbols `model.rs`.

### Hardcoded AL Values (7 instances)
The most heavily enforced rule. Violations in: al-syntax `symbols.rs` (`PAGE_CONTROL_KEYWORDS`), `formatting.rs` (`SINGLE_STMT_OPENERS`); al-core `xliff.rs` (object type list), `permissions.rs` (permissionable kinds), `code_actions.rs` (incomplete keyword array); al-dap-client `native_dap.rs` (`kind_to_object_type`); al-symbols `virtual_file.rs` (render keywords).

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
