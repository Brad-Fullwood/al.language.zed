# al-syntax Audit Report

**Files reviewed:** 17 | **Lines:** ~9K | **Issues:** 1 CRITICAL, 6 HIGH, 7 MEDIUM, 6 LOW

## Architecture: Clean (no upward dependency violations)
No imports from al-core, al-symbols, or al-semantic. LanguageData correctly loads from JSON.
**Exception:** `tower-lsp` is a dependency (part of the systemic issue — see T-001).

## CRITICAL

| ID | File | Line | Description |
|----|------|------|-------------|
| CRIT-1 | navigation.rs, type_resolver.rs | 11, 198 | UTF-16 char used as byte column in tree-sitter point |

## HIGH

| ID | File | Line | Category | Description |
|----|------|------|----------|-------------|
| HIGH-1 | symbols.rs | 943-963 | recursion | `collect_variable_name_nodes` is recursive |
| HIGH-2 | type_resolver.rs | 63-68 | hardcoded AL | "table"→"Record" mappings in `object_kind_to_al_type` |
| HIGH-3 | type_resolver.rs | 532 | hardcoded AL | `kw_table`/`kw_tableextension` node kinds |
| HIGH-4 | sort.rs | 63-88, 214-225 | hardcoded AL | Procedure modifier prefixes |
| HIGH-5 | tokens.rs | 207, 227, 257, 272 | performance | O(N²) line lookup via repeated `.split()` |
| HIGH-6 | symbols.rs | 323-333 | hardcoded AL | Section keyword→SymbolKind mapping |

## IMPORTANT

| ID | File | Line | Description |
|----|------|------|-------------|
| IMP-1 | type_resolver.rs | 750-751 | `.unwrap()` in non-test library code |
| IMP-2 | type_resolver.rs | 771 | Silent wrong column when `find()` returns None |
| IMP-4 | navigation.rs | 122-149 | Fallback emits un-normalized kind (e.g., `"kw_codeunit"`) |
| IMP-5 | symbols.rs | 976 | `"kw_function"` node kind hardcoded |
| IMP-6 | tokens.rs | 195-293 | Token emission logic duplicated verbatim (50 lines) |
| IMP-7 | parser.rs | 38, 46, 55 | `.expect()` in library code |
| IMP-8 | language_data.rs | 136-195 | LazyLock JSON panics deferred to runtime |

## MEDIUM

| ID | File | Line | Description |
|----|------|------|-------------|
| MED-1 | lib.rs | - | `complexity` module not re-exported |
| MED-2 | language_data.rs | 291 | `is_builtin_function` unused dead code |
| MED-3 | tokens.rs | 577-579 | Redundant private `has_ancestor_kind` wrapper |
| MED-4 | symbols.rs | 1006 | `"Label "` type keyword hardcoded |
| MED-5 | sort.rs | 44-46 | Ambiguous return from `sort_members` when no members |
| MED-6 | formatting.rs | - | Missing `prev_was_bare_end` tracking per CLAUDE.md |
| MED-7 | multiple | - | No `#[must_use]` on pure query functions |
