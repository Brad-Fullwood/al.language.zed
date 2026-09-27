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

## 2026-09-26 18:36 BST: local interpreter runs ordinary AL tests (second session)

- A second session worked on the local test interpreter and router beside the orchestrator, pushing to this branch (4859b011 to 826b06a9). Driven by two bench test codeunits, a language tour (8 tests) and a second one with a table that has triggers, labels, TextBuilder and Guids (4 tests): at the start of the day every test in both needed live BC; now all 12 run locally, and a changed assertion fails where it should.
- Interpreter: JSON types (references, as in AL); arrays and `Txt[i]`; chained calls run every step (`S.Trim().ToUpper()` had returned `' A,B '`, only the last call ran); enums (variables, `AsInteger`, `Names`, `FromInteger`); TextBuilder; `CalcDate`, `Date2DWY`, `Evaluate`, `DelStr`, `Maximum`, `ArrayLen`, `CreateGuid`, `IsNullGuid`; `Rename`, `TestField`, `ModifyAll`, `Ascending`, `IsTemporary`; `Format` picture strings, and numbers group thousands as BC's standard format does (`1,234,567`).
- Table code runs on its record: `Validate` with OnValidate and a TableRelation check, `Insert/Modify/Delete(true)` triggers, `Rename`'s OnRename, table procedures, bare field names and bare record methods in table code.
- Event subscribers run: integration, business and internal events, and table events (OnBefore/OnAfter Insert, Modify, Delete, Rename, Validate). Before this the router followed a publisher to its subscribers and kept the test local while the interpreter never ran them, so such a test failed locally and passed on BC.
- Router: a table with triggers is no longer refused; its code is classified as reachable. `Validate` on a conditional TableRelation or one to a table outside the workspace routes live. TextBuilder was typed as Text.
- Multi-object files: the stopped audit agent's uncommitted work (17 files: definition, hover, object/byId, debugger breakpoints, transaction lint, symbol invalidation) merged as 87ff59eb; the interpreter's own dispatch and the local test runner took a file's first object as the callee (a codeunit after a table failed as "stateful codeunit 'Tour Member'"), fixed in 901586e2.
- Left: Manual subscribers and `BindSubscription`; whether List and Dictionary should share on assignment (the interpreter copies them) is unverified.

## 2026-09-26 18:32 BST: headless session, three merges, five agents

- Origin was five commits ahead of the local checkout: a cloud session had pushed runtime and test
  router work straight to the campaign branch between 16:57 and 17:30 UTC (4859b011..901586e2,
  39 files, 3,502 insertions): table code runs on its record, event subscribers run, TextBuilder,
  Guids, Rename and TestField run locally, labels bind, a codeunit declared after a table in one
  file runs as itself. CI on PR 32 passed on each push. No reviewer has read them, so the round 8
  review starts with them.
- The 17:35 headless session left an empty log and no commits.
- Merged three complete agent branches: `campaign/fix-ghost-race-2` (3c2f2e12: the project pass
  skips a report whose input changed after staging, `findings/ghost-race-2.md`, both harness tests
  0 of 16 failures at load 22 to 26), `campaign/fix-profile-extension` (900e2174:
  `ProfileExtensions` read from `SymbolReference.json`, so Base Application indexes 7,969 of
  7,969 objects) and `campaign/ai-persisted-index-2` (a10e5eea: the summary key covers the builder
  through a fixture hash and a checked-in snapshot, `entrypoints` and `impact` rows come back in
  one order, entries are shared across projects under one store with a 1 GiB limit per user).
- The mutants worktree held a mutation that `cargo mutants --in-place` left when its agent died
  (`mock/record.rs`, `next` returning `Ok(1)`). Restored before the re-dispatch.
- `target/debug` (30 GB) deleted, 31 GB free after.
- Five agents dispatched at 18:40, listed in `STATE.md`.
- Later the same evening: codeunit variables are instances with their own globals (stateful helpers run locally; only SingleInstance helpers route live), review fixes (XML format, `sender`, JSON sharing, Validate relation agreement between router and runtime), table events see the stored row as `xRec`. Found a grammar defect while testing subscribers: an attributed `local procedure` right after a var section loses its attribute (`findings/grammar-attribute-after-var.md`, with a tested scanner patch for the grammar repository). It hides such subscribers and publishers from the call graph, the router and the interpreter.

## 2026-09-26 19:17 BST: headless session, round 8 queued, five agents again

- The 18:32 session's five agents were dead at the start: no live process, an empty session log,
  no commits from the mutants and triage agents, five findings from the round 8 reviewer
  (`findings/r8-session-review.md`, committed as 32ca741f), and the fix-r7 worktree still holding
  its uncommitted SEC-7 fix, now 296 lines.
- Origin was six commits ahead (1a081fbe..aa01cb97), pushed by the second session between 18:30 and
  19:08 BST: JSON types run in the interpreter (`json.rs`, reference semantics through an arena),
  table events get the stored row as `xRec`, a subscriber's `sender` parameter is passed, one
  `validate_relation` decides Validate routing for the router and the runtime, XML `Format` no
  longer groups thousands. Its own review fixed two of the five round 8 findings before the
  reviewer wrote them up (RT-3, and the `xRec` half of RT-1). CI on PR 32 passed on every push
  except the ubuntu job still running at 19:20. Fast-forwarded.
- Merged `campaign/slop-batch-11` (2b7bce37): the desloppify re-score (overall 80.2 unchanged,
  strict 79.6, the scan surface grew with the merges, section 5 of `findings/desloppify.md`),
  the `get_` prefix dropped from three al-dap hub accessors, a crate doc for zed-al. The agent
  found `desloppify scan` returns 7 files and 0 lines while sibling worktrees are busy and wrote
  down how to spot it.
- Five agents dispatched at 19:30 (`STATE.md`): round 7 fixes, round 8 review continuation, round 8
  fixes on a new branch `campaign/fix-r8-review`, the audit backlog triage, `cargo mutants`.

## 2026-09-26 21:44 BST: headless session after two hours of limits, mutants merged, six agents

- The 19:17 session hit its session limit at 19:53 (reset 21:40). Every headless start from 19:53
  to 21:34 exited at once on both models. Ten commits were unpushed (d1afbef3..79dc5087, the
  slop batch 11 merge and the round 8 findings), pushed now. The five agents were dead: the triage
  and plugin agents had written nothing, the reviewer left 430 lines of scratch tests, the fix-r7
  worktree still held the 317-line SEC-7 diff. All three saved as patches under `.campaign/`.
- Merged `campaign/test-mutants` (7e4c02f3): 12 commits, tests only, for five of the ten shortlist
  files (`method_id.rs`, `http_auth.rs`, `sort.rs`, `documents.rs`, `filter.rs`) and
  `findings/mutants.md` with the setup, the per-file runs and the survivors marked equivalent.
- `.campaign/run-gates.sh` now runs the semantic clippy and rustdoc lines too. Gates on 7e4c02f3
  started in the background. The `slop-11` worktree removed, 35 GB free.
- Six agents dispatched at 21:50 (`STATE.md`): round 7 fixes, round 8 fixes, round 8 review
  continuation, audit triage (third attempt, writes every five items), `cargo mutants` (the last
  five files), plugin leftovers (third attempt).
- Gates on the mutants merge (7e4c02f3): fmt, release build, both clippy runs and rustdoc clean.
  94 suites, 5209 passed, 0 failed, 10 ignored, at load average 9 with six agents starting.
  Pushed.

## 2026-09-26 22:35 BST: round 8 reviewed and half fixed, plugin merged, triage done

- The round 8 reviewer finished (`findings/r8-session-review.md`, `## Review complete`): 8 more findings, 3 high. `ReadFrom` overwrites a JSON variable's node in place, so a variable reused in a loop rewrites the rows already added. The runtime finds a table by name with no kind, so a page of the same name indexed first breaks every record operation on that table. `DeleteAll` and `ModifyAll` raise no table events. Mediums: JSON methods ignore statement position, `SelectToken` has no filters or `..`, the typed getters ignore `DefaultIfNotFound`, transaction lint and the native debug adapter still credit a file's first object. Re-check: aa01cb97 closed RT-3 for codeunit publishers and 8fd1aca1 narrowed RT-1. The persisted index store, the ghost race fix and the hand merges held up.
- Merged `campaign/fix-r8-review` (a1afe8e6): the first four round 8 findings, with a full rename cascade over plain table relations (`interpreter/records/relations.rs`) instead of the router fallback. Gates on the merge: fmt, release, both clippy runs and rustdoc clean, 94 suites, 5223 passed, 1 failed, 10 ignored. The failure is `snapshot_start_server_error_maps_to_internal_error` (al-lsp), -32602 for -32603 at load 10, green alone: queued as shared mock state.
- Merged `campaign/ai-plugin-leftovers` (cf0f794c): `plugin/evals/` (12 cases, all with a `jq` or substring ground-truth check against `al-explorer`, `make plugin-evals` passes 12 of 12) and `plugin/scripts/al-fetch-release.sh`, called from the SessionStart hook when neither binary is found: https only, `binary-checksums.txt` verified before anything is executable, one line of output. Tested against a local server with the real v0.2.2 archive, a mismatched digest and an http URL. No tagged release publishes `binary-checksums.txt` yet, so the real download refuses until the next release. `make plugin-validate` passes.
- Audit backlog triage done (`findings/audit-backlog-triage.md`): 253 rows, 249 fixed, 3 open (queued), 1 unclear. One Sonnet pass decided all of them, so round 9 spot-checks ten.
- Three more agents: round 8 second batch (runtime and JSON), round 8 multi-object (lint and native DAP), grammar corpus round 2 in the submodule. The mutants agent resumed after its `lint.rs` run.

## 2026-09-26 22:55 BST: merge of the second session's codeunit instances

- Origin had three commits from the second session: codeunit variables are instances with their
  own globals (9a9ce662: a helper's globals are kept between its calls and pushed under each call,
  a `SingleInstance` codeunit has one instance for the run, an event subscriber gets a fresh one,
  and the router now sends only a SingleInstance helper with globals to live BC), and a grammar
  finding (`findings/grammar-attribute-after-var.md`: attributes after a `var` section are hidden),
  handed to the grammar corpus agent.
- Two conflicts with the round 8 labels fix, resolved for the second session's rule (6b4be394):
  `globals_for_call` decides a call's globals and the "stateful codeunit requires live BC" refusal
  is gone, so `object_has_global_variables` went with it. The router's reason names a SingleInstance
  helper, so the `object_kind` field the labels fix added is gone too. The labels test now expects a
  table with a real global variable to run locally. Open question for round 9: a table's globals get
  a fresh frame per trigger call locally, where BC keeps them per record variable.
- al-runtime 610 and al-test 163 tests pass on the merge, fmt and clippy clean. Full gates running.
- Gates on 6b4be394: fmt, release, both clippy runs and rustdoc clean, 94 suites, 5225 passed,
  0 failed, 10 ignored. Pushed.

## 2026-09-26 22:55 BST: round 8 multi-object fixes merged

- Merged `campaign/fix-r8-multi` (bd76f89f): each `ObjectSummary` keeps the effect sites of its own
  declaration, so transaction lint warns about a write in a TryFunction of a dependency file's
  second object (`SourceFileSummary::effects` and `file_effect_sites` removed, `SCHEMA_VERSION` 3,
  snapshot rewritten). The native debug adapter resolves every breakpoint to the last object
  declared at or above its line, through `dap_mode::object_at_line`, which the daemon now shares. A
  `setBreakpoints` that resolves to no object now removes the file's earlier BC breakpoints.
- Gates on the merge: fmt, release, both clippy runs and rustdoc clean, 94 suites, 5227 passed,
  1 failed, 10 ignored. The failure is the snapshot profiling mock test again (3 of 4 workspace
  runs tonight, green alone every time), so a Sonnet agent now isolates its shared state. Pushed.

## 2026-09-27 02:41 BST: headless session, six dead agents recovered, grammar cc31863 merged

- The 21:44 session's agents were all dead at 02:41, the machine idle, origin level with local.
  Recovered: the grammar corpus agent had finished (12 commits on the grammar's campaign branch,
  `findings/grammar-corpus-r2.md` with every coverage item ticked, GR2-1 fixed in the scanner,
  GR2-2 to GR2-5 open for the interpreter). The round 8 fix agent had fixed RT-4, EV-1 and JSON-1
  to JSON-4 and left the RT-3 table publisher test uncommitted (`.campaign/r8-rt3-test-wip.patch`).
  The round 7 fix agent had fixed 12 of 15 with the BLOG-3 fix and the DOC-2 tests uncommitted in
  its worktree. The mutants agent had committed `lint.rs` (152 mutants, 140 caught, 3 equivalent)
  and `mock/record.rs`. The flake agent had written nothing. The merged plugin worktree removed.
- Merged `campaign/fix-r8-review` (12b1778a): tables, enums and codeunits found by kind, `DeleteAll`
  and `ModifyAll` raise the table events per row and reach subscribers in the router, `ReadFrom`
  gives a JSON variable a new node, JSON failures follow statement position, `SelectToken` follows
  filters, `..` and `*` and refuses slices and unions (the router sends those live), typed getters
  honour `DefaultIfNotFound`, `SCHEMA_VERSION` 4. Merged `campaign/test-mutants` (58781aa7).
- Grammar gates on cc31863 (`.campaign/run-grammar-gates.sh`): 98 of 98 corpus tests, 46,389 of
  46,389 corpus files parse, crate and generator tests, `cargo package --list`, the wasm build and
  the generator drift check all pass. Pushed to the grammar's `campaign/2026-09-21`. Submodule
  pointer and `extension.toml` rev moved (4c1b0ae6). Superproject gates on 4c1b0ae6 started 02:48.
- Five agents dispatched at 02:48 (`STATE.md`): round 7 fixes (the last three), runtime fixes
  (RT-3, GR2-2, GR2-3, GR2-5, the OnValidate call graph item), the snapshot flake (second attempt),
  mutants on the last three files, the round 9 review with ten audit triage spot-checks.

## 2026-09-27 03:06 BST: gates green on the grammar move, round 7 complete and merged

- Gates on 4c1b0ae6 (the round 8 batch, the mutation tests, grammar cc31863): fmt, release, both
  clippy runs and rustdoc clean, 94 suites, 5261 passed, 0 failed, 10 ignored. Pushed. CI on PR 32
  was green on every job for the previous push (131ebdd5).
- The round 7 fix agent finished the last three: projected rows keep `partial` and
  `partial_reason` and a name a partial row lacks goes in `absentFields` (BLOG-3), the 31 help
  examples say `al-explorer` and the fish completion example writes `al-explorer.fish` (DOC-2),
  the dispatch module doc names `MAX_RECURSION_DEPTH` (DOC-1). Merged `campaign/fix-r7-review`
  (fac24900, 15 of 15). Gates on the merge: clean, 94 suites, 5291 passed, 0 failed, 10 ignored.
  Pushed. The fix-r7 and fix-r8 worktrees removed (`git worktree remove` refuses a worktree with a
  submodule, so `rm -rf` and `git worktree prune`), 34 GB free.

## 2026-09-27 03:25 BST: round 8 closed, round 9 reviewed, three merges

- The runtime agent fixed all five: a table procedure's event with IncludeSender passes the record
  as `var Sender` (the router already kept that test local), variables named after type or object
  keywords read as names and a no-argument method on such a variable runs without parentheses
  (the router had sent any test with a local named `Page` or `Report` to live BC and now checks
  declared variables first), a `signed_case_label` evaluates through the index code, a subscriber
  bound by a bare object ID runs and `get_events` reports the publisher's name, and a call graph
  node gets the edges of every declaration of its name (the queued report said only the first
  `OnValidate` got edges, the repro showed the last one, and overloads had the same bug),
  `SCHEMA_VERSION` 5. Left for the grammar: `-5..-2` as a range label is one token.
- The docs agent corrected six claims in `native-test-runtime.md` and `debugging-dap.md` and read
  eleven other docs with nothing to change (`findings/docs-review.md`, re-check section).
- The round 9 reviewer (`findings/r9-session-review.md`) found 7: the hand merge 6b4be394 dropped
  the router rule that sent a table with globals to live BC while the runtime gives a table's
  globals a fresh frame per trigger call, where BC keeps them with the record variable (high),
  `Clear(X)` resolves to the LibraryVariableStorage stub and resets nothing (medium),
  `al-fetch-release.sh` verifies regular files only so a symlink named `al-lsp` installs
  (medium), a subscriber joins its codeunit's running instance, the release script reports
  "installed" when the install failed, the eval runner hides a missing `al-lsp`, the summary
  cache hashes the package before summarizing it (all low, the last one reproduced after two
  rounds of reading). Merge damage: ten of thirteen merges equal the automatic merge, the rest
  are docs or the regression above. Ten audit triage spot-checks all hold.
- Merged `campaign/fix-r8-runtime` (8f70b75e), `campaign/docs-r8` (6516172a) and
  `campaign/r9-review` (e64b982a). fmt clean in the main checkout. Gates on e64b982a started
  03:17.
- Three more agents (`STATE.md`): round 9 runtime fixes, round 9 plugin script fixes with a shell
  test wired into `make plugin-validate`, and GR2-4 in a grammar worktree.

## 2026-09-27 07:49 BST: the 03:25 session's gates pushed, seven agents re-dispatched

- Gates on e64b982a (the 03:17 run): fmt, release, both clippy runs and rustdoc clean, 94 suites,
  5302 passed, 0 failed, 10 ignored. The 03:25 session died before pushing. Pushed 07:50
  (8feb7c8e, 24 commits).
- All seven agents of the 03:25 session were dead with the machine idle. Security round 5 had six
  findings committed on `campaign/r5-security` (SEC5-1 to SEC5-6: a symlink moves the loaded code
  outside the hashed tree, a tree over 50,000 entries hashes to a constant, native libraries beside
  an analyzer are not hashed, the legacy proxy forwards a Sandbox or Production scenario with
  Windows or UserPassword authentication to its `server`, an analyzer named in
  `al.compilationOptions` is recorded as text, a replaced runtime beside a trusted `dotnet` does
  not move `inputs_fingerprint`) with coverage items 2, 3, 6 to 9 unread. Its scratch tests are
  saved as `.campaign/sec5-scratch-tests.patch`. The plugin runs agent had committed two doc fixes
  and no round 5. The mutants agent had two uncommitted `composition.rs` tests. The other four had
  written nothing.
- Seven agents re-dispatched onto the same worktrees and branches (`STATE.md`). The flake agent
  now has the two test names (`snapshot_start_posts_and_parses_id`,
  `snapshot_start_server_error_maps_to_internal_error`, al-lsp) and the wrong value (-32602 for
  -32603 at load), which the first two attempts lacked.

## 2026-09-27 08:05 BST: the snapshot flake was already fixed, four interactive commits merged

- The flake agent reproduced `snapshot_start_server_error_maps_to_internal_error` 11 times in 40
  runs of `cargo test -p al-lsp --lib daemon -- --test-threads=12`: the containment tests set
  `XDG_CONFIG_HOME` behind their own mutex, not the `serial_test` lock the other trust tests use,
  so a containment test could repoint the trust store while a snapshot test awaited its mocked
  server, and the authorisation then failed with -32602. Its fix was the change 17bd4758 already
  made on 2026-09-26 17:45 (the agent's worktree branched from 58781aa7, which lacks it), so the
  merge conflicted on the same lines and the branch was dropped. The two earlier flake dispatches
  after 17bd4758 were unnecessary. `campaign/fix-snapshot-flake` deleted, its worktree removed.
- CI on 5abcfd5c: all six jobs green.
- The interactive session pushed four commits (af3a36cd, 404f2fb4, 8cf97b56, b2577ceb) between
  07:56 and 08:10: List and Dictionary as reference types with List range methods, the subscriber
  test's var section moved back above the subscriber (the queued GR2-1 item), a `pack-native`
  refusal test that compares the folder as the user named it, and a `clippy.toml`. Fast-forwarded
  onto them. They go through the next gate run and the round 10 review.

## 2026-09-27 08:12 BST: security round 5 complete, plugin round 5 merged, GR2-4 in the grammar

- Security round 5 complete (`findings/r5-security.md`, merged c41346e9): 9 findings, 7 medium, 2
  low, all nine coverage items ticked. New this session: the legacy proxy forwards an online
  scenario whose `applicationFamily` puts another host in front of Microsoft's domain, reproduced
  with the deployment library sending a bearer token to a local TLS listener (SEC5-7), an analyzer
  name from Zed user settings resolves to a project copy the record never lists (SEC5-8), and a
  `.alpackages` link added after trust widens the daemon's containment to the link's target
  (SEC5-9, `/` as a root accepts `$HOME/.bashrc`). One Windows-only candidate (a UNC path resolved
  while deciding trust opens an SMB connection) is noted under item 3, unverified. A fix agent is
  on all nine (`campaign/fix-r5-security`).
- Plugin runs round 5 merged: four Haiku runs (`bc-test-locally`, `bc-upgrade-impact`,
  `bc-cop-fixer`, a `.alpackages` symbol lookup on a scaffolded project with Base Application 28
  symbols) all right, after three fixes: `bc-upgrade-impact` never fired on a dependency question
  (its description and the SessionStart routing note), `by-id` undercounts fields when an
  extension is loaded, and `composed --limit --fields` does not shrink its output (recipe now
  pipes through `jq`). `make plugin-validate` OK, `make plugin-evals` 12 of 12.
- GR2-4 fixed in the grammar (`campaign/gr2-4`, three commits on cc31863): the scanner reads a
  signed case label as alc does (blanks after the minus, `::` and member access, stops before `:`
  and `..`, and does not run where a binary operator is valid, which also fixes `Y := A -1;` as the
  last statement of a case arm, an ERROR before). The `operator` regex stops before a sign after a
  dot so `-5..-2` is four tokens. The grammar route (an optional unary minus) was tried and
  rejected: the lexer picked the unary token at the end of an arm without a semicolon. Agent gates:
  103 of 103 corpus tests, 46,389 of 46,389 corpus files, crate and generator tests, package,
  wasm, drift. Merged into the grammar's `campaign/2026-09-21`, orchestrator gates running.
- The interactive session's four commits carry `Co-Authored-By: Claude` and `Claude-Session:`
  trailers, which `~/.claude/CLAUDE.md` forbids. They are pushed, so left as they are.

## 2026-09-27 08:16 BST: round 9 plugin fixes merged, round 10 dispatched

- Merged `campaign/fix-r9-plugin` (7cd0a904): the release script refuses an archive whose members
  are not regular files or directories and a binary that is a link or a directory, chains the
  install steps and names the one that failed, and the eval runner resolves `al-lsp` beside
  `al-explorer` then on `PATH`, skips with the reason when none is found, and keeps a failing
  check's stderr. New `plugin/tests/al-fetch-release-test.sh` (a local `python3 -m http.server`
  serves the archives) runs from `make plugin-validate`, which CI now runs on ubuntu, and the
  ShellCheck glob covers `plugin/tests/*.sh`. Finding IDs taken out of the validator and CI
  comments (e60e8db7).
- Round 10 review dispatched (`campaign/r10-review`, worktree `agent-r10`): scope
  `a0e85e0b..9e3f26a1 -- crates plugin` (49 files), which covers the round 8 last batch, the round
  7 fixes, the grammar pointer move, the interactive session's List and Dictionary reference
  semantics, and the plugin round 5 fixes. Nine coverage items, ends with `## Review complete`.
- Earlier entries this morning carried clock times about twenty minutes ahead of the machine's
  clock (the orchestrator estimated them). Corrected to the times in the git log.

## 2026-09-27 08:27 BST: grammar a108400 pushed, pointer moved, gates green

- Grammar gates on a108400 (the GR2-4 merge): 103 of 103 corpus tests, 46,389 of 46,389 corpus
  files parse, 20 crate tests, 16 generator tests, package, wasm and drift all pass. Pushed to the
  grammar's `campaign/2026-09-21`. Submodule pointer and `extension.toml` rev moved (b3d5121b).
- Superproject gates on b3d5121b (the new parser through the path dependency, the interactive
  session's List and Dictionary work, the plugin fixes): fmt, release, both clippy runs and rustdoc
  clean, 94 suites, 5304 passed, 0 failed, 10 ignored. Pushed.

## 2026-09-27 13:05 BST: round 9 fixes and round 10 review merged, grammar at 142aba6, five agents out

- The 07:49 session died with five agents dead and nothing merged after 68f0bbe8. Recovered:
  the round 9 fix branch was complete (7 of 7 plus the `-5..-2` interpreter test, 217af726) and
  is merged (52d07278). The round 10 review (`findings/r10-session-review.md`) had all nine
  coverage items ticked and 11 findings, 1 high, 5 medium, 5 low, but no completion block: merged
  (817f137f), block added (c896c10c), its 1,569 lines of scratch tests saved as
  `.campaign/r10-scratch-tests.patch`. The high finding: since List, Dictionary and JSON values
  became handles, `A, B: List of [Integer]` binds one list to both names.
- Grammar corpus round 3 (`findings/grammar-corpus-r3.md`): seven commits on the grammar branch,
  six corpus files, GR3-1 fixed in the grammar (`X:=-1`, `A*-1` and `X<-1` lexed the sign into the
  operator; a sign now starts a unary expression after any operator). Grammar gates on 142aba6
  green: 119 of 119 corpus tests, 46,389 of 46,389 repository files, crate 20, generator 16,
  package, wasm and drift clean. Grammar branch fast-forwarded and pushed, pointer and
  `extension.toml` rev moved (4c429ac5). GR3-2 (indexing a keyword-named variable) and GR3-3
  (fourteen node kinds and three field names the interpreter matches that the grammar does not
  produce) go to the round 10 fix agent B.
- Dispatched 13:02: round 10 fixes A (collections and references, Opus), B (dispatch, expressions,
  GR3-2, GR3-3, Opus), text (plugin prose and comments, Sonnet), the mutants continuation on the
  formatting module (Sonnet), and the docs re-check after the round 9 merges (Opus). Three idle
  worktrees were repointed onto the new branches to reuse their build directories.
- Superproject gates on 4c429ac5 green: 92 suites, 5331 passed, 0 failed (the harness suite ran on the
  release binaries built in the same run). Pushed 13:12 (ccdf471b). CI on 68f0bbe8: all six jobs green.

## 2026-09-27 13:10 BST: round 10 batch A, text fixes and docs re-check merged

- `campaign/fix-r10-text` merged (9a364d46): the three plugin claims corrected against the release
  binary (implicit dependencies come from `application` and `platform`, `--limit` and `--fields`
  act on `composed`'s `extensions` array, AL-NL003 and AL-NL004 named), eleven comment and prose
  lines rewritten.
- `campaign/docs-recheck-3` merged (aaa3ca37): 13 claims corrected in `native-test-runtime.md`
  (dispatch order, `Clear`, table globals, `Sender`, the `var` write-back into a record field,
  subscriber instances, `ModifyAll` and `DeleteAll` globals), `analysis-and-insight.md` (record
  operations and table code as event edges, one node per name), `project-trust.md` (where linked
  package folders come from, seven `stat` calls), `semantic-bridge.md`, `cli-commands.md` and
  `settings.md` (project analyzer copies and `compilationOptions`). Twelve docs read with nothing
  to change. Two notes queued in `STATE.md`.
- `campaign/fix-r10-a` merged (19a7cae9) with two conflicts taken from the fix side: 5 of 5 fixed,
  each with a test that failed first. `Value::List` and `Value::Dict` wrap a `Collection<T>` that
  keeps the declared element or key type, `Value::TextBuilder` holds a `Shared<String>`. al-runtime
  643 lib tests on the branch. Gates on the merge in progress.

## 2026-09-27 13:22 BST: round 10 batch B merged, round 10 fixed 11 of 11

- Gates on 19a7cae9 (batch A, text, docs): 92 suites, 5336 passed, 0 failed.
- `campaign/fix-r10-b` merged (85426311, no conflicts): 6 of 6 fixed, each with a test that failed
  first. An overloaded procedure runs the declaration whose parameters accept the arguments (most
  exact type matches win, then declaration order), a codeunit publisher's `sender` is the running
  instance (a `CallFrame` now carries its instance id), a quoted variable name reads, a record
  method or table procedure written without parentheses runs when the table has no field of that
  name, `Page[1]` indexes a keyword-named variable, and the interpreter and router no longer match
  fourteen node kinds and three field names the grammar does not produce (a guard test in
  `al-test/tests/node_kind_literals.rs` found four more than GR3-3 listed). Left open in the
  status lines: the router does not classify a record method without parentheses, so an
  unsupported one routes locally. Round 10 is 11 of 11 fixed.
- Gates on 85426311 in progress.

## 2026-09-27 13:26 BST: pushed, test module splits merged

- Gates on 580151c1: 93 suites, 5343 passed, 0 failed. Pushed.
- `campaign/slop-splits-3` merged (b5c05341): `trust.rs` 3589 to 1956 lines and `lsp_dispatch.rs`
  2562 to 1409, the test modules in `trust_tests.rs` and `lsp_dispatch_tests.rs` as `#[path]`
  child modules, 257 al-project and 724 al-lsp lib tests with the same names. The
  `inputs_fingerprint` comment now says up to seven `stat` calls, which matches the doc corrected
  in the re-check. Gates on the merge in progress.

## 2026-09-27 13:40 BST: round 11 review merged, three fix agents, security round 6 and plugin round 6 out

- Gates on b5c05341 (the splits): 93 suites, 5343 passed, 0 failed. Pushed (14381c32).
- Round 11 review merged (16514e84): 7 findings over the security round 5 fixes, the round 9
  fixes and the grammar move. Medium: a user settings analyzer path that resolves into the
  project loads a file the record does not list (the round 5 rule covered bare names only), and
  Microsoft's deployment library reads `authentication` with `Enum.TryParse(ignoreCase)`, so
  `" Windows"`, `"2"`, `"3"` and `"AAD,Windows"` mean Windows or UserPassword to it while the proxy
  judges them online and forwards them without trust (checked with a .NET 8 probe against the
  library). Low: an unreadable settings file leaves `AL_DOTNET_PATH` on the project's `dotnet`,
  nested table code of two records of one table binds the wrong globals, an array element as a
  `var` target is dropped silently, the eval runner reports a mismatched al-lsp as wrong answers,
  and writing rule breaches. No merge damage in the five merges. Ten scratch tests saved as
  `.campaign/r11-scratch-tests.patch`.
- Dispatched: round 11 fixes on three branches (security, runtime, plugin and text), security
  round 6 (links in the trust inputs, proxy key folding, the summary store, hostile AL in the local
  runtime, daemon methods since round 4, plugin scripts, terminal escapes), plugin round 6 runs.
  The mutants agent is still on the formatting module.

## 2026-09-27 13:46 BST: round 11 security and runtime fixes merged, plugin round 6 merged

- `campaign/fix-r11-sec` merged (0d851d9d): 3 of 3, each with tests that failed first. `resolve`
  checks the trust record for every analyzer entry that resolves inside the project, and a path
  the user's settings name is recorded with its hash like a bare name (a path the project's own
  settings write is now recorded twice, so one existing record goes stale once). The proxy refuses
  an `authentication` that is not `Windows`, `UserPassword`, `AAD` or `MicrosoftEntraID` in ASCII
  case, before the on-premises rule. `enforce_dotnet_path` removes a project `dotnet` from
  `AL_DOTNET_PATH` when the settings cannot be read and names the file. `project-trust.md` updated.
  al-project 263, al-lsp lib 725, al-compile 43.
- `campaign/fix-r11-rt` merged (dab2eb06): 2 of 2. `write_var_argument` writes an array element
  through the indexed assignment path and errors on any shape it cannot write (a quoted name with a
  space was also dropped before). A literal passed to a `var` parameter is now an error, as alc
  17.0.34 reports (AL0130), so `var_param_non_lvalue_arg_is_not_written_back` became
  `a_literal_passed_to_a_var_parameter_is_an_error`. `run_on_active_globals` moves the active
  record's globals frame to the top for a callback and back afterwards. al-runtime 650.
- `campaign/plugin-runs-6` merged (b2c38e6a): the five skills never run against `.alpackages` and
  `package-diff` with Base Application 25 and 26 all answered right on the first try (276 to 4,446
  bytes of tool context per question), the fixture byte counts measured, `## Not covered` down to
  the release with `binary-checksums.txt` and a repeatable eval fixture. One binary defect queued:
  the daemon kept a stale `app.json` in memory after an edit on disk.
- Gates on b2c38e6a: 93 suites, 5352 passed, 0 failed. Pushed. Still out: mutants (formatting module), security round 6, the
  round 11 plugin and text fixes.
