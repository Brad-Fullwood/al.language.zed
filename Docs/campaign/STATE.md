# Campaign state

Updated: 2026-09-21 01:30 BST. Branch: `campaign/2026-09-21`. Ends: 2026-09-28.

## Phase

Round 1: four of seven reviews are done (58 findings) and their fix agents are running. Three reviews and two follow-up reviews are still running.

## In flight

| Item | Kind | Output |
|------|------|--------|
| Fix R1 syntax and grammar (11 findings) | worktree fix | branch `campaign/fix-r1-syntax-grammar` |
| Fix R1 symbols and project layer (24 findings) | worktree fix | branch `campaign/fix-r1-symbols-project` |
| Fix R1 emit, compile, BC, explorer (28 findings) | worktree fix | branch `campaign/fix-r1-emit-bc-explorer` |
| Fix R1 extension, CI, docs (12 findings) | worktree fix | branch `campaign/fix-r1-extension-ci` |
| Fix R1 analysis and insight (18 findings) | worktree fix | branch `campaign/fix-r1-analysis-insight` |
| Fix R1 LSP and protocol (15 findings, 2 security) | worktree fix | branch `campaign/fix-r1-lsp-protocol` |
| Fix R1 runtime and DAP (13 findings) | worktree fix | branch `campaign/fix-r1-runtime-dap` |
| Build the Claude Code plugin (`plugin/`, marketplace manifest, 8 skills, 2 subagents) | worktree build | branch `campaign/ai-plugin` |
| desloppify first scan | shell | `.campaign/desloppify-scan.log` |

Queued for a free build slot (at most 7 building agents, RAM is the limit):

- Fix `findings/r1b-runtime-dap.md` (28 findings, 1 high: `--filter '*Post'` drops tests from a green summary). Crates: al-test, al-dap, al-test-harness, al-runtime stubs. Start after `campaign/fix-r1-runtime-dap` merges, on a branch from the merged result.
- AI tooling build list items 1, 2, 3, 5, 7, 8 (`findings/ai-tooling-ideas.md` section 6: limit and fields projection, `scope` parameter, `source --list-procedures`, fix `subscribers` and `impact --table`, index progress, compact JSON). They edit the daemon dispatchers, so start after `campaign/fix-r1-lsp-protocol` merges. Item 4 (free object ID allocator, new file) can start as soon as a build slot frees. Items 9 and 10 (package version diff, persisted symbol index) later in the week.
- Fix `findings/r1b-analysis-insight.md` (35 findings: quoted identifiers cannot be renamed, rename misses EventSubscriber strings, fields with `)` in the name dropped from resolution). Start after `campaign/fix-r1-analysis-insight` merges.
- Fix `findings/r1c-analysis-insight.md` (29 findings, 5 high: unreachable coverage pass keyed on node kinds the grammar does not have, multi-object files read as first object only by 14 consumers, permission audit recommends dropping needed permissions, `Table::` where AL needs `Database::`, signature help picks a local procedure over the receiver's). Same crate as the analysis fix branch, start after it merges.
- Fix `findings/r1b-scaffold-generators.md` (11 findings): handed to the analysis fix agent.

A review file without a `## Review complete` line means the agent died. Re-dispatch it to
continue from the unticked coverage items. A fix branch on origin with findings still `open`
means the fix agent died. Re-dispatch a fix agent onto that branch for the open findings.
Merge a fix branch into `campaign/2026-09-21` only after the full gates pass on the merge.

Disk: 32 GB free at 02:20 on 2026-09-21. Fix agents build with `CARGO_PROFILE_DEV_DEBUG=0`
and `-p` package filters. Remove merged worktrees and their target directories promptly.

## Workstreams

Effort is spread across all workstreams. Each orchestrator session advances at least three of
them and picks the item with the most useful output next. No workstream waits for another to
finish. Every workstream reaches a usable state by 2026-09-25, and the last three days deepen
whichever ones pay off most. Record progress per workstream below so gaps are visible.

| # | Workstream | Progress | Next step |
|---|------------|----------|-----------|
| A | Correctness: review rounds, triage, fixes with a failing test first | R1 reviews running | Triage each `findings/r1-*.md` as it completes, dispatch fix agents per crate group |
| B | Old audit: mark each of the 227 `AUDIT-BACKLOG.md` findings fixed or open | R1 reviewers report still-open ones | Collect `[STILL-OPEN]` tags, queue them under A |
| C | Slop and simplification: desloppify plan, per-crate simplify pass (al-analysis 47k lines, al-lsp 37k, al-runtime 20k first) | first scan: objective 83.4, strict 20.9, 1243 issues. Triage agent running | Fix batches from `findings/desloppify.md` after R1 fix branches merge |
| D | Security: credentials in al-bc and al-publish, `.app` and zip parsing, MCP and daemon input, extension binary download, `cargo deny`, `cargo audit` | covered in part by R1 | Dedicated security review after R1 triage |
| E | Tests: coverage by crate, property tests for parser and interpreter, `cargo mutants` on al-runtime and al-analysis | not started | Measure coverage, list the weakest modules |
| F | Grammar: corpus tests, query drift between `languages/al` and `tree-sitter-al/queries` | R1 review running | From R1 findings |
| G | AI tooling: make this project speed up and sharpen AI work on Business Central (see below) | Inventory, measurements and design done (`findings/ai-tooling-ideas.md`): latency is 4 to 150 ms warm, but 14 of 20 measured answers are too large for an agent (up to 9.4 MB). Plugin build running | Daemon projection work after the LSP fix branch merges |
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
