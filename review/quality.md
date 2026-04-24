# Code Quality Audit Report

**Scope:** Full codebase (~75K lines) | **Method:** Systematic file reading + pattern search

## Architecture Boundary Violations (9 findings)

All confirm and extend the per-crate findings. The root cause is `tower-lsp` in al-core and al-syntax.

| ID | Severity | File | Description |
|----|----------|------|-------------|
| AVB-001 | HIGH | resolution.rs | Returns LSP CompletionItem vectors from al-core |
| AVB-002 | HIGH | queries/symbols.rs | Returns DocumentSymbolResponse directly |
| AVB-003 | HIGH | queries/folding.rs | Returns LSP FoldingRange with rationalization comment |
| AVB-004 | HIGH | queries/search.rs | Result struct fields use LSP SymbolKind and Range |
| AVB-005 | HIGH | queries/inlay_hints.rs | Public function takes/returns 6 LSP types |
| AVB-006 | MEDIUM | queries/code_actions.rs | Private helpers take tower_lsp Range |
| AVB-007 | HIGH | al-syntax (whole crate) | Leaf crate depends on tower-lsp; 6 files affected |
| AVB-008 | HIGH | file_index.rs | CachedProcedureInfo.selection_range is LSP Range |
| AVB-009 | MEDIUM | queries/implementation.rs | Local LSP type conversion in core query |

## Recursive Traversal Violations (9 findings — extends per-crate review)

Per-crate review found 5. Quality audit found 4 additional:

| ID | Severity | File | Function |
|----|----------|------|----------|
| REC-001 | HIGH | queries/dead_code.rs | 3 recursive functions |
| REC-002 | HIGH | queries/inlay_hints.rs | 2 recursive functions |
| REC-003 | HIGH | queries/obsolescence.rs | `scan_procedures_for_obsolete` (NEW) |
| REC-004 | HIGH | insight/calls.rs | 3 recursive functions (NEW) |
| REC-005 | HIGH | queries/code_actions.rs | Recursive child iteration (NEW) |
| REC-006 | HIGH | queries/sql_patterns.rs | Recursive child iteration (NEW) |
| REC-007 | HIGH | queries/profiler_hints.rs | 2 recursive functions (NEW) |
| REC-008 | MEDIUM | al-syntax/symbols.rs | Multiple recursive-pattern loops |
| REC-009 | LOW | al-syntax/navigation.rs | Misleading _recursive names (actually iterative) |

**Total recursive traversal instances: 12+** (was 5 from per-crate review alone)

## Parameter Sprawl (4 findings)

| ID | File | Params | Fix |
|----|------|--------|-----|
| PSP-001 | queries/inlay_hints.rs | 8 params ×2 | InlayHintContext struct |
| PSP-002 | queries/signature.rs | 8 params | SignatureContext struct |
| PSP-003 | insight/calls.rs | 8 params | Use existing NodeKey |
| PSP-004 | queries/suggest_event.rs | 10 params | TraceContext struct |

## Duplication (6 findings)

| ID | Severity | Description |
|----|----------|-------------|
| DRY-001 | MEDIUM | Workspace file iteration pattern repeated 14 times across query files |
| DRY-002 | MEDIUM | for-sym/for-child/is_procedure_symbol scan repeated 5+ times |
| DRY-003 | MEDIUM | Phase 1 diagnostics block duplicated in compute and publish paths |
| DRY-004 | MEDIUM | ensure_client duplicated in two al-explorer view structs |
| DRY-005 | MEDIUM | try_read ERR_INITIALIZING boilerplate repeated 11 times in build_dispatch |
| DRY-006 | LOW | collect_recursive duplicates file_index::walk_al_files |

## Structural Issues

| ID | Severity | Description |
|----|----------|-------------|
| STR-004 | **HIGH** | `build_dispatch.rs` is 2,420 lines of business logic in the transport layer |
| STR-005 | MEDIUM | AlInlayHintLabel is a single-variant enum |
| STR-007 | MEDIUM | FileIndex has 8 DashMaps updated as a group with no enforced invariant |
| STR-001 | MEDIUM | DiagnosticEntry.severity is String; silent default on bad values |

## Stale Comments / Suppressed Lints

| ID | File | Description |
|----|------|-------------|
| CMT-001 | queries/mod.rs | Stale T301/T302 task references |
| CMT-002 | config.rs | Blanket TODO covering 15 fields |
| CMT-003 | navigation.rs | `_recursive` function names for iterative functions |
| SLW-001 | definition.rs | Crate-wide `allow(useless_conversion)` |
| SLW-002 | symbols.rs | Crate-wide `allow(deprecated)` |
