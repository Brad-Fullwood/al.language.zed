# Checkpoint: campaign paused on 2026-09-27

Brad paused the campaign at 22:41 BST on 2026-09-27 to work on something else. This file records
the state at the pause. `.campaign/STOP` is in place, so the watchdog starts no headless session
until it is removed.

## Where the work is

- Code: every change the campaign made is on `dev`. Pull request 32 (`campaign/2026-09-21` into
  `dev`) was merged after CI passed on the checkpoint tip. The first CI run failed one macOS test, whose assertion read a display line that a long macOS temp path pushed past its 120 character cap. The test now reads the recorded value. `campaign/2026-09-21` stays on origin as the
  resume branch and equals `dev`. The other 27 remote campaign branches and 80 local branches
  (agent branches and worktree placeholders) were merged into it already and are deleted.
- Grammar (`tree-sitter-al`, the AL-Tree-Sitter repository): its `campaign/2026-09-21` branch is
  merged into `dev` (82e89f8) and pushed. `dev` also carries pull request 3
  (`al-object-name-field`), which the campaign branch did not have. The superproject pointer stays
  at 142aba6, an ancestor of that merge, so `extension.toml` and the gitlink still agree and the
  release hygiene check passes. Moving the pointer to 82e89f8 is a follow-up that needs the full
  gates, since the parser source is part of the workspace build. The four grammar branches on
  origin other than `dev` were merged and are deleted.
- Blog (technically-business-central): `campaign/2026-09-rewrite` was fast-forwarded into `main`
  (5af7849) and pushed, and the branch is deleted. All nine articles carry `draft: true`. The blog
  index, the article pages, the RSS feed and the tag pages leave drafts out of production builds,
  so nothing is published. Publishing an article is Brad's decision: remove `draft: true`.

## The last session, 2026-09-27 22:41 BST

Six agents from the 17:50 headless session had died with work in their worktrees.

- `campaign/fix-r6sec-rt` (SEC6-3, SEC6-4, SEC6-5): three commits, and al-runtime and al-test
  pass on them (658 and 179 tests). Its uncommitted scratch tests, which build a 300,000 element
  chain, overflow the stack in the test binary. Saved as `.campaign/r6rt-scratch-tests.patch` and left
  out of the branch. Merged a5be8edb.
- `campaign/fix-daemon-appjson`: the whole fix was uncommitted (408 lines in 8 files). The daemon
  hashes `app.json`, `.zed/debug.json` and `.vscode/launch.json` before each request and reads
  them again when a hash moved. al-project, al-workspace and the daemon tests pass. Committed
  5ab31df4, merged 18a1905f.
- `campaign/fix-queued-3`: one commit (the project pass clear skips a document that changed after
  staging) plus the uncommitted fix for the doubled "not trusted" notice from
  `pack-native --validate`, with its test. Committed ea69d4ed, merged 0f19a5a7.
- `campaign/r12-review`: six documentation commits. The review is not complete: coverage item 1a
  is unticked and its ten findings are open. Its 860 lines of scratch tests are saved as
  `.campaign/r12-scratch-tests.patch`. Merged 3c303a94.
- `campaign/test-mutants`: no commits. The working tree held a half-applied mutant of `indent.rs`
  and a moved submodule pointer, both discarded.
- `campaign/fix-r6sec-b`: no commits. SEC6-6, SEC6-7 and SEC6-8 stay open.

Gates on a075b62c: fmt, release build, clippy, clippy with the semantic feature and rustdoc clean.
Workspace tests: 96 suites, 5417 passed, 0 failed, 10 ignored.

## Numbers at the pause

| Measure | Value |
| --- | --- |
| Commits on `campaign/2026-09-21` since the `dev` base of pull request 32 | 552 |
| Files changed against that base | 265 (80,845 insertions, 32,485 deletions) |
| Review rounds | 12 session reviews, 6 security rounds, 6 plugin rounds, 3 grammar corpus rounds, 1 dogfood sweep, 1 audit triage of 253 rows |
| Findings with a status | 439 fixed, 4 rejected, 15 open |
| Workspace tests | 96 suites, 5417 passed, 0 failed, 10 ignored |
| Baseline on 2026-09-21 | 80 suites, 4380 tests |

## Open findings, 15

- Round 12, `findings/r12-session-review.md`, ten findings: R12-MERGE-1 (merge 19a7cae9 dropped
  the docs re-check corrections in `native-test-runtime.md` and one comment rewrite), R12-LIST-1
  and R12-LIST-2 (List element and search argument types, untyped `Keys()` and `Values()`),
  R12-RT-1 (overloads that differ only by record subtype), R12-RT-2 (`var` write back reads the
  array index after the call), R12-KW-1 (a record method without parentheses routes locally),
  R12-GR-1 (the grammar refuses a quoted `for` variable), R12-MUT-1 (formatter tests that pin
  text that is not AL), R12-DAEMON-1 (`.alpackages` read once), R12-TEXT-1 (writing rules).
- Security round 6, `findings/r6-security.md`, three findings: SEC6-6 (ten dispatchers read a
  caller path while the registry says they take none), SEC6-7 (skills quote an object name into a
  single-quoted shell argument), SEC6-8 (control characters in text output reach the terminal).
- AL-D1, `findings/async-locking.md`: `did_change_watched_files` reads files under the generation
  write guard on purpose, to keep notifications in order. Deferred.
- `findings/r1-emit-bc-explorer.md`: al-dap and al-publish post to different BC development
  endpoints. Needs a live server to settle.

The queued list in `STATE.md` has the rest: the desloppify deferred items and the `records.rs`
and `eval_expr.rs` splits, `cargo mutants` on the remaining files, the Windows named pipe owner
check, `DotNetPackages` in `SymbolReference.json`, a release with `binary-checksums.txt`, and the
notes the round 10 and round 6 agents left.

## Scratch patches

`.campaign/*.patch` holds 15 git-ignored patches of scratch tests that review agents wrote to
probe a finding. Each becomes a real test when the finding it probes is fixed, or is dropped.

## How to resume

1. `rm .campaign/STOP`. The watchdog timer is still active and starts a headless session once
   `.campaign/heartbeat` is 30 minutes old. An interactive session resumes with `/loop` on this
   branch.
2. Read `STATE.md`. The first units are the round 12 findings and SEC6-6 to SEC6-8 to fix
   agents, coverage item 1a of the round 12 review, and the grammar pointer move to 82e89f8 with
   the full gates. `HANDOFF-tree-sitter.md` is the brief for a grammar agent (R12-GR-1 and the
   pointer move), and `CHECKPOINT-2026-09-27-close.md` records the interactive runtime
   session's close-out.
3. The campaign window ends on 2026-09-28. After that, run
   `systemctl --user disable --now al-campaign-watchdog.timer`.
