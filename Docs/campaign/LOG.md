# Campaign log

Append-only. Newest entry last.

## 2026-09-21 01:30 BST: setup

- Created branch `campaign/2026-09-21` from `dev` at `cdb5fb8c`.
- Wrote the resume protocol (`README.md`) and the queue (`STATE.md`).
- Installed `scripts/campaign/watchdog.sh` as systemd user timer `al-campaign-watchdog.timer`
  (every 10 minutes, starts a headless session when the heartbeat is 30 minutes old).
- Added a `PostToolUse` hook in `.claude/settings.local.json` that touches `.campaign/heartbeat`.
- Installed desloppify into `~/.local/share/desloppify-venv`, linked at `~/.local/bin/desloppify`.
- Started baseline clippy and tests, the first desloppify scan, and seven review agents.

## 2026-09-21 01:50 BST: baseline and blog inventory

- Baseline: clippy clean, 4380 tests pass in 80 suites, 10 ignored.
- Blog inventory written to `findings/blog-inventory.md`. The blog deploys from `main` through Vercel, so blog work happens on a branch and merges once the new articles are ready. Manual cleanup beyond the six posts: `src/data/projects.ts`, `TerminalDemo.tsx`, `tui-demos/al-explorer/`, `public/demos/al-explorer/`, the article-writer skill baseline.

## 2026-09-21 02:30 BST: round 1 reviews and first desloppify scan

- Six of seven R1 reviews done: 93 verified findings. Six fix agents running in worktrees, one branch per subsystem.
- desloppify first scan: objective 83.4, strict 20.9 (subjective review not done yet), 1243 open issues: code quality 578, duplication 379, file health 154, test health 53, security 4. A triage agent is doing the subjective review and batching. Structural desloppify fixes wait until the R1 fix branches merge, to avoid conflicts.

## 2026-09-21 06:00 BST: first usage limit

- Session limit hit about 03:50, reset 06:00. All 11 agents died mid-task. Fix branches kept their commits (1 to 5 each) plus uncommitted edits in the worktrees under `.claude/worktrees/`. Review files kept their findings.
- Watchdog fired four times during the outage and exited in 3 seconds each time, as intended. Its limit detection pattern missed the message "hit your session limit", fixed.
- `AUDIT-BACKLOG.md`, `BENCHMARKS.md` and `ROADMAP.md` were deleted in the main checkout by an unknown agent. Restored from git.
- Resumed every agent from its transcript.

## 2026-09-21 07:20 BST: first fix branch merged

- Merged `campaign/fix-r1-extension-ci` (12 findings). Gates: fmt, clippy and 72 tests on zed-al, `cargo deny` all ok. The grammar submodule now tracks branch `campaign/2026-09-21` of AL-Tree-Sitter at `d8c4cc7`, shared by the extension and syntax fix agents.
- Review totals for round 1: 224 findings across 11 findings files.
- The theme was regenerated from AL extension 18.0 because the pinned 17.0 container overlay no longer exists. Output matched the committed file apart from the three hand edits.
## 2026-09-21 11:00 BST: second usage limit

- Limit hit about 08:05, reset 11:00. Ten agents died. All fix branches were pushed, uncommitted edits remain in the worktrees. Watchdog detected the limit correctly this time and tried Opus, also limited (the session limit is shared across models).
- Sub-agents spawned by agents died with their parents and their output was lost. Agents are now told to do their own reading.
- Resumed every agent from its transcript.

## 2026-09-21 11:25 BST: desloppify triage

- Subjective review imported: strict score 20.9 to 80.2, objective 84.7. All 4 security hits are test literals, suppressed by issue ID. Excluded `grammars/` (ignored copy of the submodule, 79% of duplication hits) and rezoned al-test-harness as test code.
- 12 fix batches in `findings/desloppify.md`. Highest value single finding: `al-symbols/src/language_data.rs:3` justifies a duplicate data loader with a dependency rule that `al-symbols/Cargo.toml:10` does not follow.

## 2026-09-21 11:50 BST: syntax and grammar fixes merged

- Merged `campaign/fix-r1-syntax-grammar`. Grammar submodule at `38368a0` on AL-Tree-Sitter branch `campaign/2026-09-21`, `extension.toml` rev matches. Gates: fmt, clippy, 413 tests on al-syntax and zed-al. Grammar corpus 46389 of 46389.

## 2026-09-21 12:20 BST: runtime and DAP fixes merged

- Merged `campaign/fix-r1-runtime-dap`. Gates: fmt, clippy, 982 tests on al-runtime, al-test, al-dap. The agent saw 2 al-test-harness cancellation tests fail inside its worktree. They pass in the main checkout, so the cause is the nested worktree environment. `cargo fmt --all` also fails inside nested worktrees for the same reason, agents use `-p` filters there.
- Corrections the agent made to the findings: `Round` with `<` and `>` moves the magnitude (toward and away from zero), and field capacities such as `Code[20]` were never parsed at all before this fix.

## 2026-09-21 12:50 BST: plugin merged

- Merged `campaign/ai-plugin`. `plugin/.mcp.json` was missing from the branch because the root `.gitignore` ignored every `.mcp.json`. Recreated it, anchored the rule to `/.mcp.json`, and confirmed a Haiku session lists the `mcp__plugin_al-bc_al__*` tools.
- New gaps the plugin agent found: no object to file mapping on the CLI (`location` is daemon only), workspace objects return a stub without fields and methods, skill descriptions alone did not trigger on Haiku among 60 other skills.

## 2026-09-21 13:30 BST: LSP and protocol fixes merged

- Merged `campaign/fix-r1-lsp-protocol`, 15 findings including both security ones. Gates: fmt, clippy, 769 tests on al-lsp and al-protocol. Four of seven round 1 fix branches are in.

## 2026-09-21 15:00 BST: analysis, emit and ID allocator merged

- Merged `campaign/fix-r1-analysis-insight`, `campaign/fix-r1-emit-bc-explorer`, `campaign/ai-free-ids`. Merge conflicts: two line-table types added to `al-syntax/src/lib.rs` by two agents (kept both, queued a unification), and the daemon dispatch match in `daemon/mod.rs` twice.
- The combined merge failed two catalog tests that no single branch could see: the daemon reference did not list `publish`, and the CLI smoke catalog had no `publish` case and used `snapshot list` without the new required `--company`. Fixed on the campaign branch.
- Full workspace clippy and tests started.

## 2026-09-21 16:30 BST: third usage limit, symbols merged, one regression

- Limit hit about 13:20, reset 16:00. Five agents resumed.
- Merged `campaign/fix-r1-symbols-project`. All seven first-pass fix branches are in. Workspace clippy clean. 2471 tests pass across the nine crates the merge touches.
- The full workspace test run found a regression: `al-test-harness --test edit_lifecycle` fails 3 of 3 runs. documentSymbol and workspace/symbol return LSP error -32801 after a file closes. Source is `content_modified_error()` in `al-lsp/src/server/lsp.rs`, introduced with the LSP fix branch. `cargo test` stopped at that suite, so later suites are unverified. A fix agent is on it and will run the whole harness suite.

## 2026-09-21 17:10 BST: regression root cause and a containment decision

- Root cause of the `edit_lifecycle` failures: `offload_after_ready` reported ContentModified whenever the global `generation_revision` moved during a read, and `did_close` bumps it twice. Fixed on `campaign/fix-lsp-content-modified` by recomputing against the new generation, at most 3 attempts.
- The whole harness suite then showed one more merge effect: `al-explorer parse <file outside the project>` is refused by the new daemon path containment. Decision: the daemon keeps containment. For read-only commands the CLI reads an outside file itself and sends the text. Commands that write stay refused outside the project.
- Lesson for gates: fix agents test their own crates, so merges need `cargo test -p al-test-harness` as well. Added to the merge gate.

## 2026-09-21 18:00 BST: campaign branch green

- Merged `campaign/fix-lsp-content-modified`. Full gates with `--no-fail-fast`: clippy clean, 80 suites, 4564 tests pass, 0 fail, 10 ignored. Baseline this morning was 4380, so the day added 184 tests net.
- Day 1 totals: 224 review findings, 9 fix branches merged (about 150 findings fixed, 4 rejected with evidence), the `al-bc` plugin, the free ID allocator, 4 of 9 blog articles drafted.

## 2026-09-21 21:05 BST: fourth usage limit, CI red on three platforms

- Limit hit about 18:25, reset 21:00. Six agents resumed.
- PR #30 CI: cargo-deny, WASM and the .NET bridge pass. Ubuntu, macOS and Windows each fail one test that passes locally. macOS shows a real bug (1.9.0 chosen over 1.10.0, selection depends on directory enumeration order). Windows fails the new out-of-project refusal test, probably path form. Ubuntu fails a harness test strengthened today, so something it needs exists on this machine and not on a clean runner. A fix agent has its own draft PR to iterate on real runners.

## 2026-09-21 21:50 BST: analysis second pass merged

- Merged `campaign/fix-r1b-analysis`. Two agents had changed `resolve_object_path` for different reasons (prefer the referring app, prefer the matching AL type). Combined through a new `FileIndex::object_path_where`. Gates: fmt, clippy, 2290 tests on al-source, al-analysis, al-insight, al-lsp and the whole harness suite.

## 2026-09-21 22:00 BST: round 2 security review

- 8 findings in `findings/r2-security.md`. Critical: a cloned repository can ship `.vscode/settings.json` with `al.codeAnalyzers` pointing at its own DLL, which a build loads into the compiler process. High: `launch.json` is both the allowlist for cached tokens and a place a repository can name a server. High: a dangling symlink inside the project defeats path containment for output files. Held up under attack: archive entry checks, `safe_join`, decompression caps, the `text` parameter, redirect handling, the token cache, the plugin scripts.
- Decision: project trust. Privileged repository settings apply only after the user runs `al-explorer trust`. Trust is stored outside the repository and keyed to a hash of the privileged values. MCP and daemon callers cannot grant it.

## 2026-09-21 22:40 BST: test depth merged, daemon projection merging

- Merged `campaign/test-depth`. Full gates: clippy clean, 90 suites, 4688 passed, 0 failed, 10 ignored.
- Merged `campaign/ai-daemon-projection` locally, full gates running. Haiku context bytes on the seven plugin questions fell on six of seven (for example 9634 to 2889 for a field impact question).

## 2026-09-21 23:10 BST: daemon projection merged, stale daemon found

- Merged `campaign/ai-daemon-projection`. One smoke test failed in the gate because a daemon started before the merge was still serving the fixture project with old code. Killed stale daemons, the suite passes. Queued a real fix: build identity in the client handshake.
- `campaign/fix-r1c-analysis` conflicts with the moved campaign branch in three files. Its agent is merging the campaign branch into its own and resolving there.
- Note: timestamps in this log before this entry were estimates and run up to an hour ahead of the clock.

## 2026-09-22 02:30 BST: fifth usage limit, CI green on all platforms

- Limit hit about 23:10, reset 02:00. Five agents resumed.
- Merged `campaign/fix-ci-platforms`. Its PR #31 passed all seven jobs. The merge broke one test in al-project that the test-depth branch had written against the old integer version key. Adapted.

## 2026-09-22 03:00 BST: all review fix branches merged

- Merged `campaign/fix-r1c-analysis`. Every round 1 review finding now has a fix merged, a rejection with evidence, or an entry in the open list in STATE.md. Full gates: 91 suites, 4770 passed, 0 failed.
