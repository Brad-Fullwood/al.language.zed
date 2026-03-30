# al-syntax — Parsing & Analysis (~9K lines)

**Leaf crate** — must NOT depend on al-symbols, al-semantic, or al-core.

## Quick Reference

```sh
cargo test -p al-syntax                             # all tests (~179 inline + 2 integration)
cargo test -p al-syntax --test comprehensive        # comprehensive integration tests
cargo test -p al-syntax --test query_validation     # tree-sitter query validation
```

## Key Exports (lib.rs)

- `AlParser`, `ParseResult`, `SyntaxError` — tree-sitter parsing
- `format_al`, `format_range`, `FormatOptions` — formatting with configurable style
- `lint`, `LintDiagnostic`, `LintRuleInfo`, `LintSeverity` — lint rules
- `extract_document_symbols` — symbol extraction from parse tree
- `extract_semantic_tokens`, `SemanticToken` — token classification
- `extract_folding_ranges` — code folding regions
- `find_node_at_position`, `find_definition_node`, `find_references_in_tree` — navigation
- `TypeResolver`, `VariableDecl`, `VariableScope` — type resolution within a single file
- `detect_context`, `CompletionContext`, `extract_last_identifier`, `find_call_context` — completion context
- `walk_tree`, `walk_tree_until` — iterative tree-sitter traversal helpers
- `byte_col_to_utf16_col`, `utf16_col_to_byte_offset` — UTF-16 ↔ byte conversion
- `node_text_clean`, `node_text_or`, `extract_object_name` — tree node text helpers
- `sort_members` — member sorting
- `LanguageData` — **runtime-loaded** AL keywords/builtins/types

## LanguageData (language_data.rs)

Loads ALL AL language knowledge from `tree-sitter-al/data/` JSON files at runtime. This is the ONLY correct source for keywords, builtins, types, and object kinds. NEVER hardcode these values.

## Modules

| File | Purpose |
|------|---------|
| parser.rs | tree-sitter integration, `AlParser` |
| language_data.rs | LanguageData from JSON (NOT hardcoded) |
| type_resolver.rs | type resolution, variable scoping within a file |
| navigation.rs | find_node_at_position, definition, references |
| tokens.rs | semantic token extraction and classification |
| symbols.rs | document symbol extraction |
| formatting.rs | AL code formatter |
| lint.rs | lint rules engine |
| context.rs | completion context detection |
| folding.rs | folding range extraction |
| complexity.rs | cyclomatic complexity calculation |
| traversal.rs | iterative tree-sitter walk helpers |
| sort.rs | member sorting within objects |
