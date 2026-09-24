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

## 2026-09-22 03:40 BST: file splits merged

- Merged `campaign/slop-syntax-symbols`. The agent re-applied campaign changes to the split files by hand and proved nothing was lost with a normalised line diff. Started the desloppify review queue agent, since the tool blocks a rescan until the 112 subjective items are resolved or skipped.

## 2026-09-22 04:10 BST: security round 2 merged

- Merged `campaign/fix-r2-security`. Full gates with zed-al: 92 suites, 4884 passed, 0 failed.

## 2026-09-22 05:00 BST: daemon lifecycle merged

- Merged `campaign/fix-daemon-lifecycle`. Full gates: 92 suites, 4839 passed, 0 failed. The gate run left no daemon processes behind, where earlier runs left up to nine.

## 2026-09-22 05:40 BST: round 2 reviews done

- Review A: 43 fixes verified end to end, none faked. 14 new findings. Review B: 19 findings, 4 high security: `rename` skips containment, `tests.snapshot_capture` skips credential authorisation, a repository can ship `.alpackages` as a symlink and move the containment boundary, the extension compares binary paths without normalising. The dispatch table survived three hand merges intact (93 arms). Fix agents dispatched for both.

## 2026-09-22 07:10 BST: sixth usage limit

- Limit hit about 05:50, reset 07:00. Three fix agents resumed. Started blog articles 4 and 8, and the round 3 security review focused on trust bypasses.

## 2026-09-22 07:40 BST: round 3 security review

- 10 findings, 2 critical. The trust gate itself held: every privileged key consumer passes through it, no daemon method writes the store, trust is keyed to the canonical root. Around it: a repository can put its own text in the MCP `instructions` advisory, `al-explorer trust` runs with no terminal, `binary.arguments` in `.zed/settings.json` runs on open and is not in the digest, a planted socket captured a BC password in cleartext, a forged handshake was accepted, `snapshot` and `profiling` take credentials inline. Fix agent dispatched.

## 2026-09-22 08:30 BST: queued fixes merged, a formatter seed

- Merged `campaign/fix-queued-2` (28 commits). The property test for formatter idempotence found a new failing seed in the gate run. Seed persisted and committed, fix agent dispatched. Pushed with that one test red, since the seed file is the reproduction and the branch is a draft PR.

## 2026-09-22 09:00 BST: review queue merged

- Merged `campaign/slop-review-queue`. The new architecture diagram guard failed on the merge because the daemon lifecycle branch had added a harness dependency on al-protocol. Drew the edge. Gates: 93 suites, 4873 passed, the formatter seed still red.

## 2026-09-22 10:00 BST: review B merged, a ghost diagnostic

- Merged `campaign/fix-r2-review-b`. Two conflicts with the review queue branch (which had made two queries return `Option`) resolved keeping both. Gate run: a harness test failed 1 in 3. Its own bug (iterating diagnostics as arrays) turned a real ghost publish after didClose into a panic. Fix agent dispatched with that analysis.

## 2026-09-22 12:10 BST: seventh usage limit

- Limit hit about 10:30, reset 12:00. Three agents resumed. The round 3 security merge failed clippy: the review queue branch had bumped `getrandom` to 0.4 (`fill`) and the security branch used 0.2 (`getrandom`). One-line fix, gates rerunning.

## 2026-09-24 06:30 BST: stray branches merged, four queued items fixed

- Cloud session. Merged the three branches with unmerged commits: formatter idempotence, the ghost-diagnostics test, and three file splits. Fixed the ghost-diagnostics window in the server, the Windows `&` URL bug (both copies), `pack-native --validate` analyzers, two review A items, the README CLI list, and the harness stale-binary check that failed 270 tests after an al-explorer edit. Clippy caught split damage in `native_dap/mod.rs` doc comments. Gates: 95 suites, 5011 passed, 1 failed (root-only).
- Branches fully merged into `campaign/2026-09-21` and safe to delete (the cloud session gets 403 on delete): `ai-daemon-projection`, `ai-free-ids`, `ai-plugin`, `fix-ci-platforms`, `fix-daemon-lifecycle`, `fix-formatter-idempotence`, `fix-ghost-diagnostics`, `fix-lsp-content-modified`, `fix-queued-2`, `fix-r1-analysis-insight`, `fix-r1-emit-bc-explorer`, `fix-r1-extension-ci`, `fix-r1-lsp-protocol`, `fix-r1-runtime-dap`, `fix-r1-symbols-project`, `fix-r1-syntax-grammar`, `fix-r1b-analysis`, `fix-r1b-runtime-dap`, `fix-r1c-analysis`, `fix-r2-review-b`, `fix-r2-security`, `fix-r3-security`, `slop-review-queue`, `slop-splits-2`, `slop-syntax-symbols`, `test-depth` (all under `campaign/`).

## 2026-09-24 08:00 BST: dogfooding against Base Application symbols

- Scaffolded a project with `al-explorer new` and downloaded Base Application 26 symbols from the public NuGet feed, then ran every common agent query. Twelve defects found and fixed, each with a test: see the STATE.md Done entry. The largest were a deadlock in `downloadSymbols` (a `let _ = guard;` that dropped nothing), a daemon that answered from its startup snapshot for up to 30 minutes, and native compile rejecting table extension fields. Also fixed the three CI failures on ubuntu, Windows and macOS that predate this session. Query latency on the bench project: 15 to 90 ms per CLI call, `diag` cold 1.1 s for 10,778 symbols.

## 2026-09-24 09:20 BST: R4 review fixed, queue drained

- The R4 session review (`findings/r4-session-review.md`) found 1 medium and 9 low. Nine are fixed with tests, one queued (daemon requests need snapshots, not a request-wide lock). The medium: `pack-native --validate --analyzers X` loaded a DLL an untrusted repository shipped. Fixing the syntax-only daemon refresh exposed a worse bug: the daemon refreshed its file index but not its document store, so `symbols`, `hover` and `lint` on an edited file answered from startup text.
- Queue items done: `obsolete --used` judges calls against the receiver's overloads (the Rijndael Text/SecretText case is now reported on the real System Application); the emitter encodes lowercase permission letters as indirect grants (confirmed from 16,816 Base Application grants, not an alc build); `package-diff` filters uses by object kind and by declared receivers (a probe on Base Application 25 to 26 went from 1 use plus 9 false possibles to the 1 real use); `generate` takes the next free ID in `idRanges`; one response-contract validator in the CLI (466 duplicate lines gone); file splits for `xliff.rs`, `calls.rs`, `router.rs` and the BC debug `session.rs`. MCP search results for agents are compact summaries (212 KB to 492 bytes for three Base Application hits).
- Also found while fixing: `lint --analyzers` was accepted and ignored (now refused with where the cops run), and `definition` jumped to any same-named workspace procedure when a resolved record had no such member.
- Gates: 94 suites, 4981 passed, 1 failed (root-only). A dogfood agent is sweeping every CLI command against Base Application into `findings/r5-dogfood.md`.
