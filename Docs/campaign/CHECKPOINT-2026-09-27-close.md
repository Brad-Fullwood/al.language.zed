# Checkpoint: close-out of the runtime session, 2026-09-27

This adds to `CHECKPOINT-2026-09-27.md`, which records the campaign as a whole at the pause. It
covers the long-running interactive session that worked the dogfood findings, the native
compiler fixes and the local interpreter (144 commits carry its `Claude-Session` trailer:
126 on 2026-09-24, 14 on 2026-09-26, 4 on 2026-09-27), and what it did at the pause.

## State at the pause

- Branches: `campaign/2026-09-21` is the only local branch. No stash, no other worktree. Origin
  holds `campaign/2026-09-21` and `dev` only. The 27 other campaign branches were merged into
  the campaign branch (`git rev-list --count HEAD..<branch>` was 0 for each) and deleted, and
  `git fetch --prune` dropped the stale refs here.
- Submodule: `tree-sitter-al` is checked out at the pinned 142aba6. It had been left at cc31863
  in this checkout, and `git submodule update` put it back.
- Pull request 32 (`campaign/2026-09-21` into `dev`): CI failed on macOS on 59c598de, the commit
  that added the first checkpoint, so the pull request was not merged then. Fixed by 3c831399
  below. It merges once CI passes on the commit that adds this file.

## CI fixes made at the pause

| Commit | Check | Cause |
| --- | --- | --- |
| 8cf97b56 | Windows native build, CI (macos-latest) | `an_untrusted_project_s_refusal_names_the_real_folder` compared the refusal with the canonical project path. The refusal names the folder as given, which differs from the canonical path on Windows (8.3 short names, `RUNNER~1`) and on macOS (`/var` is `/private/var`). |
| 3c831399 | CI (macos-latest) | `a_path_through_a_link_out_of_the_project_is_recorded_where_it_resolves` looked for `sha256:` in `display_line()`, which cuts a value at 120 characters. The macOS temporary path is long enough that the cut falls inside `sha256`. The test reads `setting.value` now. This session reproduced it with `TMPDIR` set to a path of the macOS length and made the same change, and the other session pushed it first, so that commit stands. |

The macOS log is too long for the GitHub MCP log tool, which returns only its tail. The raw log
downloads without authentication: `curl -sSL
https://api.github.com/repos/Brad-Fullwood/al.language.zed/actions/jobs/<job id>/logs`.

## What the session delivered

- Round 5 dogfood (`findings/r5-dogfood.md`) and the round 6 review: the CLI, impact, obsolete,
  xlf, projection, completion, hover, rename and search fixes, the r6 low findings, and the test
  module moves.
- Native compile: named return values, parameters and chained calls, temporary records, and a
  missing report layout reported on the report.
- Every object of a multi-object file in the call graph, the symbol index and the other
  whole-workspace queries.
- The local interpreter: chained calls, arrays and text indexing, Format pictures, CalcSums,
  `Record.Ascending`, enums, TextBuilder, Guids, Rename and TestField, JSON types, table code with
  its triggers and `Validate`, event subscribers (including those bound by object ID), codeunit
  variables with their own globals, and on 2026-09-27 List and Dictionary as references with
  `AddRange`, `GetRange`, `RemoveRange`, `Reverse` and `LastIndexOf` (b2577ceb). The router sends
  `GetRange(i, n, Target)`, the var-result form, to live BC.
- The grammar defect that hid an attribute after a var section was diagnosed here
  (`findings/grammar-attribute-after-var.md`) and fixed in the grammar (GR2-1).

## Open items this session's work touches

- R12-LIST-1 (medium) and R12-LIST-2 (low), `findings/r12-session-review.md`: List element and
  search arguments are not converted to the declared type, so `List of [Code[20]]` misses a
  lower-case search, and `Values()` returns a list with no element type. The fixes are written
  in the findings file.
- The dotnet host advisory cuts its reason at the display limit. With a project path about four
  characters longer than the macOS temporary path, `a_project_dotnet_is_dropped_before_a_spawn_when_a_settings_file_stops_parsing`
  and `an_untrusted_project_dotnet_is_dropped_when_its_settings_do_not_parse` fail: the reason
  ends at `.vscode/settings.js…`, so a user with a deep project path is not told which file
  failed to parse. Reproduce with `TMPDIR=/tmp/private/var/folders/36/tjdph2t965j8snz9_vkdnw0r0000gn/T
  cargo test -p al-project --lib dotnet_is_dropped`. The fix is to name the file before the
  path-bearing error text, or to cut the path rather than the message. Not changed at the pause.
- `a_failed_file_write_leaves_the_whole_workspace_unchanged` (al-explorer) fails when the tests
  run as root, which ignores the read-only directory the test makes. CI runs as a normal user and
  passes. A cloud container runs as root, so expect this one failure there.

## Grammar

The grammar changes the campaign asked for, with the steps to land them, are in
`HANDOFF-tree-sitter.md`: R12-GR-1 (a quoted `for` loop variable), a field for each call
argument, and the query drift check over the round 3 node shapes.

## Checks on the commit that adds this file

`cargo fmt --all -- --check` and `cargo clippy -p al-project --all-targets -- -D warnings`
clean. al-project lib tests: 274 passed with the default temporary directory, and the formerly
failing test passes with the macOS-length one. The full workspace ran on b2577ceb: 5303 passed,
1 failed (the root-only test above).
