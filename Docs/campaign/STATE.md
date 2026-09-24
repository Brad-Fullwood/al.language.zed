# Campaign state

Updated: 2026-09-24 08:00 BST. Branch: `campaign/2026-09-21`. Ends: 2026-09-28.

## Phase

Round 2. Every round 1 finding is fixed, rejected with evidence, or queued. Two adversarial reviewers read the whole campaign diff (`git diff dev..campaign/2026-09-21`, 277 files) looking for fixes that do not fix, merge damage and regressions. A fix agent works the queued items.

## In flight

Nothing. The three unmerged fix branches (`fix-formatter-idempotence`, `fix-ghost-diagnostics`,
`slop-splits-2`) are merged. `campaign/ai-persisted-index` never reached origin, so the
persisted index is queued again below.

Remote branch cleanup: every `campaign/*` branch except this one is merged into it. Cloud
sessions can push only to `campaign/2026-09-21`, so deleting them is left to Brad (the list is in
`LOG.md`, 2026-09-24).

Draft PR: https://github.com/Brad-Fullwood/al.language.zed/pull/30 (base `dev`, CI runs on every push).

Queued:

- CI on PR 30: ubuntu green. macOS tests and the Windows extension-integration step still fail; a triage agent is reading the logs.
- `obsolete` lists every pending obsoletion in the loaded packages (1553 and 220 KB on Base Application 26). Add a mode that reports the obsolete package symbols the workspace code uses; `obsolete_usages` exists but is only reachable as a lint rule.
- `symbols` prints JSON without `--json`.
- The LSP server does not watch the disk either: a file changed outside the editor (git checkout, a generator) stays stale until reopened. Register `workspace/didChangeWatchedFiles` and route it through `al_workspace::refresh_workspace_files`, skipping open documents.
- Four copies of `is_workspace_package` (al-symbols, al-lsp `daemon/scope.rs`, al-explorer `app/details.rs`, al-analysis `source.rs`).
- Persisted symbol and source index on disk (cold start 54 s and 2.9 GB RSS), keyed by app id, version and content hash.
- Remaining file splits from the slop-splits-2 list: `session.rs`, `xliff.rs`, `calls.rs`, `router.rs`.
- Rebuild `target/release` before measuring for articles (it predates `publish` and `free-ids`).
- `dispatch_generate` cannot warn on IDs outside the project `idRanges` until the dispatcher is async.



- al-dap and al-publish post to different BC dev endpoints (needs a live server to settle). The duplicated response validation framework in al-explorer (about 300 lines to move).
- desloppify fix batches (`findings/desloppify.md` section 4), file splits after the owning fix branch merges.
- Workstream E (tests): coverage by crate, property tests for parser and interpreter, `cargo mutants` on al-runtime and al-analysis. Start when a build slot frees.
- Workstream D (security): dedicated review after round 1 fixes are in, covering what changed.
- Blog: article 1 repeats a wrong diagnosis of the `trace` timeout (it is the cold call-graph build, 86 s with Base Application). Rewrite that paragraph. Articles 2, 4, 5, 8 after the daemon work and fix branches merge, then article 9, then the fact pass list in `findings/blog-progress.md`.
- AI tooling build item 9: dependency package version diff.
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
| C | Slop and simplification: desloppify plan, per-crate simplify pass | Batch 3 (four file splits) and the 112 item review queue merged. Strict 79.9 | Fresh `desloppify review` to re-score, then the 63 deferred items (typed RPC boundary is the largest), batches 10 (async locking) and 11 (docs and API hygiene), remaining file splits (`resolution.rs` 2754 lines, `dispatch.rs` 3243, `tests_dispatch.rs` 4216, `lsp.rs` 3415, `native_dap.rs` 3636) |
| D | Security: credentials, archive parsing, MCP and daemon input, extension binary download, supply chain | Three review rounds (8, 19, 10 findings), all fixed and merged. Project trust, dispatcher capability registry, peer-checked endpoint | Windows named pipe owner check. A fourth round late in the week over the final diff |
| E | Tests: coverage by crate, property tests, `cargo mutants` | First pass merged: 4 bugs found by property tests, coverage table, CI job proposal | Add the property test CI job, run `cargo mutants` on the 10 file shortlist in `findings/test-depth.md`, make `al-test/backends/snapshot.rs` testable |
| F | Grammar: corpus tests, query drift between `languages/al` and `tree-sitter-al/queries` | R1 review running | From R1 findings |
| G | AI tooling: make this project speed up and sharpen AI work on Business Central (see below) | Inventory, measurements and design done (`findings/ai-tooling-ideas.md`): latency is 4 to 150 ms warm, but 14 of 20 measured answers are too large for an agent (up to 9.4 MB). Plugin build running | Daemon projection work after the LSP fix branch merges |
| H | Docs: `Docs/`, `README.md`, `ROADMAP.md` match the code, then unsloppify | R1 docs review running | From R1 findings |
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
