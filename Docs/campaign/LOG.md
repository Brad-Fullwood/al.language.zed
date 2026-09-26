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

## 2026-09-24 11:30 BST: r5 dogfood findings closed

- Every finding in `findings/r5-dogfood.md` now has a status: 31 fixed, 1 partly fixed (PERF-PACKAGE-DIFF: the timeout message and a warning when the `breaking` baseline is a different app; no `.app` cache, which would pin two Base Applications in memory).
- The two that changed the most behaviour: `test-affected` now reaches tests through the records a changed table or table extension defines, and a plain `Insert()`/`Modify()`/`Delete()` raises the table's OnBefore/OnAfter events in the call graph (only `RunTrigger = true` did). `suggest-event` no longer prints "still being analyzed" on every run: it says whether the trace was cut at depth 10 or reached package code without source, and a cut trace exits 0, not 75.
- Also: `graph --scope workspace` exports the workspace slice (10 nodes on the bench, where the whole graph was 117k and over the cap), `impact` reports call/write/read and stops repeating rows, completions/hints/symbols/folding page and print readable text with LSP-shaped JSON, signature help works inside a string argument, lint findings point at the call.
- A guard test caught a node kind (`field_declaration`) the grammar does not have in the impact change after it was pushed; fixed in the next commit.

## 2026-09-24 12:30 BST: r6 session review closed, splits, rustdoc gate

- A review agent read this session's commits (`findings/r6-session-review.md`): 14 findings, 5 medium, all fixed. The mediums: test-affected missed temporary records, record arrays and triggers; `impact` read `Validate("Field", X)` as a low-confidence read (now a bound write, SetRange/SetFilter a filter); `--fields` refused optional keys rows leave out when empty (now `absentFields`); package obsolete procedures were given caller counts by bare name (now omitted); `xlf refresh` never carried a developer Comment into existing units.
- The file-index re-index race from R4 is closed without a lock: each name's owners are swapped under one entry lock and stale names pruned after. A concurrency test saw the object missing on the old code.
- Bulk fix planning has a typed error, so scan limits and bad values come back as INVALID_PARAMS instead of CODE_ANALYSIS_ERROR.
- Test modules moved out of six large files (`tests_dispatch.rs` 4182, `lsp.rs` 4043, `daemon/mod.rs` 3645, `workspace.rs`, `mcp.rs`, `client.rs`); code unchanged, test counts unchanged.
- All 46 rustdoc warnings fixed; CI now runs `cargo doc` with `-D warnings`.
- Gates: full suite 93 suites, 5026 passed, 1 failed (root-only) before the r6 fixes; CI green on every push since.

## 2026-09-25: pull request 30 merged

- Brad merged PR 30 into `dev` at 94700cf7 (merge commit afec75d1). CI runs on pushes to `main` and `dev` and on pull requests, so later pushes to the campaign branch ran no CI. A new draft PR from the campaign branch to `dev` takes over.

## 2026-09-26 07:00 BST: interactive session, five agents

- Merged the async locking batch (d34ad3d2): two deadlocks in al-lsp fixed (the diagnostics pass took the config lock before the project lock, the bridge read lock was taken twice), the DAP proxy stdout lock is held for one frame instead of a compile, 15 dead `#[allow]` attributes removed, a SAFETY comment on every unsafe block. Gates on the merge: 94 suites, 5065 passed, 1 failed (`no_ghost_diagnostics_after_close_during_debounce`, 2 in 8 at load average 20).
- Round 4 security review over the final diff: 14 findings (4 high, 4 medium, 6 low) in `findings/r4-security.md`. The highs: the Zed debug adapter and `tests.run` sent the cached credential to any server a repository file named, the XLIFF methods took absolute paths with no containment, and a user's own analyzer name resolved to a DLL the repository shipped.
- Blog: 18 commits on `campaign/2026-09-rewrite`. Articles 2, 4, 5 and 8 revised against binaries built at ba2cda14, article 9 written, fact pass and unsloppify on all nine, `pnpm validate` passes. Six new defects recorded in `findings/blog-progress.md`.
- Agents for the security fixes, the persisted symbol index, `cargo mutants`, the docs review, the blog defects and the ghost race started. The session ended at a usage limit with all of them in flight.

## 2026-09-26 11:50 BST: headless session, security round 4 merged

- Six agent branches had unmerged commits and no live agent. Merged `campaign/fix-r4-security`, 14 of 14 fixed: both DAP entry points and the test runners pass the trust gate before a credential leaves the machine, a scheme-less server means `https`, the XLIFF methods go through containment, analyzer discovery decides trust itself, the trust digest hashes a repository-resident analyzer or `dotnet` instead of naming its path, the handshake proof is checked before the build identity, a linked `.alpackages` counts as an untrusted package cache, the semantic bridge is in the release digests, the scaffold and the native build refuse symlinks, `trust --yes` is pinned to a reviewed digest, and the language server re-gates its settings when the trust inputs move.
- Merged `campaign/fix-blog-findings`, 3 of 4: `packages` counts leave out synthetic Option enums, the client keeps waiting while the call graph builds, `object` and `byId` answer without the graph.
- Merge damage: the lock batch made `gate_repository_settings` synchronous and the security branch still awaited it at two sites. Fixed in a8bb710e. Merged `origin/dev` (the PR 30 merge commit) so the branch contains `dev`.
- Re-dispatched five agents: docs review (35 commits plus an uncommitted prose pass), `cargo mutants` (3 of 10 files done, three proptest seeds written under mutants to check against clean code), persisted index (step 4, the measurement), ghost race debug (nothing committed), the `${CodeCop}` token refusal (blog finding 4).
- Gates on the merge (a6f6653c): fmt, release build and clippy clean. 92 suites, 5118 passed, 2 failed, 10 ignored, at load average 16 with five agents building. Both failures are a diagnostics publish after didClose: `no_ghost_diagnostics_after_close_during_debounce` (regression) and `test_completeness_d03_close_file_clears_diagnostics` (completeness, a native `AL-NC001` diagnostic republished for the closed file). Passed to the ghost race agent. Pushed with those two red, since the branch is a draft PR.

## 2026-09-26 12:20 BST: docs review merged

- Merged `campaign/docs-review` (workstream H, `findings/docs-review.md`, `## Review complete`): every file under `Docs/`, `README.md`, `ROADMAP.md` and `plugin/` checked against the code, the CLI help, the dispatch table, the MCP tool list, the settings reader and the workflows, then a plain-wording pass. Four drift items fixed after this morning's merges: `object` and `byId` fill workspace members only with `waitForMembers`, the request deadline also waits for the call graph (up to 600 s), `trust --yes` needs `--digest`, `al.assemblyProbingPaths` also serves the in-process bridge. Docs only, no code. The architecture diagram test passes on the merge.
- Grammar: two README commits on the grammar's `campaign/2026-09-21` branch (f211aef), pushed before the submodule pointer and the `extension.toml` rev moved to it.
- 12:30: CI on PR 32 failed the semantic bridge job. Clippy with the `semantic` feature flagged `require_document_text` (`result_large_err`), whose allow the lock batch had removed because the workspace clippy run does not see the async function the same way. Allow restored with a reason (76072840), the semantic clippy line added to the README gates, and it passes locally.

## 2026-09-26 12:50 BST: analyzer token fix merged, two CI-only gates

- CI's ubuntu job failed on rustdoc: two intra-doc links to private items in `trust.rs` from the security round. Fixed with plain code spans (f13fec50) and the rustdoc line joined the README gates.
- Merged `campaign/fix-blog-findings` again for blog finding 4 (8431a2a3): `analyzer_name` unwraps the `${Name}` token before stripping `.dll`, so `is_builtin_analyzer` and `resolve_analyzer_paths` accept the spelling `al.codeAnalyzers` uses and the `al-explorer new` template writes. `trust::is_builtin_analyzer_token` delegates to it, one predicate instead of two. The `--analyzers` wording in the refusal had already gone with `project_local_analyzers` in 2d889e93; a test pins that the message names no caller. Reproduced by hand: a fresh untrusted scaffold with `${PerTenantExtensionCop}` now passes `pack-native --validate` where it was refused. Four tests, two of which fail before the fix.
- The fix agent found that `cargo fmt --all` and `cargo clippy --workspace` fail inside `.claude/worktrees/` because the `tree-sitter-al` submodule's manifest walks up to the outer checkout's workspace. Per-crate `-p` invocations work. Pre-existing, worktree-only.
- Gates on the merge (8431a2a3): fmt, release build, both clippy runs and rustdoc clean. 92 suites, 5123 passed, 1 failed, 10 ignored. The one failure is `test_completeness_d03_close_file_clears_diagnostics`, the ghost publish after didClose that the debug agent has. Pushed.

## 2026-09-26 13:05 BST: round 7 review, fix agent dispatched

- The round 7 adversarial review (`findings/r7-session-review.md`, 44b52c87) read everything merged since round 6: 15 findings, 1 high, 5 medium, 9 low. Of the 14 round 4 security fixes, 8 hold and 6 have a gap. The high: a built-in analyzer name such as `CodeCop` reaches the semantic bridge unresolved, and the bridge looks in the working directory first, so a cloned repository that ships a file of that name gets it loaded with default settings. Mediums: one launch entry the strict parser rejects empties the trust record's server list, `object` and `byId` miss every object that is not first in its file, `--validate` judges trust on its temporary copy, `${CodeCop}` on the semantic path. No merge damage found in the two hand merges. Fix agent on worktree branch `campaign/fix-r7-review`.

## 2026-09-26 13:10 BST: blog re-read after the security round

- Eight of nine articles updated on the blog branch `campaign/2026-09-rewrite` (a351c07 to ad27608, pushed): articles 7, 8 and 9 tell security round 4 as closed (ten methods through the credential check, the debug adapter checks the server before sending the token, the bridge files in the release digests, `trust --yes` with `--digest`), findings 1 to 4 told as fixed with new measurements on the medium project (`by-id table 18` first call 619 to 694 ms, down from 52.6 s; cold `trace` 18 to 29 s at load 1 to 4), article 9 adds the round 4 row, the docs review, the PR 30 merge and PR 32. `pnpm validate` passes. Record in `findings/blog-progress.md` (a96657f3).
- Two new defects from the re-read, queued: the symbol reader has no field for profile extensions (`model.rs:630-652`), so Base Application indexes 7,968 of its 7,969 declared objects; `pack-native --validate` on a new project prints the "not trusted" notice twice.
- Open before publishing: timings on a quiet machine, the local share of a real BC test suite, four articles over the plan's word range, `readTime` not recomputed, article 9 needs a final re-read when the campaign ends. All nine keep `draft: true`; merging to `main` is Brad's call.

## 2026-09-26 13:40 BST: persisted dependency source index merged

- Merged `campaign/ai-persisted-index` (workstream G, `findings/persisted-index.md` section 3). Dependency source is kept as per-procedure summaries instead of syntax trees, the call graph is built from the summaries, and the summaries are persisted per package under the user's data directory (0700, owner-checked, keyed by schema version, grammar fingerprint and the `.app` bytes). On the medium benchmark project, alternating before and after runs at load 0.6 to 7.7: first start `impact "Sales-Post"` 17.2 s to 4.6 s, second start 23.6 s to 1.25 s, peak memory 2,857 MB to 526 MB on the first start and 2,834 MB to 371 MB after. Graph sizes identical, `impact` (760 rows) and `entrypoints` (34,420 rows) equal as sets. 59 MB of summaries per project. Tests: key, invalidation, equality of summaries and graphs built and loaded, corrupt and foreign and open-permission entries fall back to a rebuild, transaction lint equality. A documented example `crates/al-workspace/examples/dep_profile.rs` times each phase.
- Left, queued: the cache key does not cover the summary builder's code, so a change in al-insight without a `SCHEMA_VERSION` bump answers from stale entries (a checked-in summary snapshot of a fixture would catch it); entries are per project, so two projects on one Base Application store it twice; the package header scan could use the same cache; `entrypoints` and `impact` rows come back in a different order per start.
- Gates on the merge (0d11fc88): fmt, release build, both clippy runs clean. Rustdoc failed on one intra-doc link to a private item in `al-insight/src/calls/nodes.rs`, fixed. 92 suites, 5135 passed, 1 failed (the ghost publish, `test_completeness_d03_close_file_clears_diagnostics`), 10 ignored. Pushed.

## 2026-09-26 16:48 BST: headless session after an hour of usage limits

- Every headless start from 15:40 to 16:40 exited at once with a session limit on both models
  (`.campaign/watchdog.log`). This session started at the 16:47 tick.
- Five agent worktrees, three with unmerged commits and none with a live agent: `campaign/fix-r7-review`
  (9 commits, 6 findings fixed, 9 open), `campaign/fix-ghost-race-2` (the mechanism written up, the
  test uncommitted), `campaign/test-mutants` (8 commits, 4 of 10 files, `filter.rs` tests uncommitted).
  `campaign/ai-persisted-index-2` and `campaign/slop-batch-11` had nothing committed: the persisted
  index agent had not started, the desloppify agent had run its scan and written nothing down.
- All five re-dispatched onto their branches (see `STATE.md`).

## 2026-09-26 18:45 BST: local interpreter runs ordinary AL tests (second session)

- A second session worked on the local test interpreter and router beside the orchestrator, pushing to this branch (4859b011 to 826b06a9). Driven by two bench test codeunits, a language tour (8 tests) and a second one with a table that has triggers, labels, TextBuilder and Guids (4 tests): at the start of the day every test in both needed live BC; now all 12 run locally except one JSON test, and a changed assertion fails where it should.
- Interpreter: arrays and `Txt[i]`; chained calls run every step (`S.Trim().ToUpper()` had returned `' A,B '`, only the last call ran); enums (variables, `AsInteger`, `Names`, `FromInteger`); TextBuilder; `CalcDate`, `Date2DWY`, `Evaluate`, `DelStr`, `Maximum`, `ArrayLen`, `CreateGuid`, `IsNullGuid`; `Rename`, `TestField`, `ModifyAll`, `Ascending`, `IsTemporary`; `Format` picture strings, and numbers group thousands as BC's standard format does (`1,234,567`).
- Table code runs on its record: `Validate` with OnValidate and a TableRelation check, `Insert/Modify/Delete(true)` triggers, `Rename`'s OnRename, table procedures, bare field names and bare record methods in table code.
- Event subscribers run: integration, business and internal events, and table events (OnBefore/OnAfter Insert, Modify, Delete, Rename, Validate). Before this the router followed a publisher to its subscribers and kept the test local while the interpreter never ran them, so such a test failed locally and passed on BC.
- Router: a table with triggers is no longer refused; its code is classified as reachable. `Validate` on a conditional TableRelation or one to a table outside the workspace routes live. TextBuilder was typed as Text.
- Multi-object files: the stopped audit agent's uncommitted work (17 files: definition, hover, object/byId, debugger breakpoints, transaction lint, symbol invalidation) merged as 87ff59eb; the interpreter's own dispatch and the local test runner took a file's first object as the callee (a codeunit after a table failed as "stateful codeunit 'Tour Member'"), fixed in 901586e2.
- Left: JSON types; List and Dictionary are values in the interpreter where AL has reference semantics; Manual subscribers and `BindSubscription`.
