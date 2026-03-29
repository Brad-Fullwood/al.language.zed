# Comprehensive Code Review: AL Language Server

**Date**: 2026-03-29
**Scope**: All crates (~84K lines of Rust)
**Method**: Parallel deep review by 4 specialized Opus agents (foundation, core, transport, clients), consolidated by supervisor

---

## Executive Summary

The codebase is well-architected with clear separation between transport (al-lsp) and business logic (al-core). The dependency hierarchy is sound and no violations were found. However, there are concrete bugs (UTF-16 position handling, stale line arrays in workspace edits), security issues (unbounded reads, process::exit bypassing Drop), and structural problems (blocking I/O in async contexts, hardcoded AL values surviving in al-syntax).

**Total findings**: 10 Critical, 19 High, 12 Medium, 5 Low

---

## Critical Findings

### C1: UTF-16 Position Used as Byte Offset (BUG)

**Files:**
- `crates/al-syntax/src/navigation.rs:9-15` — `find_node_at_position`
- `crates/al-syntax/src/type_resolver.rs:181-189` — `find_enclosing_procedure`

```rust
let point = tree_sitter::Point {
    row: pos.line as usize,
    column: pos.character as usize,  // BUG: UTF-16 code unit, not byte offset
};
```

LSP `Position.character` is UTF-16 code units. Tree-sitter `Point.column` expects **byte offsets**. For any source with non-ASCII characters (common in Danish/Norwegian BC — characters like "Ø", "Å"), this resolves to the wrong node. The crate already has `utf16_col_to_byte_offset()` but it's not used here.

### C2: Stale Line Array in `apply_workspace_edit` (DATA CORRUPTION)

**File:** `crates/al-cli/src/commands/lsp.rs:889-958`

```rust
let lines: Vec<&str> = content.split('\n').collect();  // computed ONCE
let mut new_content = content.clone();
for (sl, sc, el, ec, new_text) in parsed_edits {
    let start_byte = lsp_pos_to_byte_offset(&lines, sl, sc)?;  // uses STALE lines
    new_content.replace_range(start_byte..end_byte, new_text);  // mutates content
}
```

The `lines` array is computed from the original content but edits mutate `new_content`. After the first edit changes byte layout, subsequent `lsp_pos_to_byte_offset` calls use stale line boundaries. **The `al rename` command can corrupt files** when multiple edits affect different positions in the same file.

### C3: Hardcoded `PAGE_CONTROL_KEYWORDS` (Architectural Violation)

**File:** `crates/al-syntax/src/symbols.rs:430-435`

```rust
const PAGE_CONTROL_KEYWORDS: &[&str] = &[
    "area", "group", "repeater", "field", "part", "action", "separator",
    "cuegroup", "grid", "fixed", "usercontrol", "label", "dataitem", ...
];
```

Direct violation of the #1 design rule. The `page_controls.json` data file exists and `language_data::page_controls()` loads it. This constant must be replaced.

### C4: Hardcoded `SINGLE_STMT_OPENERS` (Architectural Violation)

**File:** `crates/al-syntax/src/formatting.rs:419-425`

```rust
const SINGLE_STMT_OPENERS: &[(&str, &str)] = &[
    ("if ", " then"), ("for ", " do"), ("while ", " do"), ...
];
```

Hardcoded AL control-flow keyword pairs. Available via `language_data::keywords().control`.

### C5: Unbounded Reads (DoS Risk)

**al-lsp/src/daemon/mod.rs** — `read_line()` without size limit. OOM before `MAX_MESSAGE_SIZE` check.
**al-dap-client/src/framing.rs** — `vec![0u8; content_length]` without upper bound. OOM from malformed Content-Length.

### C6: `std::process::exit(0)` Bypasses Drop Guards

**File:** `crates/al-lsp/src/main.rs:39, 74`

Both `spawn_parent_monitor()` and `spawn_signal_handlers()` call `std::process::exit(0)` from tokio tasks. This skips ALL Drop destructors — open file handles, log file Mutex, tower-lsp state. Should use `CancellationToken` or close stdin to trigger graceful shutdown.

### C7: Blocking `std::fs::read_to_string` in Async Daemon Handler

**File:** `crates/al-lsp/src/daemon/mod.rs:568` — `ensure_document()`

```rust
let content = std::fs::read_to_string(&path).ok()?;
```

Synchronous filesystem read in async handler path. Called by nearly every daemon dispatch handler. On slow/network filesystems, blocks tokio worker threads. With 64 concurrent connections and a small thread pool, this can starve the runtime.

### C8: Blocking `std::fs::write` in Async Format Dispatcher

**File:** `crates/al-lsp/src/daemon/build_dispatch.rs:166`

Same pattern — synchronous file write in async context. Blocks tokio runtime.

### C9: Recursive Tree-sitter Traversal (Stack Overflow)

**al-syntax/src/parser.rs**, **al-syntax/src/folding.rs**, **al-syntax/src/lint.rs** — AST traversals use native Rust recursion. Deeply nested AL code can crash the language server via stack overflow.

### C10: `tower_lsp::lsp_types::Range` in Core Data Structures

**File:** `crates/al-core/src/file_index.rs:93`

```rust
pub selection_range: tower_lsp::lsp_types::Range,
```

`CachedProcedureInfo` stores LSP transport types in core data structures, violating transport-agnosticism. The `queries/mod.rs` defines its own `Range` for this purpose.

---

## High-Severity Findings

### H1: UTF-16 Mismatch in `find_workspace_field`

**File:** `crates/al-core/src/resolution.rs:1165-1188`

```rust
let col_start = line.find(name_part).unwrap_or(0) as u32;
```

Uses byte offset from `String::find()` as LSP `character` field. For non-ASCII field names (Scandinavian characters), produces incorrect positions.

### H2: TOCTOU Race in `get_or_build_insight_graph`

**File:** `crates/al-core/src/workspace.rs:120-134`

Read lock dropped before write lock acquired. Two concurrent callers both observe `None`, both build the graph. Wastes CPU. Compare with `get_or_build_call_graph` (line 155) which correctly uses double-checked locking.

### H3: `permissions.rs` Re-parses Files Already in Cache

**File:** `crates/al-core/src/permissions.rs:36`

`collect_permissions` calls `parse_quick` on every file even though `file_index.file_trees` caches parse trees. Unnecessary O(n) re-parse of entire workspace.

### H4: `unsafe impl Send + Sync` Without Full Justification

**File:** `crates/al-semantic/src/host.rs:34-37`

```rust
unsafe impl Send for DotNetHost {}
unsafe impl Sync for DotNetHost {}
```

`Sync` on `DotNetHost` directly (not the Mutex wrapper) means any `&DotNetHost` reference can be shared across threads. The safety argument references the Mutex in `SemanticBridge` but doesn't account for direct access.

### H5: `unsafe Mmap` Without Safety Documentation

**Files:** `crates/al-symbols/src/source_index.rs:37`, `crates/al-symbols/src/virtual_file.rs:80`

```rust
let mmap = unsafe { Mmap::map(&file)? };
```

Memory-mapped files are unsafe because concurrent modification is UB. `.app` files could be replaced by NuGet download while mmap is active. Staleness check at entry point doesn't protect against mid-read replacement.

### H6: Semantic Bridge Deadlock Risk

**al-core/src/semantic.rs** — `restart_bridge()` holds mutex during `SemanticBridge::new()` which invokes .NET CLR `Init`. If .NET hangs, mutex permanently deadlocked.

### H7: JSON-RPC `id` Typed as `u64` Only

**File:** `crates/al-daemon-client/src/jsonrpc.rs:10`

JSON-RPC 2.0 allows `id` to be string, number, or null. Daemon uses `pub id: u64`. String IDs fail to deserialize. The error response at `daemon/mod.rs:294` manually constructs `"id":null` because the typed struct can't represent it.

### H8: CLI Command Boilerplate (14+ copies)

**al-cli/src/commands/debug.rs** — 14 subcommands repeat identical connect/request/format lifecycle. `run_command` helper exists in `mod.rs:201-226` but isn't used in debug.rs.

### H9: `folding.rs` Returns Transport Types

**File:** `crates/al-core/src/queries/folding.rs:9`

```rust
pub fn folding_ranges(...) -> Option<Vec<tower_lsp::lsp_types::FoldingRange>>
```

Returns LSP types directly, breaking the transport-agnostic pattern all other query modules follow.

### H10: `tower_lsp::lsp_types` Used Throughout `resolution.rs`

**File:** `crates/al-core/src/resolution.rs:3-4`

31 occurrences of LSP types in business logic. Creates inconsistency with transport-agnostic query pattern.

### H11: Manual JSON in Daemon Dispatch

**al-lsp/src/daemon/lsp_dispatch.rs** — LSP types manually reconstructed as JSON instead of using `serde_json::to_value()` on the existing `Serialize` impls.

### H16: Dedup Cache Returns Empty Results for Valid Requests

**File:** `crates/al-lsp/src/daemon/mod.rs:327-333`

When a request is detected as duplicate (within 50ms), the daemon returns empty (`[]` for completions, `Null` for hover). User sees no hover info if client doesn't retry. Should queue duplicates and return the first request's actual result to all.

### H17: DAP Step Commands Acknowledged But Not Executed

**File:** `crates/al-dap-client/src/native_dap.rs:451-461`

`next`, `stepIn`, `stepOut`, `pause` respond `success: true` but don't invoke the corresponding `BcDebugSession` hub methods. User clicks "step over" and nothing happens.

### H18: DAP Shared `seq_counter` Between Concurrent Tasks

**File:** `crates/al-lsp/src/dap/mod.rs:116`

`AtomicI64` shared between `stdin_to_child` and `child_to_stdout` tasks. DAP spec requires monotonically increasing seq per direction; interleaving breaks this.

### H19: Client-Side Unbounded Read in DaemonClient

**File:** `crates/al-daemon-client/src/client.rs:140-149`

`read_line()` into unbounded `String`, limit checked after. Server side correctly uses `read_bounded_line`, but client doesn't.

### H12: Duplicate Daemon Connection in Explorer Views

**al-explorer/src/main.rs** — Four separate `Option<DaemonClient>` instances (EventChainView:117, CallGraphView:226, App:487, plus a fourth). Nearly identical `ensure_client()` methods copy-pasted.

### H13: Hardcoded `ObjectKind` Enum in al-explorer

**File:** `crates/al-explorer/src/types.rs:19-38`

Hardcoded AL object type list. Comment says "mirrors al-symbols" to avoid compile-time dependency, but still violates the no-hardcoded-values rule. Will go stale when Microsoft adds new object types.

### H14: Tests Silently Pass When Env Var Unset

**Files:** `data_driven.rs:8`, `zed_simulation.rs:12`, `performance.rs:15`

Tests return early with `eprintln!` when `AL_TEST_PROJECT_PATH` unset. CI shows "passed" while testing nothing. Should use `#[ignore]` to show "skipped".

### H15: Explorer `init_workspace` Blocks UI Thread

**File:** `crates/al-explorer/src/main.rs:526-529`

Retry loop calls `std::thread::sleep(800ms)` on the ratatui main thread. Freezes terminal for up to 4 seconds during startup.

---

## Medium-Severity Findings

### M1: Fallback Parser is Dead Code

**al-syntax/src/parser.rs** — Unreachable fallback parser allocation when `parse` returns None (only happens with timeout/cancellation, neither configured).

### M2: String Allocations in Hot Paths

**al-syntax/src/navigation.rs** — `extract_parameters` allocates `String` per call.
**al-core/src/file_index.rs**, **al-core/src/insight/search.rs** — `.to_lowercase()` in iteration loops.

### M3: DAP Framing Fragility

**al-dap-client/src/framing.rs** — `ensure_seq` patches JSON at byte level. Should use serde_json for safety.

### M4: OAuth Directory Permissions

**al-symbols/src/oauth.rs** — Parent directory `~/.cache/al-lsp/oauth/` created with default (potentially world-readable) permissions.

### M5: Concurrent Initialization Race

**al-lsp/src/server.rs** — No mechanism for requests to await workspace readiness. Early requests may return incomplete results.

### M6: `code_actions.rs` is a 4100-line God File

**File:** `crates/al-core/src/queries/code_actions.rs`

15+ distinct code action implementations in a single file. Should be split into a `code_actions/` directory with one file per action family.

### M7: Full Workspace Scan on Every References/Rename Request

**Files:** `crates/al-core/src/queries/references.rs:40-59`, `rename.rs:59-88`

Both iterate every file in `workspace.file_index.files` per request. For 10K-file workspaces, this is expensive per keystroke. Should use reverse index to narrow search.

### M8: Linear Builtin Scan in `signature.rs` When O(1) Index Exists

**File:** `crates/al-core/src/queries/signature.rs:149-177`

O(n*m) scan over all builtin types and methods. `SemanticCache::find_methods_by_name()` provides O(1) lookup and is already used by `hover.rs`.

### M9: Inconsistent Idiom Usage

Mixed `if let Some`, `.map_or(false, ...)`, `.is_some_and(...)` throughout codebase. Could standardize on `let-else` and `is_some_and`.

### M7: Mutex Poisoning Boilerplate

Pervasive `.unwrap_or_else(|e| e.into_inner())` pattern. Consider `tokio::sync` locks which don't poison.

### M8: Known OOM in Test Harness

**al-test-harness/src/lib.rs:751** — `vec![0u8; content_length]` without bound. Documented as ST-14 vulnerability.

---

## Low-Severity Findings

### L1: `tower-lsp` Dependency in al-syntax

Heavy dependency for a parsing library. Could use local position types.

### L2: Missing `#[must_use]` on Query Functions

`al-core/src/queries/` return `Option<T>` without `#[must_use]`.

### L3: Test Fixture Gaps

No tests for: very large files, deeply nested structures, Unicode identifiers, malformed .al files, concurrent daemon connections, slow filesystem behavior.

### L4: No Property-Based or Fuzz Testing

Parser has no fuzz testing despite handling arbitrary user input.

### L5: DashMap Ref Lifetimes

Throughout al-core, DashMap refs could potentially be held across await points. Needs audit of all `workspace.symbols.get()` call sites in async code.

---

## Recommendations Summary

| # | Priority | Item | Effort | Crate |
|---|----------|------|--------|-------|
| 1 | Critical | Fix UTF-16 → byte offset in navigation.rs, type_resolver.rs | Small | al-syntax |
| 2 | Critical | Fix stale line array in apply_workspace_edit | Small | al-cli |
| 3 | Critical | Replace PAGE_CONTROL_KEYWORDS with LanguageData | Small | al-syntax |
| 4 | Critical | Replace SINGLE_STMT_OPENERS with LanguageData | Small | al-syntax |
| 5 | Critical | Bound reads in daemon + DAP | Small | al-lsp, al-dap-client |
| 6 | Critical | Replace process::exit with graceful shutdown | Medium | al-lsp |
| 7 | Critical | Use tokio::fs in async daemon handlers | Medium | al-lsp |
| 8 | Critical | Convert recursive traversals to iterative | Medium | al-syntax |
| 9 | Critical | Remove LSP types from al-core data structures | Medium | al-core |
| 10 | High | Fix UTF-16 in resolution.rs field lookup | Small | al-core |
| 11 | High | Fix TOCTOU in insight graph build | Small | al-core |
| 12 | High | Use cached parse trees in permissions.rs | Small | al-core |
| 13 | High | Document unsafe Mmap safety invariants | Small | al-symbols |
| 14 | High | Semantic bridge restart timeout | Small | al-core |
| 15 | High | Fix JSON-RPC id typing | Small | al-daemon-client |
| 16 | High | CLI debug.rs boilerplate reduction | Medium | al-cli |
| 17 | High | Make folding.rs transport-agnostic | Small | al-core |
| 18 | High | Replace manual JSON in daemon dispatch | Medium | al-lsp |
| 19 | Medium | Remove dead fallback parser | Trivial | al-syntax |
| 20 | Medium | Secure OAuth directory permissions | Trivial | al-symbols |
| 21 | Medium | Add workspace readiness signal | Small | al-lsp |
| 22 | Low | Fuzz testing for parser | Medium | al-syntax |
