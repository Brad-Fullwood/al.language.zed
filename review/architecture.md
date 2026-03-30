# Architecture Violations

The project enforces strict dependency rules:
- al-core must NOT contain LSP types in its public API
- al-syntax, al-symbols, al-semantic must NOT depend on each other or on al-core
- al-lsp must NOT contain business logic
- Dependencies flow downward only

## CRITICAL

### ARCH-C01: al-core — LSP types in public query function signatures (8+ locations)
- **Files:**
  - `crates/al-core/src/queries/symbols.rs:8-17` — returns `tower_lsp::lsp_types::DocumentSymbolResponse`
  - `crates/al-core/src/queries/folding.rs:12` — returns `Vec<lsp_types::FoldingRange>`
  - `crates/al-core/src/queries/inlay_hints.rs:17-21` — takes `lsp_types::Range`, returns `Vec<InlayHint>`
  - `crates/al-core/src/queries/search.rs:9` — `WorkspaceChildSearchResult` contains `SymbolKind`, `Range`
  - `crates/al-core/src/queries/mod.rs:140-172` — `get_or_create_virtual_file` returns `lsp_types::Range`, `is_procedure_symbol` takes `SymbolKind`
  - `crates/al-core/src/file_index.rs:88-94` — `CachedProcedureInfo.selection_range` is `lsp_types::Range`
- **Impact:** Violates the #1 architecture rule. Any non-LSP consumer (daemon, CLI, al-explorer) must import `tower-lsp` to use query results.
- **Fix:** Define transport-agnostic counterpart types in `queries/mod.rs`. Convert at the `al-lsp` boundary.

### ARCH-C02: al-core — `resolution.rs` constructs `lsp_types::CompletionItem` directly
- **File:** `crates/al-core/src/resolution.rs:786-922, 925-1015`
- **Impact:** `completion_items_for_receiver` and `enum_completion_items` produce LSP types in business logic code. Called from `queries/completions.rs` which then has to convert.
- **Fix:** Return `Vec<CompletionEntry>` (crate-local type).

### ARCH-C03: al-core — `resolution.rs` uses `SymbolKind` for routing decisions
- **File:** `crates/al-core/src/resolution.rs:1082-1138`
- **Impact:** `workspace_member` checks `child.kind == SymbolKind::FUNCTION`. Hard dependency on LSP types for purely business logic routing.
- **Fix:** Define al-core-owned symbol kind enum.

### ARCH-C04: al-core — `From<lsp_types::Position/Range/Location>` impls live in al-core
- **File:** `crates/al-core/src/queries/mod.rs:250-296`
- **Impact:** These `From` impls give al-core a compile-time dependency on `tower_lsp::lsp_types`. Should be in al-lsp.
- **Fix:** Move to al-lsp as extension traits or free functions.

## MEDIUM

### ARCH-M01: al-lsp — `dispatch_inlay_hints` constructs `tower_lsp::lsp_types::Range` in daemon code
- **File:** `crates/al-lsp/src/daemon/lsp_dispatch.rs:226-232`
- **Impact:** Daemon layer should use transport-agnostic types, not LSP types.
- **Fix:** Construct `al_core::queries::Range` directly.

### ARCH-M02: al-lsp — Compile diagnostic conversion duplicated between server and daemon
- **File:** `crates/al-lsp/src/server.rs:983-1020` and `daemon/build_dispatch.rs:500-553`
- **Impact:** Same conversion logic in two places. Maintenance burden.
- **Fix:** Extract shared helper.

### ARCH-M03: al-semantic — `cache` and `host` modules are `pub` instead of `pub(crate)`
- **File:** `crates/al-semantic/src/lib.rs:8-9`
- **Impact:** External callers can bypass `SemanticBridge`'s Mutex serialization by directly accessing `DotNetHost`.
- **Fix:** Change both to `pub(crate)`.

### ARCH-M04: zed-al — Platform detection logic duplicated between `platform.rs` and `lib.rs`
- **File:** `src/lib.rs:87-99, 114-117` vs `src/platform.rs`
- **Impact:** `Platform::bin_dir()` returns `"darwin"` but asset name uses `"macos"`. Binary name duplicated. Divergence risk.
- **Fix:** Use `Platform`-derived values everywhere.

## Confirmed Clean

- **al-syntax** — No imports from al-core, al-symbols, or al-semantic
- **al-symbols** — No imports from al-core, al-syntax, or al-semantic
- **al-semantic** — No imports from al-core, al-syntax, or al-symbols
- **al-daemon-client** — Only depends on serde/serde_json (no al-core)
- **zed-al** — Completely isolated, no native crate imports
