# Round 2: regressions found after the round 1 merges

Failures seen running the whole `al-test-harness` suite on
`campaign/2026-09-21` after the six round 1 fix branches merged. Everything
here reproduces from a clean worktree with `al-explorer` and `al-lsp` built
into the worktree's own `target/debug` (both tests below fall back to an
`al-explorer` on `PATH` otherwise and then measure whatever is installed).

## [OPEN] `al-explorer parse` no longer reads a file outside the loaded project

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
- why it is still open: two deliberate contracts collide. The security fix
  answers a real hole (`al_call` forwards any `method`/`params` pair, so
  `format` could rewrite any file the daemon's user can write), and the audit
  entry it closes recommended containment on the write paths at minimum. The
  CLI contract is the other one: a user who types
  `al-explorer parse <path>` named that path themselves, and `parse` returns
  only counts, timings and parse-error positions, no file text.
- the decision to make: whether the daemon boundary applies to read-only
  methods at all, and if not, how `ensure_document` avoids leaving a
  non-project file's text in the document store where `symbols` and `source`
  can reach it. A narrower alternative is a local parse in `al-explorer`
  (it has no `al-syntax` dependency today) for a path the daemon refuses.
