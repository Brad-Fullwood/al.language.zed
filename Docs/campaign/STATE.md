# Campaign state

Updated: 2026-09-26 13:05 BST. Branch: `campaign/2026-09-21`. Ends: 2026-09-28.

## Phase

Round 2. Every round 1 finding is fixed, rejected with evidence, or queued. Two adversarial reviewers read the whole campaign diff (`git diff dev..campaign/2026-09-21`, 277 files) looking for fixes that do not fix, merge damage and regressions. A fix agent works the queued items.

## In flight

Session 2026-09-26 11:50 BST (headless, Fable orchestrator). Five agents re-dispatched onto the
branches the 07:00 session left, each in its existing worktree under `.claude/worktrees/`:

- `cargo mutants` (E): worktree branch `campaign/test-mutants`, 3 of 10 files done (`method_id.rs`
  42 mutants 2 missed, `http_auth.rs` 26 and 3, `sort.rs` 107 and 20, tests added for the misses).
  Three uncommitted proptest seeds in `property_formatting.proptest-regressions` were written while
  mutants were active and are checked against clean code first. Seven files left.
- Persisted symbol index (G): worktree branch `campaign/ai-persisted-index`, five commits (baseline,
  design, call graph from source summaries, summaries instead of trees, persisted summaries per
  package). Step 4, the after measurement, and the merge of today's campaign branch remain.
- Ghost race (A): worktree branch `campaign/fix-ghost-race-2`, nothing committed by the first agent.
  Told to name the mechanism and whether it predates the lock batch before changing code, and to
  write `findings/ghost-race-2.md`.
- Round 7 fixes (A, D): review done 13:00 (`findings/r7-session-review.md`, 15 findings: 1 high,
  5 medium, 9 low, 6 of the 14 round 4 fixes have a gap). Fix agent on worktree branch
  `campaign/fix-r7-review`, highs first.
- Blog re-read (I), dispatched 12:13: articles 7, 8, 9 against the closed security round and the
  merged blog fixes, `blog-plan.md` numbers, unsloppify and humanizer, `pnpm validate`. Appends
  `### Re-read 2026-09-26 after the security round` to `findings/blog-progress.md`.

Merged this session: `campaign/fix-r4-security` (14 of 14), `campaign/fix-blog-findings` (3 of 4),
`origin/dev`, `campaign/docs-review` (done, 12:20), `campaign/fix-blog-findings` again for finding 4 (12:50). Gate result on the merge is in `LOG.md`.

PR 30 was merged into `dev` on 2026-09-25 (afec75d1). CI runs on pushes to `main` and `dev` and on
pull requests, so draft PR 32 (https://github.com/Brad-Fullwood/al.language.zed/pull/32, base `dev`)
now runs CI on every push of this branch.

A fix branch on origin or in `.claude/worktrees/` with commits not in this branch and no live
agent means the agent died: re-dispatch onto that branch.

Remote branch cleanup: every `campaign/*` branch except this one is merged into it. Cloud
sessions can push only to `campaign/2026-09-21`, so deleting them is left to Brad (the list is in
`LOG.md`, 2026-09-24).

Queued:

- al-dap and al-publish post to different BC dev endpoints (needs a live server to settle).
- desloppify fix batches (`findings/desloppify.md` section 4), file splits after the owning fix branch merges.
- Workstream E (tests): coverage by crate, property tests for parser and interpreter, `cargo mutants` on al-runtime and al-analysis. Start when a build slot frees.
- Blog: re-read articles 7, 8 and 9 now that the security round is closed, then merge to `main` (Brad's call). The fact pass list is in `findings/blog-progress.md`.
- Plugin leftovers: `plugin/evals/`, release binary download hook, test on a project with `.alpackages`.
- Windows named pipe owner check (documented, no Windows machine in the campaign).
- A wedged daemon request can hold a daemon past its idle window (warns every 60 s).

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
| C | Slop and simplification: desloppify plan, per-crate simplify pass | Batch 3 (four file splits) and the 112 item review queue merged. Strict 79.9. 2026-09-24: test modules split out of six large files, rustdoc warnings 46 to 0 (CI gated), bulk-fix errors typed | Fresh `desloppify review` to re-score, then the 63 deferred items (typed RPC boundary is the largest), batches 10 (async locking) and 11 (docs and API hygiene), remaining file splits (`resolution.rs` 2754 lines, `dispatch.rs` 3243, `tests_dispatch.rs` 4216, `lsp.rs` 3415, `native_dap.rs` 3636) |
| D | Security: credentials, archive parsing, MCP and daemon input, extension binary download, supply chain | Four review rounds (8, 19, 10, 14 findings), all fixed and merged. Project trust, dispatcher capability registry, peer-checked endpoint, trust digest over analyzer and dotnet file hashes, credential authorisation on every DAP and test path | Windows named pipe owner check. A fifth round over what changed after 2026-09-26 |
| E | Tests: coverage by crate, property tests, `cargo mutants` | First pass merged: 4 bugs found by property tests, coverage table, CI job proposal | Nightly property job added (`property-nightly.yml`, 8192 cases; the per-PR run already covers 128). Next: `cargo mutants` on the 10 file shortlist in `findings/test-depth.md`, make `al-test/backends/snapshot.rs` testable |
| F | Grammar: corpus tests, query drift between `languages/al` and `tree-sitter-al/queries` | R1 review running | From R1 findings |
| G | AI tooling: make this project speed up and sharpen AI work on Business Central (see below) | Inventory, measurements and design done (`findings/ai-tooling-ideas.md`): latency is 4 to 150 ms warm, but 14 of 20 measured answers are too large for an agent (up to 9.4 MB). Plugin build running | Daemon projection work after the LSP fix branch merges |
| H | Docs: `Docs/`, `README.md`, `ROADMAP.md` match the code, then unsloppify | Done 2026-09-26: every user doc checked against the code and given a plain-wording pass (`findings/docs-review.md`) | Re-check the docs each later merge touches |
| I | Blog: replace the six articles with a new series on the current project, unsloppify each | On blog branch `campaign/2026-09-rewrite`: six posts deleted, site cleaned, `pnpm validate` passes (it failed on `main`), fact sheet and nine-article plan in `findings/blog-plan.md`, articles 1 to 8 drafted (8 of 9), article 9 (the campaign retrospective) is written last, article 1 `trace` paragraph corrected | Articles 2, 4, 5, 8 after the fix branches settle, article 9 last, final fact pass, merge to `main` |

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

- 2026-09-26 docs review merged (`findings/docs-review.md`): every user doc checked against the code, four drift items fixed, plain-wording pass over `Docs/`, `README.md`, `ROADMAP.md` and `plugin/`.
- 2026-09-26 security round 4 merged: 14 of 14 fixed (`findings/r4-security.md`). Every DAP and test-run path authorises the target before a credential leaves the machine, a scheme-less server is `https`, XLIFF methods are contained, the trust digest hashes repository-resident analyzers and `dotnet`, the handshake proof is checked before the build identity, a linked `.alpackages` is an untrusted package cache, the semantic bridge is in the release digests, the scaffold and native build refuse symlinks, `trust --yes` is pinned to a reviewed digest, the language server re-gates settings when trust inputs move.
- 2026-09-26 blog findings 1 to 4 merged: `packages` counts skip synthetic Option enums, the client deadline extends while the call graph builds, `object` and `byId` answer without the graph, the `${Name}` analyzer token spelling is a builtin so a fresh untrusted scaffold passes `pack-native --validate`.
- 2026-09-26 async locking batch merged (`findings/async-locking.md`): two deadlocks in al-lsp, DAP proxy stdout lock per frame, dead allow attributes removed, SAFETY comments on every unsafe block.
- 2026-09-24 r6 session review: 14 of 14 fixed (`findings/r6-session-review.md`). File-index re-index race closed, typed bulk-fix errors, six large files lost their test modules to `tests.rs`, rustdoc at zero warnings and gated in CI.
- 2026-09-24 r5 dogfood closed: 31 of 32 findings fixed, 1 partly (`findings/r5-dogfood.md`). Table and table-extension changes reach their tests, plain Insert/Modify/Delete raise table events in the call graph, `graph --scope workspace`, `impact` call/write/read, LSP-shaped JSON and readable text for completions/hints/symbols/folding, suggest-event says why a trace is partial.
- 2026-09-24 features and CI: all six CI jobs green on PR 30 (first time this campaign; ubuntu ShellCheck, Windows data directory and output-pipe inheritance, macOS `/var` and `/tmp` symlinks). `package-diff <old.app> <new.app>` (Base Application 25 to 26: 1,137 changes, 7 s, only the ones the workspace uses), `obsolete --used`, a `.al` file watcher in the LSP server, a text outline for `symbols`, and the `bc-upgrade-impact` skill rewritten around them (it had told agents `obsolete` lists workspace uses and `breaking` diffs dependencies; neither did).
- 2026-09-24 dogfood pass: a project scaffolded with `al-explorer new` against Base Application 26 symbols from the public NuGet feed (`Docs/campaign/LOG.md`). Fixed: `download-symbols` never returned after a successful download (the daemon waited on its own project read guard), the client failed a request on EINTR, the daemon never saw files written after it started (now an incremental scan per request), native compile rejected fields a table extension adds (ALN2404), go-to-definition on such a field opened the package outline, `free-ids` gave dependency tables field numbers outside `idRanges` and listed workspace extensions twice, `--fields` printed `?` columns in text mode, `al-explorer new` failed outside an AL project and wrote the nil GUID as app id, the daemon startup error lost the file it named. CI: ShellCheck SC2015, Windows data directory, macOS `/var` symlink refusal.
- 2026-09-24 session: merged `fix-formatter-idempotence` (the persisted seed passes), `fix-ghost-diagnostics` and `slop-splits-2`. Diagnostics publishes now hold the generation read lock from the currency check through the send, so a didClose cannot slip between them (the harness test passes 32 of 32 at 12-way load). One Windows browser opener in al-types (`rundll32 url.dll,FileProtocolHandler`, http(s) only) replaces the two `cmd /c start` copies that split OAuth and debugger URLs at `&`. `pack-native --validate` runs the project's analyzers through the trust gate, `--analyzers` overrides. The harness judges a binary stale only against the crates it links. Review A: `extract_table_relation_table` removed, `PermissionAuditReport` doc restated. Gates: fmt and clippy clean, 95 suites, 5011 passed, 1 failed (`a_failed_file_write_leaves_the_whole_workspace_unchanged`, which needs a non-root user: the cloud container runs as root, which writes through a 0555 directory).
- Merged `campaign/fix-r3-security`: 10 of 10. The MCP advisory names keys only. The Zed extension ignores `binary.path`, `binary.arguments` and the debug adapter path from any settings (WASM sandbox cannot read the trust store), `PATH` is the way to run a specific build. `al-explorer trust` confirms on a terminal. Unix socket ownership and peer uid checked before connect, HMAC handshake with a per-user key. `snapshot` and `profiling` gated. Revoke takes effect per request. Open: Windows named pipe owner check (documented). Gates: 94 suites, 4995 passed, 1 failed (formatter seed).
- Merged `campaign/fix-r2-review-b`: 19 of 19. The daemon dispatch match is generated from a capability registry (`dispatch_table!`), so every method declares whether it reads a path or reaches a credential, and tests drive those declarations end to end. `rename` contained, `tests.snapshot_capture` gated, `.alpackages` symlink root only with trust, extension path comparison by component. Full gates: 94 suites, 4966 passed, 2 failed (formatter seed in flight, ghost diagnostics race found).
- Merged `campaign/slop-review-queue`: 112 review items closed (34 changed, 63 deferred behind live branches, 7 deferred, 3 accepted, 15 skipped as false positives). Strict 80.2 to 79.9 because the scan surface grew and the subjective dimensions are not re-scored until a fresh review. Architecture diagram now pinned to the manifests by a test (it caught the new harness dependency on al-protocol at once). Windows auth credentials unified across the three BC clients. Gates: 93 suites, 4873 passed, 1 failed (the formatter seed, fix in flight).
- Merged `campaign/fix-queued-2`: the seven queued items and the seven review A items. `test-run <id>` sibling calls work, MCP tool schemas derive from the daemon catalog, six workspace queries attribute per object in multi-object files, bare fields in a table's own procedure rename, the copilot scaffold template emitted fabricated API and now emits the documented one, `SymbolReference.json` carries `Variables`, ID 50000 accepted, tooltip prefix check is char-aware, `skipped` and `parseIssues` printed. Gates: 92 suites, 4858 passed, 1 failed (a new formatter idempotence seed, fix agent dispatched).
- Merged `campaign/fix-daemon-lifecycle`: build identity handshake replaces a daemon built from other code, idle exit (30 minutes, `AL_DAEMON_IDLE_SECS`), exit when the project root is gone, PATH daemon refused on version mismatch, `daemon-shutdown` waits, plugin SessionEnd hook, harness stops its daemons. Open: a wedged request can hold a daemon past its idle window (warns every 60 s). Full gates: 92 suites, 4839 passed, 0 failed.
- Merged `campaign/fix-r2-security`: 8 of 8 fixed. Project trust (`Docs/features/project-trust.md`, `al-explorer trust`), symlink-safe containment, one credential authorisation function, https required for credentials to non-loopback servers (`AL_ALLOW_INSECURE_BC_HTTP=1` overrides), NuGet feeds https only, daemon socket directory ownership check, checksum docs corrected. Full gates including zed-al: 92 suites, 4884 passed, 0 failed.
- Merged `campaign/slop-syntax-symbols`: `formatting.rs`, `symbols.rs`, `index.rs` and `oauth.rs` split into module directories (largest file now 796 lines), the duplicate data loader in al-symbols removed, `LineIndex` and `SourceLines` merged, one HTTP retry policy, one temp path helper, typed `ObjectKind` error. Gates: 2825 tests across al-syntax, al-symbols, al-analysis, al-lsp and the harness. desloppify refuses to rescan until its 112 item review queue is empty.
- Merged `campaign/fix-r1c-analysis`: 28 of 29 fixed plus the three items left by the second pass (type-position completion, the `attribute_list` dead branch, `EdgeKind::TriggerInvocation` removed). A node-kind guard test now fails if code names a syntax node the grammar lacks. Open then: the whole-workspace queries that attribute findings in a multi-object file to its first object. Six of them had it, fixed on `campaign/fix-queued-2` (ae5f69c3). Full gates: 91 suites, 4770 passed, 0 failed.
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
