# Campaign state

Updated: 2026-09-21 01:30 BST. Branch: `campaign/2026-09-21`. Ends: 2026-09-28.

## Phase

Round 2. Every round 1 finding is fixed, rejected with evidence, or queued. Two adversarial reviewers read the whole campaign diff (`git diff dev..campaign/2026-09-21`, 277 files) looking for fixes that do not fix, merge damage and regressions. A fix agent works the queued items.

## In flight

| Item | Kind | Output |
|------|------|--------|
| desloppify review queue: work all 112 subjective items to empty (resolve, skip with reason, or defer items in files other branches hold), then rescan and record scores | worktree refactor | branch `campaign/slop-review-queue`, `findings/slop-review-queue.md` |
| Fix R2 review B (19: rename and snapshot capture skip the gates, `.alpackages` symlink moves the boundary, extension path check unnormalised, plus a structural test that every dispatcher passes its gate) | worktree fix | branch `campaign/fix-r2-review-b` |
| Fix queued items plus R2 review A (14: tooltip byte slice panic on localized packages, dead `skipped` and `parseIssues` reports, ID 50000 boundary, merge leftovers) | worktree fix | branch `campaign/fix-queued-2` |
| Blog: articles 4 (`native-app-emitter`) and 8 (`zed-extension-and-release-integrity`) | blog repo branch `campaign/2026-09-rewrite` | `findings/blog-progress.md` |
| R3 security review: break project trust and the daemon lifecycle as a hostile repository or a local process, MCP prompt injection vectors | review | `findings/r3-security.md` |

Draft PR: https://github.com/Brad-Fullwood/al.language.zed/pull/30 (base `dev`, CI runs on every push).

Queued:



- 124 `trim_matches('"')` identifier cleanups in al-analysis (102) and al-insight (22), replace with `al_syntax::node_text_clean`. Start after both analysis fix branches merge.
- al-dap and al-publish post to different BC dev endpoints (needs a live server to settle). The duplicated response validation framework in al-explorer (about 300 lines to move).
- desloppify fix batches (`findings/desloppify.md` section 4), file splits after the owning fix branch merges.
- Workstream E (tests): coverage by crate, property tests for parser and interpreter, `cargo mutants` on al-runtime and al-analysis. Start when a build slot frees.
- Workstream D (security): dedicated review after round 1 fixes are in, covering what changed.
- `al-explorer packages` prints per-package source counts keyed by folded display name (`al-symbols` `index.rs:259`), so two `System` packages with one app id and different versions both show `0/502/1` against 529 manifest objects. Key by app id and version. Details in `findings/blog-progress.md`.
- Blog: article 1 repeats a wrong diagnosis of the `trace` timeout (it is the cold call-graph build, 86 s with Base Application). Rewrite that paragraph. Articles 2, 4, 5, 8 after the daemon work and fix branches merge, then article 9, then the fact pass list in `findings/blog-progress.md`.
- README.md line 339 omits `publish` from the CLI list. Rebuild `target/release` before measuring for articles (it predates `publish` and `free-ids`).
- AI tooling build items 9 and 10: dependency package version diff, persisted symbol index (2.9 GB RSS and 54 s cold index today).
- Plugin leftovers: `plugin/evals/`, release binary download hook, test on a project with `.alpackages`.

A review file without a `## Review complete` line means the agent died. Re-dispatch it to
continue from the unticked coverage items. A fix branch on origin with findings still `open`
means the fix agent died. Re-dispatch a fix agent onto that branch for the open findings.
Merge a fix branch into `campaign/2026-09-21` only after its gates pass on the merge.

## Workstreams

Effort is spread across all workstreams. Each orchestrator session advances at least three of
them and picks the item with the most useful output next. No workstream waits for another to
finish. Every workstream reaches a usable state by 2026-09-25, and the last three days deepen
whichever ones pay off most. Record progress per workstream below so gaps are visible.

| # | Workstream | Progress | Next step |
|---|------------|----------|-----------|
| A | Correctness: review rounds, triage, fixes with a failing test first | R1 reviews running | Triage each `findings/r1-*.md` as it completes, dispatch fix agents per crate group |
| B | Old audit: mark each of the 227 `AUDIT-BACKLOG.md` findings fixed or open | R1 reviewers report still-open ones | Collect `[STILL-OPEN]` tags, queue them under A |
| C | Slop and simplification: desloppify plan, per-crate simplify pass | Triage done (`findings/desloppify.md`). Scores 2026-09-21: strict 80.2, objective 84.7. Weakest: file health 62.2, type safety 72, stale migration 74, contracts 75. 12 fix batches, about 198 hours | Start batches that do not collide with open fix branches. File splits of `resolution.rs`, `dispatch.rs`, `formatting.rs`, `symbols.rs`, `file_index.rs` wait for their fix branch to merge |
| D | Security: credentials, archive parsing, MCP and daemon input, extension binary download, supply chain | Two review rounds, all findings fixed and merged, project trust shipped | Round 3 review after the daemon lifecycle work merges, focused on trust bypasses |
| E | Tests: coverage by crate, property tests, `cargo mutants` | First pass merged: 4 bugs found by property tests, coverage table, CI job proposal | Add the property test CI job, run `cargo mutants` on the 10 file shortlist in `findings/test-depth.md`, make `al-test/backends/snapshot.rs` testable |
| F | Grammar: corpus tests, query drift between `languages/al` and `tree-sitter-al/queries` | R1 review running | From R1 findings |
| G | AI tooling: make this project speed up and sharpen AI work on Business Central (see below) | Inventory, measurements and design done (`findings/ai-tooling-ideas.md`): latency is 4 to 150 ms warm, but 14 of 20 measured answers are too large for an agent (up to 9.4 MB). Plugin build running | Daemon projection work after the LSP fix branch merges |
| H | Docs: `Docs/`, `README.md`, `ROADMAP.md` match the code, then unsloppify | R1 docs review running | From R1 findings |
| I | Blog: replace the six articles with a new series on the current project, unsloppify each | On blog branch `campaign/2026-09-rewrite`: six posts deleted, site cleaned, `pnpm validate` passes (it failed on `main`), fact sheet and nine-article plan in `findings/blog-plan.md`, articles 1, 2, 3, 5, 6, 7 drafted (6 of 9), article 1 `trace` paragraph corrected | Articles 2, 4, 5, 8 after the fix branches settle, article 9 last, final fact pass, merge to `main` |

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

- Merged `campaign/fix-daemon-lifecycle`: build identity handshake replaces a daemon built from other code, idle exit (30 minutes, `AL_DAEMON_IDLE_SECS`), exit when the project root is gone, PATH daemon refused on version mismatch, `daemon-shutdown` waits, plugin SessionEnd hook, harness stops its daemons. Open: a wedged request can hold a daemon past its idle window (warns every 60 s). Full gates: 92 suites, 4839 passed, 0 failed.
- Merged `campaign/fix-r2-security`: 8 of 8 fixed. Project trust (`Docs/features/project-trust.md`, `al-explorer trust`), symlink-safe containment, one credential authorisation function, https required for credentials to non-loopback servers (`AL_ALLOW_INSECURE_BC_HTTP=1` overrides), NuGet feeds https only, daemon socket directory ownership check, checksum docs corrected. Full gates including zed-al: 92 suites, 4884 passed, 0 failed.
- Merged `campaign/slop-syntax-symbols`: `formatting.rs`, `symbols.rs`, `index.rs` and `oauth.rs` split into module directories (largest file now 796 lines), the duplicate data loader in al-symbols removed, `LineIndex` and `SourceLines` merged, one HTTP retry policy, one temp path helper, typed `ObjectKind` error. Gates: 2825 tests across al-syntax, al-symbols, al-analysis, al-lsp and the harness. desloppify refuses to rescan until its 112 item review queue is empty.
- Merged `campaign/fix-r1c-analysis`: 28 of 29 fixed plus the three items left by the second pass (type-position completion, the `attribute_list` dead branch, `EdgeKind::TriggerInvocation` removed). A node-kind guard test now fails if code names a syntax node the grammar lacks. Open: nine whole-workspace queries still attribute findings in a multi-object file to its first object. Full gates: 91 suites, 4770 passed, 0 failed.
- Merged `campaign/fix-ci-platforms`: all seven CI jobs pass on PR #31 (closed after merge). Root causes: analyzer versions compared as strings per directory order (macOS), Record platform methods only offered when a toolchain was installed (ubuntu), verbatim path prefix in the containment message (Windows). CI now runs the whole suite before failing a job.
- Merged `campaign/ai-daemon-projection`: `subscribers` and `impact --table` answer correctly, `limit`, `offset`, `fields`, `scope` on list methods, `source --list-procedures`, `al-explorer location`, `--compact`, single-flight background call-graph build with progress, `--timeout-ms`. Haiku context bytes fell on six of seven plugin questions. Full gates: 90 suites, 4726 passed, 0 failed after stale daemons were killed.
- Merged `campaign/test-depth`: property tests found and fixed 4 bugs (formatter not idempotent with same-line braces, ropey counting U+2028 and four other characters as line breaks where LSP does not, `Code` keys iterating case-sensitively, ASCII-only folding in range filters). Coverage about 89 percent, table in `findings/test-depth.md`. Full gates after the merge: 90 suites, 4688 passed, 0 failed.
- Merged `campaign/fix-r1b-analysis`: 34 of 35 fixed. Quoted identifiers rename end to end, rename rewrites `[EventSubscriber]` arguments, fields are read from the syntax tree, obsolescence reads `ObsoleteState` properties, breaking changes are classified by what a dependent app must change. Merge conflict in `resolve_object_path` resolved by adding `FileIndex::object_path_where` (type match first, then nearest app). Open: bare field references inside a table's own procedure are not renamed (`definition()` does not resolve implicit `Rec`). The `EdgeKind::TriggerInvocation` removal landed on `campaign/fix-r1c-analysis` once `test_coverage.rs` was free.
- Merged `campaign/fix-lsp-content-modified`: read requests recompute across generation swaps, read-only daemon methods take `text` for files outside the project, the `lsp_dispatch` queries gained the path containment they lacked, error code -32002 for refused paths.
- Full gates on 2026-09-21 18:00 after nine merges: clippy clean, 80 suites, 4564 passed, 0 failed, 10 ignored (baseline was 4380).
- Merged `campaign/fix-r1b-runtime-dap`: 26 fixed, 1 rejected, 1 no action. Glob filter no longer drops tests from a green summary. JUnit output stays parseable and names timeouts. SignalR reader no longer blocks on a full channel. Nine harness tests now assert what their names claim. The harness refuses to run against stale binaries (`AL_HARNESS_ALLOW_STALE_BINARY=1` overrides), so build al-lsp and al-explorer before `cargo test -p al-test-harness`.
- Merged `campaign/fix-r1-symbols-project`: 24 of 24. Multi-app workspaces keep both objects and go-to-definition prefers the referring file's app. Unknown or ill-typed `al.*` editor settings warn and no longer stop startup. Source index cache bounded at 64 entries. ZIP-slip closed in nupkg extraction. All seven first-pass fix branches are in.
- Merged `campaign/ai-free-ids`: `freeIds` daemon method, `al_freeids` MCP tool, `al-explorer free-ids`. Object IDs per kind, field numbers and enum ordinals with extension collision checks. Responses are 200 to 280 bytes.
- Merged `campaign/fix-r1-emit-bc-explorer`: 24 fixed, 2 rejected after running Microsoft alc 17 (the packaged XLIFF and the generated `.g.xlf` follow different rules and the emitter already matched both), 3 new findings. `al-explorer publish` and a daemon `publish` method now exist. Archive entry names are checked. `rename` is all or nothing.
- Merged `campaign/fix-r1-analysis-insight`: 21 of 21 plus the 11 scaffold and generator findings.
- Merged `campaign/fix-r1-lsp-protocol`: 15 of 15 fixed. Cached tokens only reach hosts the project's launch configuration names. Every daemon path parameter goes through `daemon/containment.rs`. No generation read guard is held across a long await. Behavior change: a relative `file` parameter resolves against the project root.
- Merged `campaign/ai-plugin`: Claude Code plugin `al-bc` under `plugin/` with 8 skills, 2 subagents, a SessionStart hook, `.claude-plugin/marketplace.json`, and `make plugin-validate`. 7 of 7 Haiku test questions answered correctly on the fixture project (`plugin/TESTING.md`). Left: agent runs for `bc-test-locally`, `bc-upgrade-impact`, `bc-cop-fixer`, a test on a project with `.alpackages`, `plugin/evals/`, a setup hook that downloads release binaries. `plugin/ROADMAP.md` lists the workarounds to remove once the daemon projection work lands.
- Merged `campaign/fix-r1-runtime-dap`: 13 of 13 fixed, each cited to Microsoft Learn. Left open: a real `line-rate` for dynamic Cobertura needs a statement-line query in al-analysis (the document now says `line-coverage="unavailable"`), and `Assert.RecordIsEmpty`, `RecordIsNotEmpty`, `TableIsEmpty` still route to live BC.
- Merged `campaign/fix-r1-syntax-grammar`: 10 fixed, 1 rejected with proof (scanner.c wasm build is clean, CI now builds the grammar to wasm). New open items recorded in `findings/r1-syntax-grammar.md`: 124 more `trim_matches('"')` identifier cleanups in al-analysis (102) and al-insight (22), `clean_attr_arg` does not unescape doubled quotes, `sort_members` strands a blank line.
- Merged `campaign/fix-r1-extension-ci`: 12 of 12 findings fixed (cargo-deny green, al-lsp upgrades again with offline fallback, extracted binaries verified against `binary-checksums.txt`, theme fixes moved into the generator, toolchain action pinned, release-dryrun runs 16 stages).
- Campaign branch, protocol docs, watchdog timer, heartbeat hook, desloppify install.

## Baseline (2026-09-21)

- `cargo clippy --workspace --all-targets`: clean.
- `cargo test --workspace`: 80 suites, 4380 passed, 0 failed, 10 ignored.
