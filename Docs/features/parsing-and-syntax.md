# Parsing & Syntax Engine

**Module:** `crates/al-syntax/src/` · **Status:** ✅ shipped (formatter & lint have gaps, noted below)

The syntax layer is the foundation everything else stands on. It wraps the bundled `tree-sitter-al`
grammar and turns parse trees into the structured information that completions, hover, definitions,
symbols, folding, formatting, and analysis all consume. It is transport-agnostic: it speaks in
`SyntaxRange`/`SyntaxPosition`, never in LSP types.

## Overview

| Capability | File | What it produces |
| --- | --- | --- |
| Parser wrapper | `parser.rs` | `ParseResult` (tree + errors), incremental & thread-local "quick" parsing |
| Semantic token extraction | `tokens.rs` | 43 AL-aware token classes, delta-encoded for LSP |
| AST navigation | `navigation.rs` | node-at-position, object/procedure info, reference collection |
| Folding | `folding.rs` | foldable regions (blocks, procedures, comments) |
| Document symbols | `symbols.rs` | hierarchical outline (objects → members) |
| Completion context | `context.rs` | member/enum/type/default context detection |
| Type resolution | `type_resolver.rs` | variable/field type inference in scope |
| Member sort | `sort.rs` | canonical member ordering |
| Complexity | `complexity.rs` | cyclomatic + cognitive complexity per procedure |
| Formatting | `formatting.rs` | indentation/keyword-casing formatter |
| Lint framework | `lint.rs` | rule/diagnostic types (currently an empty engine — see below) |
| Language data | `language_data.rs` | data-driven keyword/builtin/type tables |
| Traversal & encoding | `traversal.rs`, `mod.rs` | tree walking + UTF-16 ⇄ byte conversion |

## How it works

### Parser (`parser.rs`)

`AlParser` links the compiled C grammar via `extern "C" { fn tree_sitter_al() -> Language; }` and
offers three entry points:

- `parse(text)` — full parse → `ParseResult`.
- `parse_incremental(text, old_tree)` — reuses the previous tree for edited documents (this is what
  keeps editing fast under the document store).
- `parse_quick(text)` — a thread-local cached parser for one-shot use.

Errors are collected from both `is_error()` nodes and `is_missing()` nodes (the latter reported as
`Missing {kind}`), so syntactic diagnostics reflect both garbage and absent-but-required tokens.

### Semantic tokens (`tokens.rs`)

A single tree walk classifies every meaningful node into one of **43** token types — the 12 standard
LSP types plus 31 AL-specific ones (e.g. `OBJECT_KEYWORD`, `BUILTIN_TYPE`, `TABLE_FIELD`,
`PAGE_CONTROL`, `PAGE_ACTION`, `TRIGGER_NAME`, `EVENT_CREATION`, `EVENT_SUBSCRIPTION`,
`QUERY_DATA_ITEM`, `XMLPORT_FIELD_ELEMENT`, `EXCLUDED_CODE`, …). Tokens are delta-encoded per the LSP
spec, and byte offsets are converted to UTF-16 columns via a per-line lookup table. Multi-line block
comments and strings emit one token per line. Context-sensitive cases (e.g. `key_declaration`
classified as `QUERY_DATA_ITEM` vs `TABLE_KEY` vs `XMLPORT_TABLE_ELEMENT` based on its keyword child)
are handled by inspecting siblings rather than guessing from text. The traversal collects children
via a cursor and walks them in reverse for a stack-based DFS to avoid O(n²) behavior.

### Navigation (`navigation.rs`)

Position-aware helpers used by go-to-definition, references, and rename:

- `find_node_at_position` — LSP position → tree-sitter node (UTF-16 → byte conversion).
- `find_object_declaration` → `ObjectInfo { kind, id, name, range }`.
- `find_procedure_at` → `ProcedureInfo { name, parameters, return_type, is_local, range }`.
- `find_variable_references`, `find_call_references`, `find_event_subscriber_references`,
  `collect_call_site_names` — reference collection with exact-span dedup.

Because object names are not a named field in the grammar, `extract_object_name` scans children for
identifier/quoted-identifier/string/name nodes.

### Type resolution (`type_resolver.rs`)

`TypeResolver` collects all variables visible at a cursor — local, parameter, global, implicit
`self`, and trigger-implicit (Rec, xRec, CurrPage, …) — and resolves a name to a
`VariableDecl { name, type_name, type_subtype, is_var, scope, range }`. Object kinds map to AL types
(`table`/`tableextension` → `Record`, `page`/`pageextension` → `Page`, etc.), and tables expose
implicit `Rec`/`xRec` in triggers and pages. This is what powers member-access completion, hover, and
the `with`-elimination refactor.

### Complexity (`complexity.rs`)

`compute_complexity` returns per-procedure cyclomatic complexity (decision points + 1) and cognitive
complexity (Sonar-style nesting-weighted). Decision sources: `if`, `for`, `foreach`, `while`,
`repeat`, each `case` branch, and `and`/`or` operators. It does not recurse into nested procedures.
Exposed via `al-explorer metrics`.

### Formatting (`formatting.rs`)

A keyword-driven state machine (`format_al`, `format_range`) that reindents AL using `begin`/`end`,
`var`, `if`/`then`, `repeat`/`until`, `case`/`of`, paren depth, and property-continuation tracking.
`FormatOptions` honored today: `tab_size`, `insert_spaces`, `keyword_casing`
(Preserve/Lower/Upper). Parsed-but-not-yet-implemented options:
`blank_lines_between_procedures`, `max_line_length`, `brace_style`, `sort_properties` — the format
query logs a `tracing::warn!` when one of these is set (honesty over silence).

### Sort (`sort.rs`)

`sort_members` is a text-based reorder (no tree needed) producing canonical order: `var` block
(verbatim), then triggers alphabetically, then procedures alphabetically (including
local/internal/protected), preserving the object header/footer. Returns `None` if the text is not a
single, non-nested object.

### Language data (`language_data.rs`)

All AL vocabulary — keywords, builtin functions, object types, implicit variables, page controls,
single-statement openers, token classification — is loaded **from JSON** in `tree-sitter-al/data/`
via `LazyLock` singletons, not hard-coded. This is how the project keeps parity with Microsoft's
keyword/type/builtin lists from a single source of truth.

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| Grammar | tree-sitter AL grammar (generated), used natively in Rust | TextMate grammar for highlighting + the .NET compiler for structure |
| Tokenization | 43-class semantic tokenizer, AL-aware | TextMate scopes + server semantic tokens |
| Reusability | tokens/nav/complexity available to LSP, CLI, MCP, and analysis | coupled to the VS Code extension/server |
| Complexity metrics | built-in (`metrics`) | not provided |
| Formatter | native, fast, partial option coverage | compiler/extension formatter (more complete) |

## Why this approach

A tree-sitter front end gives error-resilient, incremental parsing that runs in-process in Rust on
every surface — the editor, the CLI, CI, and AI tools — instead of being locked behind a .NET
language server. Driving the vocabulary from generated JSON data (rather than hard-coded lists) keeps
the parser and the analysis layer in sync with the grammar and avoids the classic "the highlighter
and the analyzer disagree about what a keyword is" drift.

## How to use

- **Implicitly** through every editor feature (highlighting, outline, folding) and analysis command.
- **`al-explorer parse <file>`** — node/error counts and parse time.
- **`al-explorer tokens <file>`** — semantic tokens.
- **`al-explorer metrics <file> [--all] [--threshold-cyclomatic N] [--threshold-cognitive N]`** —
  complexity (Zed task: *AL: Complexity Metrics*).
- **`al-explorer format <file> [--check] [--all]`** and **`sort-members`** — formatting/sort.

## Limitations

- ⛔ **Native lint engine is empty.** `lint.rs` defines `LintDiagnostic`/`LintSeverity`/
  `LintRuleInfo` and the `lint()`/`lint_rules()` entry points, but they return empty — all AL
  diagnostics today come from syntax parse errors and the semantic bridge. Settings
  `al.enableNativeLint`/`al.nativeLintRules` are parsed but inert.
- 🟡 Formatter ignores four `FormatOptions` fields (blank lines, max line length, brace style,
  property sort).
- Complexity does not descend into nested procedures.

## Roadmap

From `ROADMAP.md` (Native Lint And Diagnostics): implement a high-value native rule set (unsafe
`FindFirst` in loops, missing `SetLoadFields`, missing `ApplicationArea`/`DataClassification`/
tooltips, obsolete usage, architecture-layer violations), keep native lint output distinct from
semantic-compiler diagnostics, add tests proving disabled rules stay disabled, and keep
`schemas/settings.json` / `docs/settings.md` / README wording in lockstep with actual behavior —
or rename/remove the inert settings until the engine starts.
