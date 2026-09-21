# Campaign state

Updated: 2026-09-21 01:30 BST. Branch: `campaign/2026-09-21`. Ends: 2026-09-28.

## Phase

Round 1: review. Seven subsystem reviews run in parallel and write to `findings/`.

## In flight

| Item | Agent | Output |
|------|-------|--------|
| R1 review: syntax and grammar | background agent | `findings/r1-syntax-grammar.md` |
| R1 review: analysis and insight | background agent | `findings/r1-analysis-insight.md` |
| R1 review: LSP and protocol | background agent | `findings/r1-lsp-protocol.md` |
| R1 review: symbols and project layer | background agent | `findings/r1-symbols-project.md` |
| R1 review: runtime, test, DAP | background agent | `findings/r1-runtime-dap.md` |
| R1 review: emit, compile, BC, explorer | background agent | `findings/r1-emit-bc-explorer.md` |
| R1 review: extension, CI, scripts, security | background agent | `findings/r1-extension-ci-security.md` |
| desloppify first scan | shell | `.campaign/desloppify-scan.log` |

If a review file exists but has no `## Review complete` line, the agent died. Re-dispatch it
with the instruction to read the file and continue from the areas not yet covered.

## Queue (top first)

1. Triage round 1 findings: verify each, reject false positives, group into fix batches by crate.
2. Fix batches, one worktree agent per crate group, each fix with a failing test first.
3. Verify `AUDIT-BACKLOG.md` (2026-07-31, 227 findings): mark each finding fixed or open
   against current code, then queue the open ones.
4. desloppify: work the plan from the first scan (`desloppify next`), rescan, record the score.
5. Simplification pass per crate (largest first: al-analysis 47k lines, al-lsp 37k, al-runtime 20k).
6. Security review: credential handling in al-bc and al-publish, `.app` and zip parsing,
   MCP and daemon input, extension binary download, `cargo deny`, `cargo audit`.
7. Test depth: coverage by crate, property tests for the parser and interpreter, `cargo mutants`
   on al-runtime and al-analysis hot paths.
8. Grammar: corpus tests (the audit found one test file), query drift between
   `languages/al` and `tree-sitter-al/queries`.
9. Docs: bring `Docs/`, `README.md`, `ROADMAP.md` in line with the code, then unsloppify.
10. Blog: delete the six articles in `technically-business-central/src/content/blog/en/`,
    write the new series from the current state of this project, unsloppify each article.
11. `~/Projects/tools` is Brad's personal tooling, external to this project. Nothing moves out
    of this repository into it. Its skills may call this project's binaries (al-lsp,
    al-explorer, the grammar) where that helps them. Check whether any do or should.
12. Repeat: adversarial review of everything the campaign changed, then a new review round.

## Done

- Campaign branch, protocol docs, watchdog timer, heartbeat hook, desloppify install.

## Baseline (2026-09-21)

- `cargo clippy --workspace --all-targets`: clean.
- `cargo test --workspace`: 80 suites, 4380 passed, 0 failed, 10 ignored.
