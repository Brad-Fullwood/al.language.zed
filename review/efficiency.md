# Efficiency Audit Report

**Scope:** Full codebase (all 12 crates) | **Issues:** 4 HIGH, 8 MEDIUM, 10 LOW

## HIGH — Must fix for acceptable performance

| ID | File | Line | Description | Impact |
|----|------|------|-------------|--------|
| H1 | al-core/queries/code_lens.rs | 86 | O(N²) — scans ALL files per procedure per code lens request | 30× reduction possible |
| H2 | al-lsp/workspace.rs | 200-235 | Startup re-parses files already cached by file_index | 200ms-2s startup saving |
| H3 | al-core/file_index.rs | 141-145 | `get_cached_parse` deep-clones file String (20+ callsites) | ~3MB/request eliminated |
| H4 | al-symbols/nuget.rs | 213 | Service index fetched per package, not per feed (N+1 HTTP) | 5+ HTTP calls saved |

## MEDIUM

| ID | File | Line | Description |
|----|------|------|-------------|
| M1 | al-syntax/type_resolver.rs | - | `variables_at` called 2-3× per hover/completion |
| M2 | al-syntax/symbols.rs | 68-88 | `object_kind_to_symbol_kind` O(N) linear scan (missing HashMap) |
| M3 | al-symbols/index.rs | 355-365 | `search_in_package` calls `to_lowercase()` per entry |
| M4 | al-lsp/workspace.rs | 207-236 | Config lock held across hundreds of files at startup |
| M5 | al-core/documents.rs | 77 | `get_text` deep-clones; should use `get_text_arc` |
| M6 | al-core/file_index.rs | 200 | Incremental scan clones all PathBufs for stale detection |
| M7 | al-symbols/index.rs | 490-549 | `remove_package_entries` O(total) scan |
| M8 | al-core/queries/inlay_hints.rs | 103, 570 | Recursive traversal (also a CLAUDE.md violation) |

## LOW

| ID | File | Description |
|----|------|-------------|
| L1 | al-core/queries/completions.rs:356 | Two sorts + per-item lowercase alloc in finalize |
| L2 | al-core/resolution.rs:332 | `inside_quoted_identifier` O(col) scan per call |
| L3 | al-symbols/app_reader.rs:178 | Archive file opened twice (find then open by name) |
| L4 | al-syntax/type_resolver.rs:63 | `to_ascii_lowercase()` allocs for simple match |
| L5 | al-core/queries/completions.rs:197 | `detect_context` called twice on fallback path |
| L6 | al-core/queries/code_lens.rs:96 | Full URI String in dedup HashSet |
| L7 | al-syntax/type_resolver.rs:166 | Four iterator passes in debug log |
| L8 | al-symbols/index.rs:320 | `search("", N)` misses default_completions cache |
| L9 | al-core/queries/signature.rs:45 | Parameters iterated twice for label construction |
| L10 | al-core/workspace.rs + al-lsp/workspace.rs | Duplicate initialization logic |
