# Cross-Crate Architecture Review

## Dependency Compliance

### Rule: al-syntax, al-symbols, al-semantic must NOT depend on each other or al-core
**Status: PASS** — No inter-leaf dependencies found. Each leaf crate imports only external crates.

### Rule: al-daemon-client must NOT depend on al-core
**Status: PASS** — Only depends on serde, serde_json.

### Rule: zed-al is completely isolated
**Status: PASS** — Root Cargo.toml only depends on zed_extension_api, serde, serde_json.

### Rule: al-lsp must not contain business logic
**Status: MOSTLY PASS** — One violation found:
- `build_dispatch.rs:1670` — hardcoded `"ToolTip"` property check (business logic in transport layer)
- `workspace.rs:598-618` — `handle_workspace_symbol` accesses Workspace internals directly

### Rule: No LSP types in al-core or al-syntax
**Status: FAIL** — This is the #1 finding of this audit.

Both `al-core` and `al-syntax` have `tower-lsp` as a production dependency.

**al-core contamination (13+ files):**
- `resolution.rs` — all types use `lsp_types::Position/Range`
- `queries/inlay_hints.rs` — returns `Vec<InlayHint>` (LSP type)
- `queries/symbols.rs` — returns `DocumentSymbolResponse`
- `queries/folding.rs` — returns `Vec<FoldingRange>`
- `queries/search.rs` — result struct has LSP fields
- `queries/code_actions.rs` — private helpers take LSP Range
- `file_index.rs` — `CachedProcedureInfo` stores `lsp_types::Range`
- `queries/mod.rs` — contains `From<tower_lsp>` impls (belong in al-lsp)
- `completions.rs`, `hover.rs`, `definition.rs`, `references.rs`, `rename.rs`, `signature.rs`, `implementation.rs` — all convert Position→LSP on entry

**al-syntax contamination (7 files):**
- `formatting.rs` — `format_range` returns `Vec<lsp_types::TextEdit>`
- `navigation.rs`, `symbols.rs`, `context.rs`, `folding.rs`, `type_resolver.rs` — import `lsp_types`
- `lib.rs` — `ts_range_to_lsp` returns LSP Range

**Impact:**
1. Daemon mode (non-LSP transport) must import `tower_lsp::lsp_types` just to call query functions
2. Core business logic crates are coupled to a specific transport library
3. Prevents future non-LSP transports (HTTP API, WASM, etc.)
4. `From<tower_lsp>` impls in al-core pull tower-lsp into the transitive dependency tree

**Remediation:** Task T-001 (Tier 0, large effort). Transport-agnostic types already exist in `queries/mod.rs` but are not used consistently.

## Cross-Crate Issue Patterns

### Pattern 1: Hardcoded AL Values
Found in 3 crates (al-syntax, al-core, al-lsp). Total 10+ instances.
- al-syntax: object types, section keywords, procedure modifiers, Label keyword
- al-core: "table"/"tableextension" string comparisons
- al-lsp: "ToolTip" property name

All should use `LanguageData` or runtime symbol queries. Tasks T-008, T-009.

### Pattern 2: Recursive Tree-Sitter Traversal
Found in 2 crates (al-core: 4 instances, al-syntax: 1 instance). Total 5 instances.
CLAUDE.md mandates iterative traversal with explicit stack. Task T-004.

### Pattern 3: UTF-16 Position Bugs
Found in 2 crates (al-syntax: 2, al-core: 3). Total 5 instances.
All use tree-sitter byte columns as UTF-16 character positions. Task T-002.

### Pattern 4: `.unwrap()` in Non-Test Code
Found in 4 crates (al-symbols: 2, al-core: 3, al-syntax: 2, al-cli: 2). Total 9 instances.
Tasks T-006.

### Pattern 5: Blocking I/O in Async
Found in 1 crate (al-lsp: 10+ locations in workspace.rs and build_dispatch.rs).
Task T-005.

### Pattern 6: Duplicate Logic
- al-core/dead_code.rs + audit.rs — duplicate field extraction
- al-syntax/tokens.rs — 50-line token emission duplication
- al-core + al-lsp workspace initialization — duplicate package loading
Task T-024.

## Positive Findings

1. **DashMap safety** — consistently good patterns. Refs dropped before `.await` in most places (except al-dap-client).
2. **Error propagation** — thiserror used correctly across leaf crates.
3. **Test infrastructure** — al-test-harness spawns real LSP binary, good coverage for happy paths.
4. **Symbol caching** — Arc-based sharing, lazy initialization, correct cache invalidation.
5. **Daemon architecture** — clean JSON-RPC over Unix socket, proper lifecycle management.
6. **File index** — efficient DashMap-based concurrent index with incremental updates.
