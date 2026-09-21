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
