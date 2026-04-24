# Code Reuse Audit Report

**Scope:** Full codebase | **Method:** Cross-crate pattern search

## HIGH Priority (exact or near-exact duplicates)

### 1. `did_visit` cursor traversal loop — 5 re-implementations
Canonical: `al-syntax/src/traversal.rs` (`walk_tree` / `walk_tree_until`)
Duplicates in: `queries/duplicates.rs`, `queries/tests.rs` ×2, `queries/test_coverage.rs` ×3
**Root cause:** `walk_tree` doesn't support subtree skipping. Need `TraversalAction { Continue, Skip, Stop }` enum.
**Fix:** Extend `walk_tree_until` API, then replace all copies.

### 2. `scope_label` — exact duplicate of `VariableScope::Display`
- `al-core/src/queries/mod.rs:258-270` — `scope_label()` function
- `al-syntax/src/type_resolver.rs:42-51` — `Display` impl
Verbatim identical match arm content.
**Fix:** Delete `scope_label`. Use `scope.to_string()` everywhere.

### 3. `has_local_modifier` — 3 different implementations
- `al-syntax/src/navigation.rs:382-394` — checks `member_modifier` OR `kw_local` (most correct)
- `al-core/src/queries/test_coverage.rs:257-269` — checks `"local"` kind (stale grammar variant)
- `al-core/src/insight/calls.rs:1049-1061` — checks `member_modifier` with text `"local"`
**Fix:** Add `pub fn has_local_modifier()` to al-syntax. Delete al-core copies.

### 4. Quote-aware attribute argument splitting — 4 implementations
- `dead_code.rs:489-517` — `split_args` (single+double quote, no paren depth)
- `obsolescence.rs:287-324` — `extract_attr_arg` (strips quotes, returns single arg by index)
- `calls.rs:875-893` — `parse_attr_args_from_text` (naive `split(',')`)
- `calls.rs:1081-1146` — `extract_attribute_args` (most robust: quotes + paren depth + escapes)
**Fix:** Promote `extract_attribute_args` to `queries/mod.rs`. Delete other 3.

## MEDIUM Priority (structural duplication)

### 5. Preceding-attribute sibling walker
- `dead_code.rs:429-449` — `get_preceding_attribute`
- `obsolescence.rs:207-238` — `extract_obsolete_from_preceding_attr`
Identical sibling-walking loop, different callback.
**Fix:** Extract `find_preceding_attribute<T>(node, source, f) -> Option<T>`.

### 6. JSONC comment stripper duplicated
- `al-lsp/src/workspace.rs:776-842` — handles `//` and `/* */`
- `al-dap-client/src/json_util.rs:16-51` — handles `//` only
al-lsp already depends on al-dap-client.
**Fix:** Add `/* */` support to al-dap-client version. Delete al-lsp copy.

### 7. File-index iteration boilerplate — 25+ repetitions
```rust
for entry in workspace.file_index.files.iter() {
    let path = entry.key().clone();
    drop(entry);
    let Some((text, tree)) = workspace.file_index.get_cached_parse(&path) else { continue; };
}
```
**Fix:** Add `FileIndex::for_each_parsed_file()` helper.

## LOW Priority

### 8. `extract_node_text`/`clean_node_text` useless wrappers
`al-syntax/symbols.rs:979-984` — two wrappers that just call `node_text_clean`. Delete, call directly.

### 9. `node_clean_name` near-duplicates `node_text_clean`
Justified by lifetime difference (borrowed vs owned). Document, don't merge.

### 10. BC HTTP error variants duplicated across 4 error enums
`bc_client.rs`, `test_runner.rs`, `profiling.rs`, `snapshot.rs` all define Http+AuthFailed+ServerError.
**Fix:** Extract `map_http_error_response` helper.

### 11. `collect_methods_recursive` uses recursion not `walk_tree`
`insight/calls.rs:739-757`. Convert to iterative.
