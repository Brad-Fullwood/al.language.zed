# Plan: Full CLI Parity + Agentic Test Coverage

## Context

The CLI should be able to do everything the LSP can — it's the second frontend, not a subset. Currently 5 LSP features have no CLI equivalent: folding ranges, semantic tokens, inlay hints, code actions/quickfixes, and semantic diagnostics. This plan adds those missing CLI commands, then uses the full CLI + in-process tests to achieve near-100% test coverage across all crates.

**Current state:** 402 passing tests, 1 failing. 22 CLI commands. No coverage measurement.

## Part A: Missing CLI Commands

### A1. `al folding FILE` — Folding ranges
- Calls `al_syntax::extract_folding_ranges(&tree, &text)` (already public)
- JSON: `[{ start_line, end_line, kind }]`
- Human: `line X..Y (kind)` per range
- **Trivial wrapper, ~30 lines**

### A2. `al tokens FILE` — Semantic tokens
- Calls `al_syntax::extract_semantic_tokens(&tree, &text)` (already public)
- JSON: `[{ line, character, length, token_type, modifiers }]` (absolute positions, not deltas)
- Human: summary stats (`42 tokens: 10 keywords, 8 identifiers, ...`)
- **Trivial wrapper, ~40 lines**

### A3. `al hints FILE [--range START_LINE END_LINE]` — Inlay hints
- Currently in `handlers.rs:831` as `handle_inlay_hint()` — tightly coupled to `AlServer` (needs symbol index + builtins + doc symbols)
- **Extraction needed**: Move the core hint-collection logic from `handlers.rs` into a shared function in `al-syntax` (or keep in al-lsp but make it callable with explicit params instead of `&AlServer`)
- CLI version: parse file, extract doc symbols, look up parameter names from:
  1. Local procedures (from doc symbols)
  2. Package symbols (from SymbolIndex via CliWorkspace)
  3. Built-in types (from al-semantic if available, empty otherwise)
- JSON: `[{ position: {line, character}, label, kind }]`
- Human: `line:col paramName:` per hint
- **Medium effort, ~80 lines + extraction**

### A4. `al fix FILE [--all] [--dry-run] [--rule CODE]` — Code actions/quickfixes
- Currently in `handlers.rs:255-824` — 10 diagnostic-based quickfixes + 2 source actions
- **Extraction needed**: Move quickfix compute functions from `handlers.rs` to a shared module (either `al-syntax::actions` or `al-lsp::actions` re-exported)
  - Pure functions to extract: `compute_remove_begin_end`, `compute_pascal_case_fix`, `compute_extract_to_label`, `generate_label_name`, `detect_indent`
  - Helper functions: `make_quickfix`, `diag_code_str`
  - Source actions: `source_action_add_doc_comment`, `source_action_add_region`
- CLI workflow: lint file → match diagnostics to available fixes → apply or show edits
- `--dry-run`: output JSON edits without applying
- `--rule AL-L016`: only apply fixes for specific rule
- `--all`: apply all available fixes and write file
- JSON: `[{ title, kind, rule, edits: [{ line, character, end_line, end_character, new_text }] }]`
- Human: `Applied 3 fixes: PascalCase (2), Remove TODO (1)`
- **High effort, ~150 lines + extraction of ~200 lines from handlers.rs**

### A5. `al lint FILE --semantic` — Semantic diagnostics (requires .NET)
- Extends existing `lint` command with `--semantic` flag
- Spawns `al_semantic::SemanticBridge`, calls `bridge.analyze()`
- Only runs if ALTool is installed (graceful error otherwise)
- JSON: same format as lint but with additional entries from semantic analysis
- **Medium effort, ~50 lines in lint command + error handling**

### A6. `al parse FILE` — Parse tree info
- Not an LSP feature per se, but useful for debugging and completeness
- Output: syntax errors from tree-sitter, node counts, parse time
- JSON: `{ errors: [{line, column, message}], nodes: count, parse_time_ms }`
- Human: `Parsed in 2ms, 142 nodes, 0 errors`
- **Trivial, ~30 lines**

### Summary of new commands

| Command | Effort | Extraction from handlers.rs | al-syntax function |
|---------|--------|---------------------------|-------------------|
| `al folding FILE` | Trivial | None | `extract_folding_ranges()` |
| `al tokens FILE` | Trivial | None | `extract_semantic_tokens()` |
| `al hints FILE` | Medium | `collect_inlay_hints` logic | `extract_document_symbols()` + symbol lookup |
| `al fix FILE` | High | 10 quickfixes + 2 source actions (~400 lines) | `lint()` + quickfix compute fns |
| `al lint --semantic` | Medium | None | `al_semantic::SemanticBridge::analyze()` |
| `al parse FILE` | Trivial | None | `AlParser::parse()` |

## Part B: Test Coverage Strategy

### B0. Coverage Baseline (Opus, 1 agent)
1. Run `cargo tarpaulin --workspace --exclude zed-al --out Html --output-dir coverage/ --skip-clean`
2. Record per-crate percentages
3. Create `scripts/coverage.sh` for repeatable runs

### B1. Test Fixture Expansion (Haiku, 1 agent)
Create 10 AL files in `test_al_project/src/` covering all object types:

| File | Purpose |
|------|---------|
| `Table50100.al` | Table with fields, keys, field triggers |
| `Page50100.al` | Card page referencing Table50100 |
| `Enum50100.al` | Enum with values |
| `Interface50100.al` | Interface definition |
| `CodeunitWithEvents.al` | IntegrationEvent publisher + subscriber |
| `PageExtension50100.al` | Page extension |
| `TableExtension50100.al` | Table extension |
| `ErrorCases.al` | Deliberate syntax errors |
| `DeepNesting.al` | 10+ levels of nesting |
| `MultiProcedure.al` | 10+ procs, locals, globals |

### B2. al-lsp In-Process Handler Tests (Sonnet, 3 parallel agents)
Uses `test_server()` from `server.rs:406` — creates `AlServer` without I/O.

**Agent 2A — completions.rs** (15 tests)
- All 5 `CompletionContext` variants with pre-populated symbols/builtins

**Agent 2B — definition.rs** (20 tests)
- `handle_definition`, `handle_references`, `handle_rename`, `handle_prepare_rename`
- Local vars, cross-file, package symbols, quoted identifiers

**Agent 2C — workspace.rs + formatting.rs + parsing.rs + lifecycle** (15 tests)
- `handle_workspace_symbol`, `handle_formatting`, `get_or_parse` cache, `did_open`/`did_change`/`did_close`

### B3. al-cli Integration Tests (Haiku, 2 parallel agents)
Subprocess tests using `al_binary()` pattern from `crates/al-cli/tests/integration.rs`.

**Agent 3A — Position-based + new commands** (20 tests)
- `symbols`, `hover`, `definition`, `references`, `signature`, `completions`, `rename`
- New: `folding`, `tokens`, `hints`, `fix`, `parse`
- Error paths: missing file, empty file, invalid position

**Agent 3B — Symbol queries + batch ops** (18 tests)
- `search`, `object`, `by-id`, `events`, `subscribers`, `composed`, `packages`, `deps`
- `lint --all`, `format --check --all`, `doctor`
- Run from test_al_project/ with .alpackages

### B4. al-semantic Mock Bridge (Sonnet, 1 agent)
- Mock binary in `crates/al-semantic/tests/` that responds to JSON-RPC
- Tests: happy path, builtin_types parsing, timeout, crash recovery
- **10 tests**

### B5. Edge Cases (Haiku, 1 agent)
- Empty files, large files, Unicode identifiers, binary input, nonexistent paths
- **12 tests**

### B6. Final Measurement + Gap Fill (Opus, 1 agent)
- Re-run tarpaulin, compare to baseline, fill remaining <50% gaps
- Fix the 1 failing test (`test_diagnostics_lint_empty_begin_end`)
- **~8 targeted tests**

## Execution Order

```
Phase 0: Coverage baseline (Opus, 1 agent)
Phase 1: Test fixtures (Haiku, 1 agent)
    ↓
Phase 2: CLI new commands — A1-A6 (Sonnet, 2-3 agents)
Phase 3: al-lsp handler tests (Sonnet, 3 agents)     } parallel after Phase 1
Phase 4: al-cli integration tests (Haiku, 2 agents)   }
Phase 5: al-semantic mock (Sonnet, 1 agent)            }
    ↓
Phase 6: Edge cases (Haiku, 1 agent)
Phase 7: Final measurement + gap fill (Opus, 1 agent)
```

### Agent Tier Strategy

| Tier | When to use | Cost |
|------|------------|------|
| **Haiku** | Fixture files, mechanical subprocess tests, edge case tests | Lowest |
| **Sonnet** | Handler tests with exact patterns, CLI command implementation with clear specs | Medium |
| **Opus** | Coverage analysis, extraction architecture (what moves where), gap identification | Highest — use sparingly |

## Key Files to Modify

| File | Changes |
|------|---------|
| `crates/al-cli/src/main.rs` | Add 6 new commands (folding, tokens, hints, fix, parse, lint --semantic) |
| `crates/al-lsp/src/handlers.rs` | Extract quickfix/action functions to shared module |
| `crates/al-syntax/src/lib.rs` | Possibly add `pub mod actions;` if quickfixes move here |
| `crates/al-cli/tests/integration.rs` | +38 subprocess tests |
| `crates/al-lsp/src/completions.rs` | +15 unit tests |
| `crates/al-lsp/src/definition.rs` | +20 unit tests |
| `crates/al-lsp/src/workspace.rs` | +15 unit tests |
| `crates/al-semantic/tests/` | Mock bridge binary + 10 tests |
| `test_al_project/src/` | 10 new AL fixture files |
| `scripts/coverage.sh` | Coverage measurement script |

## Target Coverage

| Crate | Before (est.) | After |
|-------|--------------|-------|
| al-discovery | ~70% | 85%+ |
| al-syntax | ~75% | 90%+ |
| al-symbols | ~65% | 80%+ |
| al-semantic | ~30% | 55%+ |
| al-dap | ~35% | 40%+ |
| al-lsp | ~50% | 85%+ |
| al-cli | ~25% | 80%+ |

**Expected: ~525 total tests (402 existing + ~120 new), 28 CLI commands (22 existing + 6 new)**

## Verification

1. `cargo test --workspace` — 0 failures
2. `cargo tarpaulin --workspace --exclude zed-al` — per-crate meets targets
3. `cargo build` — no warnings
4. Each new CLI command smoke-tested on test fixtures
5. `al --help` shows all 28 commands
