# al-core Audit Report

**Files reviewed:** 64 | **Lines:** ~32K | **Issues:** 10 CRITICAL, 7 HIGH, 8 MEDIUM, 8+ LOW

## CRITICAL: Architecture Violations (tower-lsp contamination)

| ID | File | Line | Description |
|----|------|------|-------------|
| C-1 | Cargo.toml | 16 | `tower-lsp` is a production dependency |
| C-2 | resolution.rs | 3 | All position/range types are LSP; functions return `CompletionItem` |
| C-3 | queries/inlay_hints.rs | 9 | Returns and uses LSP types throughout |
| C-4 | queries/symbols.rs | 11 | Returns `DocumentSymbolResponse` |
| C-5 | queries/folding.rs | 12 | Returns `Vec<FoldingRange>` |
| C-6 | queries/search.rs | 9 | Result struct has LSP field types |
| C-7 | file_index.rs | 93 | `CachedProcedureInfo` stores `lsp_types::Range` |
| C-8 | queries/code_actions.rs | 64 | Private helpers take LSP range |
| C-9 | queries/*.rs | multiple | All queries convert Position to LSP on entry |
| C-10 | queries/mod.rs | 334-513 | `From<tower_lsp>` impls belong in al-lsp |

## CRITICAL: Safety / Panics

| ID | File | Line | Description |
|----|------|------|-------------|
| C-11 | parsing.rs | 25 | `.unwrap()` after `is_none()` check |
| C-12 | queries/code_actions.rs | 594 | `.unwrap()` on `common_var` in production |
| C-13 | queries/code_actions.rs | 1439 | `.unwrap()` on `chars().next()` |

## CRITICAL: Recursive Traversals (stack overflow)

| ID | File | Line | Description |
|----|------|------|-------------|
| C-14 | queries/dead_code.rs | 213-240 | `collect_procedures` recursive |
| C-15 | queries/dead_code.rs | 396-426 | `collect_event_subscribers` recursive |
| C-16 | queries/inlay_hints.rs | 61-116 | `collect_inlay_hints` recursive |
| C-17 | queries/inlay_hints.rs | 515-574 | `collect_return_type_hints` recursive |

## HIGH

| ID | File | Line | Category | Description |
|----|------|------|----------|-------------|
| H-1 | queries/inlay_hints.rs | 83 | bug | Raw byte column as UTF-16 character |
| H-2 | queries/inlay_hints.rs | 541 | bug | Same UTF-16 bug in return type hints |
| H-7 | queries/audit.rs | 124 | safety | `stack.pop().unwrap()` in production |
| H-10 | queries/audit.rs | 65 | hardcoded AL | `"table" \| "tableextension"` strings |
| H-11 | queries/dead_code.rs | 100 | hardcoded AL | `obj_kind_lower == "table"` |
| H-4 | file_index.rs | 66 | type-safety | `CachedObjectInfo::kind` is `String` not enum |
| H-8 | queries/dead_code.rs | 318 | duplication | Duplicate field extraction with audit.rs |

## MEDIUM

| ID | File | Line | Category | Description |
|----|------|------|----------|-------------|
| M-5 | workspace.rs | 128 | concurrency | `get_or_build_insight_graph` lacks double-checked locking |
| L-6 | queries/dead_code.rs | 65 | memory | Materializes all file trees into Vec simultaneously |
| L-7 | workspace.rs | 163 | API | Returns `RwLockReadGuard` to caller |
| M-4 | queries/completions.rs | 198 | performance | Redundant `detect_context` call |
| H-9 | queries/inlay_hints.rs | 151 | borderline | Hardcoded primitive type names (minor) |

## Performance (from efficiency audit)

| ID | File | Line | Description |
|----|------|------|-------------|
| H1-eff | queries/code_lens.rs | 86 | O(N²) — scans all files per procedure |
| H3-eff | file_index.rs | 142 | `get_cached_parse` deep-clones file String |
| M1-eff | type_resolver.rs | - | `variables_at` called 2-3× per request |
| M5-eff | documents.rs | 77 | `get_text` deep-clones; should use `get_text_arc` |
| M6-eff | file_index.rs | 200 | Incremental scan clones all PathBufs |
