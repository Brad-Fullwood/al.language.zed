# R1 review: al-analysis and al-insight

Adversarial read-only review. Baseline: AUDIT-BACKLOG.md section "Analysis & Insight" (2026-07-31).
Findings marked [STILL-OPEN] were in that audit and are still present in current code.

## Coverage

Code actions (edit user files) — highest priority:
- [ ] queries/code_actions/mod.rs
- [ ] queries/code_actions/add_parens.rs
- [ ] queries/code_actions/if_to_case.rs
- [ ] queries/code_actions/with_elimination.rs
- [ ] queries/code_actions/implement_interface.rs
- [ ] queries/code_actions/events.rs
- [ ] queries/code_actions/promoted.rs
- [ ] queries/code_actions/make_local.rs
- [ ] queries/code_actions/namespace.rs
- [ ] queries/code_actions/doc_region.rs
- [ ] queries/code_actions/test_support.rs
- [ ] queries/bulk_fix.rs
- [ ] queries/rename.rs
- [ ] generators.rs / scaffold.rs (file-writing)
- [ ] xliff.rs (writes user .xlf)

Diagnostics:
- [ ] queries/diagnostics.rs
- [ ] queries/sql_patterns.rs
- [ ] queries/dead_code.rs
- [ ] queries/arch_lint.rs
- [ ] queries/transaction_lint.rs
- [ ] queries/native_check.rs
- [ ] queries/audit.rs
- [ ] queries/breaking_changes.rs
- [ ] queries/obsolescence.rs / obsolete_usage.rs
- [ ] queries/test_diagnostics.rs
- [ ] queries/duplicates.rs
- [ ] queries/upgrade.rs
- [ ] queries/impact.rs
- [ ] permissions.rs
- [ ] workspace_sources.rs
- [ ] resolution.rs
- [ ] queries/source.rs
- [ ] queries/inlay_hints.rs, code_lens.rs, hover.rs, completions.rs, signature.rs
- [ ] queries/suggest_event.rs, profiler_hints.rs, test_coverage.rs

Insight:
- [ ] al-insight/src/graph.rs
- [ ] al-insight/src/calls.rs
- [ ] al-insight/src/search.rs
- [ ] al-insight/src/analysis.rs
- [ ] al-insight/src/index.rs
- [ ] al-insight/src/discovery.rs

## Findings

