# Correctness — UTF-16/Byte Confusion, Protocol Compliance, Type Confusion

## CRITICAL

### CORR-C01: al-syntax — `find_node_at_position` uses `position.character` as byte column
- **File:** `crates/al-syntax/src/navigation.rs:8-15`
- **Impact:** `position.character` is UTF-16 code units from LSP. `tree_sitter::Point::column` expects byte offset. Wrong AST node returned for any non-ASCII source. Affects all features: hover, completion, go-to-definition.
- **Fix:** Use `utf16_col_to_byte_offset` to convert before constructing `tree_sitter::Point`.

### CORR-C02: al-syntax — `find_enclosing_procedure` has same UTF-16-as-bytes bug
- **File:** `crates/al-syntax/src/type_resolver.rs:196-204`
- **Impact:** Core type resolution path for completions and hover. Every call path through `variables_at` or `resolve_type` affected when source has non-ASCII characters.
- **Fix:** Same as CORR-C01.

### CORR-C03: al-core — `source_action_make_local` uses UTF-16 character as byte column
- **File:** `crates/al-core/src/queries/code_actions.rs:1501`
- **Impact:** `range.start.character as usize` passed directly as byte column to `tree_sitter::Point`. Wrong node for non-ASCII source. Irony: `source_action_if_to_case` at lines 570-573 in the same file correctly converts via `utf16_col_to_byte_offset`.
- **Fix:** Use `utf16_col_to_byte_offset` before constructing Point.

### CORR-C04: al-core — `implement_interface_stubs` has same UTF-16-as-bytes bug
- **File:** `crates/al-core/src/queries/code_actions.rs:838`
- **Impact:** `range.start.character as usize` passed directly to `find_codeunit_at_point`. Same class as CORR-C03.
- **Fix:** Same — convert via `utf16_col_to_byte_offset`.

## HIGH

### CORR-H01: al-core — `inlay_hints.rs` uses tree-sitter byte column as LSP character
- **File:** `crates/al-core/src/queries/inlay_hints.rs:82-84`
- **Impact:** `node.start_position().column as u32` placed directly in `Position.character`. Hints positioned wrong for non-ASCII identifiers.
- **Fix:** Convert tree-sitter byte column to UTF-16 using source line.

### CORR-H02: al-core — `find_workspace_field` in `resolution.rs` returns byte offset as character
- **File:** `crates/al-core/src/resolution.rs:1227-1237`
- **Impact:** `line.find(name_part).unwrap_or(0) as u32` returns byte position, used as LSP character. Wrong hover/definition position for non-ASCII field names.
- **Fix:** Count UTF-16 code units to the match position.

### CORR-H03: al-syntax — Hardcoded `PAGE_CONTROL_KEYWORDS` list
- **File:** `crates/al-syntax/src/symbols.rs:433-457`
- **Impact:** Violates CLAUDE.md no-hardcoded-AL-values rule. List will go stale when Microsoft adds new page control keywords. `page_controls.json` data file exists for this purpose.
- **Fix:** Use `crate::language_data::page_controls()`.

### CORR-H04: al-syntax — Hardcoded `control_keyword_to_symbol_kind` match
- **File:** `crates/al-syntax/src/symbols.rs:459-469`
- **Impact:** Same class as CORR-H03. New keywords fall through to `SymbolKind::NAMESPACE` silently.
- **Fix:** Add `lsp_symbol_kind` field to `page_controls.json` or document the fallback behavior.

### CORR-H05a: al-syntax — Hardcoded `SINGLE_STMT_OPENERS` in formatting.rs
- **File:** `crates/al-syntax/src/formatting.rs:421-427`
- **Impact:** Encodes AL control-flow keywords (`if/then`, `for/do`, `while/do`, `with/do`, `foreach/do`) directly. Duplicates `tree-sitter-al/data/single_stmt_openers.json`. Will go stale when Microsoft adds new single-statement openers (as happened with `foreach`).
- **Fix:** Load from `LanguageData::single_stmt_openers()`.

### CORR-H05b: al-core — Hardcoded `permission_for_kind` match in permissions.rs
- **File:** `crates/al-core/src/permissions.rs:135-145`
- **Impact:** Hardcodes which object types are permissionable (`table→RIMD`, `page→X`, etc.). New permissionable types silently missed — generated permission sets will be incomplete.
- **Fix:** Add `permission_type`/`permission_value` fields to `object_types.json`, query via `LanguageData`.

### CORR-H05c: al-core — Incomplete `al_keywords` array in code_actions.rs
- **File:** `crates/al-core/src/queries/code_actions.rs:1319-1322`
- **Impact:** Local array of 14 AL keywords used to skip keyword-led lines. Missing `namespace`, `using`, `with`, `foreach`, `do`, `trigger`, `procedure`, `var`, etc. Mixes keywords with comment syntax (`//`) and compound tokens (`end;`).
- **Fix:** Use `al_syntax::language_data::is_keyword()`. Keep `//` and `end;` as explicit prefix checks.

### CORR-H05: al-core — `xliff.rs` hardcoded AL object type list
- **File:** `crates/al-core/src/xliff.rs:189-206`
- **Impact:** Missing newer object types (`controladdin`, `entitlement`, `fieldgroup`, `dotnet`). Translations from these files silently skipped.
- **Fix:** Use `al_syntax::find_object_declaration` instead of matching against hardcoded list.

### CORR-H06: al-symbols — `ControlJson` `Kind` integer not mapped to string names
- **File:** `crates/al-symbols/src/model.rs:559-564`
- **Impact:** Newer BC packages use integer `Kind` values. Code comparing `control.kind` to string literals silently fails for integer-valued kinds.
- **Fix:** Add integer-to-string normalization function.

### CORR-H07: al-symbols — `SymbolReferenceJson` namespace recursion unbounded
- **File:** `crates/al-symbols/src/model.rs:740-743`
- **Impact:** Malicious `.app` with deeply nested namespaces causes stack overflow. Violates iterative traversal rule.
- **Fix:** Convert to iterative with explicit `Vec` stack.

### CORR-H08: zed-al — `extension.toml` version `0.8.0` pinned but `zed_extension_api` on `branch = "main"`
- **File:** `extension.toml:14`, `Cargo.toml:16`
- **Impact:** `cargo update` pulls newer API while extension declares `0.8.0`. Breaking changes upstream silently break the build.
- **Fix:** Pin `zed_extension_api` to a specific `rev`.

## MEDIUM

### CORR-M01: al-syntax — `format_range` end character uses byte length, not UTF-16
- **File:** `crates/al-syntax/src/formatting.rs:358-370`
- **Impact:** `l.len()` returns UTF-8 byte count. LSP `TextEdit.range.end.character` requires UTF-16 code units. Edit range end wrong for non-ASCII lines.
- **Fix:** Use `byte_col_to_utf16_col(line, line.len())`.

### CORR-M02: al-syntax — `collect_dataitem_vars` is case-sensitive for `"dataitem("`
- **File:** `crates/al-syntax/src/type_resolver.rs:607`
- **Impact:** AL keywords are case-insensitive. `DataItem(...)` (uppercase D) not matched.
- **Fix:** `trimmed.to_ascii_lowercase().starts_with("dataitem(")`.

### CORR-M03: al-syntax — `collect_action_trigger_vars` requires `begin` on its own line
- **File:** `crates/al-syntax/src/type_resolver.rs:686-688`
- **Impact:** `trigger OnAction() begin` (same line) not recognized. Variables not collected.
- **Fix:** Check `lower.contains("begin")` instead of exact match.

### CORR-M04: al-syntax — `find_call_context` doesn't skip `//` or `/* */` comments
- **File:** `crates/al-syntax/src/context.rs:153-224`
- **Impact:** `(` inside a `// comment with (` corrupts paren_depth. Wrong function name returned.
- **Fix:** Add comment detection to the backwards scan.

### CORR-M05: al-lsp — `semantic_to_diagnostic` column units unclear
- **File:** `crates/al-lsp/src/diagnostics.rs:343-368`
- **Impact:** .NET bridge column units (byte vs UTF-16) not documented. Raw `saturating_sub(1)` applied.
- **Fix:** Clarify and validate column unit from the bridge.

### CORR-M06: al-lsp — `dispatch_inlay_hints` uses LSP types directly in daemon code
- **File:** `crates/al-lsp/src/daemon/lsp_dispatch.rs:226-232`
- **Impact:** Daemon constructs `tower_lsp::lsp_types::Range` directly. Should use al-core's transport-agnostic type.
- **Fix:** Construct `al_core::queries::Range` instead.

### CORR-M07: al-semantic — `to_string_lossy()` silently corrupts non-UTF-8 DLL paths
- **File:** `crates/al-semantic/src/host.rs:90-91`
- **Impact:** Non-UTF-8 paths have replacement characters injected. CLR fails to load DLL with opaque error.
- **Fix:** Use `path_to_pdcstring` consistently.

### CORR-M08: al-semantic — Hardcoded `net8.0` TFM in `compile_bridge_from_source`
- **File:** `crates/al-semantic/src/host.rs:267-270`
- **Impact:** If bridge project targets `net9.0`, output path wrong. Build succeeds but DLL not found.
- **Fix:** Use `dotnet build -o <output_dir>` to force output location.

### CORR-M09: al-symbols — `parse_symbol_reference_json` double-parses common case
- **File:** `crates/al-symbols/src/app_reader.rs:151-159`
- **Impact:** Most `.app` files have no trailing padding. Streaming deserializer used first, then full re-parse as fallback. Common case pays double parsing cost.
- **Fix:** Use `serde_json::from_slice` as primary path; strip trailing padding before parsing.

### CORR-M10: al-explorer — Mouse handler recomputes layout from `terminal::size()` instead of last frame
- **File:** `crates/al-explorer/src/main.rs:1211-1218`
- **Impact:** Between resize and next draw, click coordinates land in wrong pane.
- **Fix:** Store last rendered layout for hit-testing.
