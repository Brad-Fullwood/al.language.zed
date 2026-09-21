# Round 2: regressions found after the round 1 merges

Failures seen running the whole `al-test-harness` suite on
`campaign/2026-09-21` after the six round 1 fix branches merged. Everything
here reproduces from a clean worktree with `al-explorer` and `al-lsp` built
into the worktree's own `target/debug` (both tests below fall back to an
`al-explorer` on `PATH` otherwise and then measure whatever is installed).

## [RESOLVED] `al-explorer parse` no longer reads a file outside the loaded project

- suite: `cargo test -p al-test-harness --test cli_smoke`
- test: `cli_commands_use_the_real_project_daemon`
- assertion: crates/al-test-harness/tests/cli_smoke.rs:225

```
`al-explorer parse ../syntax_errors/ErrorCases.al --json` JSON output missing "\"errors\":":
{
  "error": "path '.../crates/al-test-harness/data/syntax_errors/ErrorCases.al' is outside the project at '.../crates/al-test-harness/data/test_al_project' (code -32602)"
}
```

- suspected commit: 0949a7ae, `fix(lsp): contain every caller-supplied daemon
  path to the loaded project` (branch campaign/fix-r1-lsp-protocol). It routes
  `file_uri_from_params` through `containment::resolve_within_project`, so every
  daemon method that takes a path now rejects anything outside the project root,
  the package cache and the directories the `.app` packages came from. The CLI
  fixture `data/syntax_errors/ErrorCases.al` is a sibling of the fixture
  project, so `parse` now refuses it.
- what collided: two deliberate contracts. The security fix answers a real hole
  (`al_call` forwards any `method`/`params` pair, so `format` could rewrite any
  file the daemon's user can write). The CLI contract is the other one: a user
  who types `al-explorer parse <path>` named that path themselves.
- resolution: the boundary stays exactly as it is, because the same dispatchers
  answer MCP, where the caller may be an agent. What changed is who reads the
  file. A read-only single-file method now accepts `text` beside the path,
  analyses it and never opens the path, and drops the document again when the
  request is answered. `al-explorer` sees the dedicated `-32002` code, reads
  the file itself and asks again with the text, so a read works for any file
  the user can read. `text` is refused for a path inside the project, where the
  daemon's own copy is authoritative, and refused outright by `format`, `fix*`,
  `sortMembers`, `organizeFiles` and `rename`, which stay confined to the
  project and say so, naming its root.
- covered by: `cli_smoke::a_read_only_command_answers_for_a_file_outside_the_project`,
  `cli_smoke::formatting_a_file_outside_the_project_is_refused`, and four daemon
  tests in `crates/al-lsp/src/server/daemon/mod.rs`
  (`a_read_refuses_an_outside_path_it_was_given_no_text_for`,
  `a_read_answers_an_outside_path_from_the_supplied_text`,
  `supplied_text_is_refused_for_a_path_inside_the_project`,
  `a_write_refuses_supplied_text`).

## [RESOLVED] Member completion offered no Record method without a Microsoft toolchain

- job: CI (ubuntu-latest), run 35622921775
- suite: `cargo test -p al-test-harness --test real_world`
- test: `test_completion_after_dot`, `crates/al-test-harness/tests/real_world.rs:692`
- symptom: `member completion must offer the Record methods; "FindSet" missing`.
  The table's own fields came back, and not one platform method. Green on
  the development machine, red on a clean runner.
- root cause: `completion_items_for_receiver` read the Record methods from
  `Workspace::semantic_cache`. That cache is filled by
  `load_caches_from_disk` from `~/.cache/al-lsp/semantic/builtins-<toolchain
  version>.json`, which exists only after a Microsoft AL toolchain has been
  found and its CodeAnalysis metadata extracted through the `semantic`-feature
  CLR bridge. A default build has no bridge, so the cache file is the only
  source, and CI has neither the toolchain nor the file. The development
  machine has both, which is the whole difference. This is a product defect,
  not a test defect: every editor install without a Microsoft toolchain got an
  empty method list after `Customer.`.
- fix: `crates/al-analysis/src/resolution.rs` falls back to
  `al_syntax::language_data::record_methods()`, the checked-in catalog
  generated from the same `TableClass` metadata, when the cache holds nothing
  for a Record receiver. A loaded cache is still consulted first and keeps its
  signatures and documentation.
- reproduced on Linux by pointing `XDG_CACHE_HOME` at an empty directory:
  `builtin_methods=0, total=40` and the same panic before the change,
  `builtin_methods=81, total=121` and a pass after it.
- covered by `resolution::tests::record_completion_offers_platform_methods_with_an_empty_semantic_cache`
  and `resolution::tests::a_loaded_semantic_cache_supplies_the_record_method_signatures`.

## [RESOLVED] Analyzer version selection depended on where the project sat on disk

- job: CI (macos-latest), run 35622921775
- suite: `cargo test -p al-project`
- test: `analyzers::tests::project_package_discovery_selects_highest_numeric_version`,
  `crates/al-project/src/analyzers.rs:336`
- symptom: `1.9.0` selected over `1.10.0` on macOS, `1.10.0` on Linux.
- root cause: `version_key` returned the largest dotted number found anywhere
  in a candidate's path. macOS hands a test its temporary directory under
  `/private/var/folders/36/...`, so both candidates keyed on `36`, the
  comparison tied, and the `left.cmp(right)` tiebreak compared the paths as
  strings, where `1.10.0` sorts below `1.9.0`. Linux `/tmp/.tmpXXXXXX` holds no
  number, so the package's own version directory decided it there. Directory
  enumeration order was never involved: the candidate list is fully sorted.
  The same shape would hit a real user whose project sits under a numbered
  directory.
- fix: `compare_versioned_paths` now keys each path component separately and
  compares component by component. Candidates share their prefix, so the first
  difference is the package's version directory. A named directory ranks below
  any numbered release.
- covered by `analyzers::tests::a_numeric_directory_above_the_project_does_not_decide_the_version`
  (the macOS path shape, reproduced on any platform),
  `the_highest_version_wins_in_either_enumeration_order` and
  `a_numbered_release_outranks_a_named_directory`.
- swept the workspace for the same pattern. `dedup_package_versions`
  (`crates/al-project/src/project.rs`) keys on the file stem only,
  `search_dotnet_tool_store` and `toolchain_version_rank`
  (`crates/al-project/src/toolchain.rs`) key on one directory name and on the
  deepest version-shaped component, and `resolve_version`
  (`crates/al-symbols/src/nuget.rs`) parses version strings. None of them takes
  a maximum over a whole path.

## [RESOLVED] The Windows containment refusal printed a verbatim path

- job: Windows native build, run 35622921775
- suite: `cargo test -p al-test-harness --test cli_smoke`
- test: `formatting_a_file_outside_the_project_is_refused`,
  `crates/al-test-harness/tests/cli_smoke.rs:274`
- symptom: the refusal read `is outside the project at
  '\\?\D:\a\al.language.zed\...\test_al_project'`, and the assertion that it
  names the project failed against the plain `D:\a\...` form.
- root cause: containment canonicalises the requested path and the project
  root before comparing them, and Windows `canonicalize` returns the verbatim
  `\\?\` form. Comparing two verbatim paths is correct and the refusal itself
  was right. Printing one is not, because the caller never typed that path and
  cannot match a message against it.
- fix: `display_path` in `crates/al-lsp/src/server/daemon/containment.rs`
  strips the `\\?\` prefix for the message only, handling the drive form and
  `\\?\UNC\server\share`, and leaving `\\?\Volume{...}` alone because it has no
  plainer spelling. The comparison still runs on the canonical paths.
- covered by `containment::tests::a_verbatim_prefix_is_stripped_for_display`
  and `a_path_with_no_plainer_spelling_is_left_alone` (plain text, so they run
  everywhere) plus
  `the_displayed_root_matches_the_path_the_caller_would_type` behind
  `#[cfg(windows)]`.

## CI ran only as far as the first failing suite

Each of the three jobs stopped at its first failure, so everything after it
stayed unverified on that platform: on macOS that was almost the whole
workspace, since `al-project` sorts early. The `cargo test` steps in
`.github/workflows/ci.yml` now pass `--no-fail-fast`, so one failing crate
reports alongside the rest instead of hiding it.
