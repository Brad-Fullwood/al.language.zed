# R1 review: al-syntax and tree-sitter-al

Adversarial read-only review of `crates/al-syntax`, the `tree-sitter-al` submodule
(grammar, scanner, queries, generator, bindings) and `languages/al/*.scm`.
Baseline: the 2026-07-31 audit in `AUDIT-BACKLOG.md` section "Syntax & Grammar".
Findings already fixed in current code are not repeated. Still-open audit items
are tagged `[STILL-OPEN]`.

## Coverage

- [x] AUDIT-BACKLOG.md "Syntax & Grammar" section
- [x] tree-sitter-al/grammar.js
- [x] tree-sitter-al/src/scanner.c
- [x] tree-sitter-al/queries/*.scm (7 files)
- [x] languages/al/*.scm + config.toml + semantic_token_rules.json + tasks.json
- [ ] tree-sitter-al/bindings/rust
- [ ] tree-sitter-al/generator/
- [ ] tree-sitter-al/test/corpus + tests/
- [ ] crates/al-syntax/src/lib.rs
- [ ] crates/al-syntax/src/parser.rs
- [ ] crates/al-syntax/src/tokens.rs
- [ ] crates/al-syntax/src/type_resolver.rs
- [ ] crates/al-syntax/src/symbols.rs
- [ ] crates/al-syntax/src/formatting.rs
- [ ] crates/al-syntax/src/sort.rs
- [ ] crates/al-syntax/src/lint.rs
- [ ] crates/al-syntax/src/navigation.rs
- [ ] crates/al-syntax/src/complexity.rs
- [ ] crates/al-syntax/src/folding.rs
- [ ] crates/al-syntax/src/context.rs
- [ ] crates/al-syntax/src/language_data.rs
- [ ] crates/al-syntax/src/lexical.rs
- [ ] crates/al-syntax/src/traversal.rs
- [ ] crates/al-syntax/src/types.rs
- [ ] crates/al-syntax/benches/parser.rs

## Findings
