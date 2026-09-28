# Checkpoint: close of the campaign window, 2026-09-28

This adds to `CHECKPOINT-2026-09-27-close.md`. It records the last day of the window and where
everything stands for a session that resumes after the watchdog timer has stopped. `STATE.md`
holds the same facts in its working form and is the file to update from here on.

## Counts since the 2026-09-27 close (402d5c84)

| What | Count |
| --- | --- |
| Commits on `campaign/2026-09-21` on 2026-09-28 | 176, of which 28 merges |
| Files changed against `dev` (b749cfbe) | 126, 18,692 lines added, 8,651 removed |
| Findings across the 48 findings files | 476 fixed, 4 rejected, 16 open |
| Review rounds run on the day | rounds 12 (last items) and 13, security round 7 |
| Gates on 41ab5260 | fmt, clippy, clippy semantic, rustdoc green, 95 suites, 5,537 passed, 0 failed, harness 354 passed |

## State at the close

- Branch `campaign/2026-09-21` is pushed to origin at 219fccbb. Pull request 33 (into `dev`) is
  a draft. Its CI failed on ca181401 on ubuntu, macOS and Windows for one cause, a test that did
  not compile after two branches met (fixed by d82b8d01). CI on 219fccbb was started by the push
  at 23:57 BST.
- Submodule `tree-sitter-al` is at baf782b, the pointer `extension.toml` names.
- Nine agent worktrees under `../al.language.zed-worktrees/`, each on a branch of its own:
  seven fix agents and two reviewers dispatched by the 23:42 session, listed in `STATE.md`
  under "In flight". Each was told to commit after every verified step and to stop by 01:15 or
  01:30 BST. A branch with commits and no completion note in `LOG.md` means the agent or the
  session died: merge it after the gates pass on the merge.
- `campaign/fix-r13-router` (dc6ce66b) is merged locally at 81c47204 and not yet pushed: the
  gates on the merge had not run when this file was written.
- The watchdog timer stops starting sessions after 23:59 (`END_EPOCH` in
  `scripts/campaign/watchdog.sh`). Brad disables it: `systemctl --user disable --now
  al-campaign-watchdog.timer`.
- The blog series is on the blog branch `campaign/2026-09-rewrite` with article 9's re-read on
  `campaign/article-9-reread` (124e0bb). Merge to `main` is Brad's call.

## What 2026-09-28 delivered

- Round 12's last items (R12-DAEMON-1, R12-MUT-1) and the two file splits (`records.rs` into
  seven modules, `eval_expr.rs` into seven).
- Round 13 (`findings/r13-session-review.md`, 16 findings): the trust record hashes a probing
  path written without `./` and records the analyzer copy under an absolute path inside the
  project, nested lists compare and free without a native frame per level, a JSON value added to
  itself is copied, one byte budget over a test's collections, a cancelled test stops inside
  `List.Contains`, overloads chosen by enum type, typed lists from `Split`, `Names`, `Ordinals`,
  `Keys` and `Values`, `app.json` read again when it changes, the daemon's write arms resolve
  under the project root, every al-explorer text renderer escapes what it prints, and a codeunit
  member without parentheses routes as the call with them. 13 of 16 fixed and merged, 3 open.
- Security round 7 (`findings/r7-security.md`, 15 findings): the byte cap and memo on tree
  hashing, outside paths the repository writes hashed or recorded as absent, the per-test byte
  budget, the JSON depth cap, the `named_write` registry. 6 of 15 fixed and merged, 9 open with
  fix agents on them at the close.
- Formatter: four `case` layout fixes checked against Microsoft's formatter.
- Grammar: the query drift check (grammar baf782b, ten query tests over every corpus entry).
- Tests: `snapshot.rs` behind debugger and runner traits with 926 lines of unit tests.
- Docs: re-checks 4 and 5 against the day's merges.
- Blog: article 9's final re-read.

## Open items, with the file that holds each

- `findings/r7-security.md`: SEC7-6, SEC7-8 to SEC7-15 (fix agents in flight at the close, see
  `STATE.md`).
- `findings/r13-session-review.md`: R13-TRUST-3, R13-TEXT-1 (a fix agent in flight).
- Found by the R13-ROUTER-1 fix, not yet a finding: `classify_bare_member` still returns
  silently for a Page, Report, XmlPort, Query, JSON, Text, Dictionary, List or Enum receiver, so
  a bare member on one of those that names an unsupported method may stay local where the call
  with parentheses goes to live BC (`crates/al-test/src/router/ast.rs`). For round 14.
- Left by the SEC7 fixes for security round 8: `al.dotnetPath` and `binary.path` outside the
  project recorded as text, a record made before the SEC7-4 change going `Stale` once, the two
  `{e:?}` prints in `crates/al-explorer/src/lib.rs` and `tui.rs`.
- Left by the formatter fixes: an `else` after a `case ... end` with no `;` sits at the case's
  level, and a dangling `else` inside nested openers lands at the outermost opener's level.
- Older: `findings/async-locking.md` AL-D1, `findings/r2-review-a.md` (two rows about this
  record and the Windows `open_browser` quoting), `findings/r1-emit-bc-explorer.md` (the two BC
  publish endpoints, needs a live server), and the queued list in `STATE.md`.

## Resume steps for a later session

1. `Docs/campaign/README.md`, the resume protocol, as written. The watchdog is off, so the
   session is started by hand.
2. `git worktree list`, then for each worktree `git log --oneline campaign/2026-09-21..<branch>`
   and `git status --short`. Save uncommitted reviewer scratch tests as
   `.campaign/<round>-scratch-tests-<n>.patch` and discard them. Merge each fix branch, run the
   gates on the merge, push. Merge the review branches, mark statuses, push.
3. Write the two review rounds' fix agents for what they found, or queue the findings in
   `STATE.md` if the session is short.
4. Mark pull request 33 ready when CI is green on the head, and merge it into `dev` as PR 30 and
   PR 32 were.
5. Remove the worktrees and delete the merged branches, local and remote.

## Checks on the head this file describes

Gates on 41ab5260 (`.campaign/gates-41ab5260.log`, 23:47 to 23:54 BST): fmt, clippy, clippy
semantic and rustdoc clean, 95 workspace suites with 5,537 tests passed and 0 failed, the
harness 354 passed and 0 failed. The commits after it up to 219fccbb change campaign docs only.
