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

## Workstreams

Effort is spread across all workstreams. Each orchestrator session advances at least three of
them and picks the item with the most useful output next. No workstream waits for another to
finish. Every workstream reaches a usable state by 2026-09-25, and the last three days deepen
whichever ones pay off most. Record progress per workstream below so gaps are visible.

| # | Workstream | Progress | Next step |
|---|------------|----------|-----------|
| A | Correctness: review rounds, triage, fixes with a failing test first | R1 reviews running | Triage each `findings/r1-*.md` as it completes, dispatch fix agents per crate group |
| B | Old audit: mark each of the 227 `AUDIT-BACKLOG.md` findings fixed or open | R1 reviewers report still-open ones | Collect `[STILL-OPEN]` tags, queue them under A |
| C | Slop and simplification: desloppify plan, per-crate simplify pass (al-analysis 47k lines, al-lsp 37k, al-runtime 20k first) | first scan running | Read the scan, record the score, start `desloppify next` |
| D | Security: credentials in al-bc and al-publish, `.app` and zip parsing, MCP and daemon input, extension binary download, `cargo deny`, `cargo audit` | covered in part by R1 | Dedicated security review after R1 triage |
| E | Tests: coverage by crate, property tests for parser and interpreter, `cargo mutants` on al-runtime and al-analysis | not started | Measure coverage, list the weakest modules |
| F | Grammar: corpus tests, query drift between `languages/al` and `tree-sitter-al/queries` | R1 review running | From R1 findings |
| G | AI tooling: make this project speed up and sharpen AI work on Business Central (see below) | not started | Inventory the MCP and CLI surface, design the plugin |
| H | Docs: `Docs/`, `README.md`, `ROADMAP.md` match the code, then unsloppify | R1 docs review running | From R1 findings |
| I | Blog: replace the six articles with a new series on the current project, unsloppify each | inventory done | Outline the series, work on a blog branch, merge to `main` only when ready (Vercel deploys `main`) |

### G: AI tooling detail

Goal: an AI agent working on a BC codebase gets answers from this project's symbol index, call
and event graphs, and impact analysis in one tool call. Today the same agent decompiles `.app`
files by hand and greps for symbols.

- Inventory what al-lsp's MCP server, the daemon catalog (`al_call`) and al-explorer already
  expose. Measure them on a real workspace: latency, output size in tokens, accuracy.
- Find the gaps for agent use: symbol lookup across dependency `.app` packages without
  extraction, "who subscribes to this event", "what breaks if I change this field", table and
  field lookup by name or ID, object ID range allocation, source of a base-app procedure.
- Build a Claude Code plugin in this repository (skills, MCP config, agents) that packages
  those tools with instructions for when to use each. Output must be compact, since token
  cost decides whether an agent uses a tool.
- Propose new skills and ideas freely. Record them in `findings/ai-tooling-ideas.md` and build
  the ones with the best payoff.
- `~/Projects/tools` is Brad's personal tooling and stays external. Nothing moves out of this
  repository into it. Its skills (for example `bc-build-deploy`) may call this project's
  binaries where that helps them.

After each round: adversarial review of everything the campaign changed, then a new review
round on the areas with the most findings.

## Done

- Campaign branch, protocol docs, watchdog timer, heartbeat hook, desloppify install.

## Baseline (2026-09-21)

- `cargo clippy --workspace --all-targets`: clean.
- `cargo test --workspace`: 80 suites, 4380 passed, 0 failed, 10 ignored.
