# Ghost diagnostics after didClose, second round

Branch: `campaign/fix-ghost-race-2`, merged with `campaign/2026-09-21` at a8bb710e.

Tests: `no_ghost_diagnostics_after_close_during_debounce`
(`crates/al-test-harness/tests/regression.rs:192`) and
`test_completeness_d03_close_file_clears_diagnostics`
(`crates/al-test-harness/tests/completeness.rs:414`).

Line numbers below are for a8bb710e unless a commit is named.

## Where a publishDiagnostics for a document is sent

| Site | Currency check | Guard at the check | Guard at the send |
|---|---|---|---|
| `publish_if_current`, `crates/al-lsp/src/server/diagnostics.rs:484`. Phase 1 (:414) and phase 2 (:456) of `publish_diagnostics`, called from `did_open` (`lsp/mod.rs:1286`), `did_save` (:1525), `refresh_diagnostics_after_configuration` (:580), `al.lintFile` (`commands.rs:137`) and the compile result (`commands.rs:456`) | document open with the same text `Arc` and client version (`snapshot_is_current`, :519) | generation read (:492) | the same read guard (:497, released :499) |
| Debounced per-document task in `schedule_diagnostics`, `lsp/mod.rs:593` | `snapshot_is_current` (:696). An earlier unlocked check at :638 only skips work | generation read (:695) | the same read guard (:722, released :724) |
| Project pass `publish_workspace_diagnostics_parts`, `diagnostics.rs:262`. Called from the init task (`workspace/mod.rs:463`), the 5 s debounced pass (`lsp/mod.rs:736`, armed by `did_change` :1376 and `did_change_watched_files` :1194), `did_close` (:1509), `did_save` (:1538) and `refresh_diagnostics_after_configuration` (:583) | none per document. It compares `generation_revision` (:326) and recomputes when it moved, but on attempt 3 of `MAX_STAGING_ATTEMPTS` (:291) it publishes anyway | staging under one read guard (:296 to :320), released, then a second read guard (:325) | the second read guard: stale clears (:353), then every non-empty report in URI order (:360) |
| `did_close` clear, `lsp/mod.rs:1502` | none needed | first write guard (:1414) closes the document, aborts the debounced task and bumps the revision (`on_document_close`, :1440), then is released for the disk read (:1446) | second write guard (:1453), after the file index entry is restored from disk or removed (:1463 to :1495) |
| `did_close` of a non-file URI, `lsp/mod.rs:1513` | none | none | none |
| `did_open` rejection clear, `lsp/mod.rs:1273` | none, the document never opened | none | none |
| Configuration refresh clears, `lsp/mod.rs:561` | none, clears only | generation read (:553) | the same read guard |
| Compile results, `commands.rs:440` and the clear at :460 | none | none | none |
| Test results, `diagnostics.rs:881` and the clear at :899 | none | none | none |

Semantic diagnostics reach a client only through phase 2 of `publish_diagnostics` (guarded by
`publish_if_current`) or through the project pass, which merges a cached semantic result only
when its text and version match the open document (`diagnostics.rs:229` to :234). `did_close`
drops the cache entry under its first write guard.

The project pass is the only path that sends diagnostics for a URI without checking that URI.
Its reports cover every file in the file index, open or closed. For a closed file that exists
on disk that is correct in project scope: the pass reports the saved text. For a URI that is not
on disk, `did_close` removes the file from the index, so no current generation has diagnostics
for it.

## What the lock batch changed in these paths

The batch (b0748792, ddbcc0ad, 5cbec6f8, b868cf9a, merged at d34ad3d2) changed no currency check
and no guard around a send. In these paths it:

- moved the syntax pass into `syntax_diagnostics` (`diagnostics.rs:99`), which reads the project
  root before taking the config guard,
- released the bridge guard in `run_semantic_analysis` before the error-code reload,
- added comments at `lsp/mod.rs:553`, :737 and :1500.

`publish_workspace_diagnostics_parts` and the structure of `did_close` are unchanged.

## Counts

Each loop runs the single test 16 times with 8 extra `yes` processes on a 12-core machine. The
load average column is the range reported at the start of each run. Other agents were building
at the same time.

| Server | Test binary | Failed | Load average |
|---|---|---|---|
| a8bb710e, the campaign branch after the lock batch | same commit | 4 of 16 | 6.4 to 7.8 |
| 3a8e9eca, the parent of the lock batch merge d34ad3d2 | same commit | 3 of 16 | 8.0 to 13.0 |
| 52a62864, the 2026-09-24 ghost fix | a8bb710e | 5 of 16 | about 7 |
| a8bb710e, `test_completeness_d03_close_file_clears_diagnostics` | same commit | 3 of 16 | about 4 |

For the 52a62864 row only `al-lsp` was rebuilt. The test does not use `al-explorer`.

Every regression failure had the publish sequence clear, error, clear. The failure predates the
lock batch.

## Mechanism

The ghost is the init task's project pass, publishing a report staged before the close.

The harness returns from `LspClient::spawn` as soon as `workspace/symbol` answers. The init
task is then still running `initialize_workspace`, which ends with a project pass
(`workspace/mod.rs:463`, logged as "Published project-scoped diagnostics generation"). Its
staging overlaps the test's didOpen, didChange and didClose. The server trace of the first
failing run (`RUST_LOG=al_lsp=debug,tower_lsp=trace`, where `->` is a message written to the
client) reads:

```
10:59:48.568458 -> publishDiagnostics ghost_test.al [syntax error, end 4:7]   did_open, version 1
10:59:48.569068 <- didChange (version 2)
10:59:48.578289 <- didClose
10:59:48.590240 -> publishDiagnostics ghost_test.al []                         did_close clear
10:59:48.590543 -> publishDiagnostics DeeplyNestedActions.al ...               init pass, 9 files
10:59:48.592191    "Published project-scoped diagnostics generation"
10:59:48.592253 -> publishDiagnostics ghost_test.al [syntax error, end 4:8]   init pass, last in URI order
10:59:48.610911 -> publishDiagnostics ghost_test.al []                         did_close's own project pass
```

The error range ends at 4:8, which is the version 2 text, so the pass staged after the
didChange. All four failing head runs and all three failing pre-batch runs show the same
shape.

Why the guard does not stop it:

1. Each staging attempt holds a generation read guard (`diagnostics.rs:296`), then releases it
   (:320) before taking a second read guard to publish (:325). A queued writer runs in that
   gap.
2. didOpen, didChange and the first write section of didClose each bump `generation_revision`.
   Under load each one lands in a gap, so attempts 1 and 2 see the revision move and restage.
3. Attempt 3 stages while the file index still holds the unsaved overlay: after the didChange,
   or between the two write sections of `did_close`, which leaves the overlay in the index
   while it reads the disk without a guard (`lsp/mod.rs:1446` to :1453).
4. The second write section of `did_close` runs in the gap after attempt 3. It removes the file
   from the index, bumps the revision and sends the clear under the write guard.
5. Attempt 3 takes its publish guard, sees the revision moved, and publishes anyway because it
   is the last attempt (:326). `ghost_test.al` sorts after the capitalised fixture names, so its
   stale error is the last publish of the pass.
6. `did_close` then runs its own project pass (`lsp/mod.rs:1509`). The ghost URI is now in
   `workspace_diagnostic_uris` and not in the index, so that pass sends the trailing clear.

The read guard at the send only orders the send against writers. It does not make the reports
current, and the revision check that did was waived on the last attempt. That waiver came with
0255a549 (2026-09-21, "make the optimistic staging loops converge while the user types"),
whose message says the debounce armed by the last keystroke corrects a stale result. After a
close the next correction is `did_close`'s own pass, so the client shows the ghost in between.
Before 0255a549 the loop retried without limit and never published a superseded generation.

The completeness failure is the same path. In run 5 of the d03 loop the init pass published
`close_diag.al` with AL-NC001 ("Duplicate codeunit id 50100") 3 ms after the did_close clear.
The test read the latest publish of that batch, the error, and the process ended before
`did_close`'s own pass cleared it.

## Why 32 of 32 passed on 2026-09-24

At 52a62864 the test polled until it saw the clear and asserted only on later batches. A publish
that arrived after the clear in the same 25 ms drain batch was recorded but not asserted. The
ghost arrives 2 to 3 ms after the clear, in the same batch. f153fd1f ("keep watching after
diagnostic clear", merged at fcb3e3f1 on 2026-09-26, before the lock batch) made the test assert
on those publishes as well. The server at 52a62864 fails the current test 5 of 16 times.

## Is the test wrong

No. The harness reads the server's stdout in order, and the server's own trace shows the same
order on the wire: clear, error, clear. The ghost is real and short-lived. It stays on screen
until `did_close`'s own project pass clears it, 10 to 20 ms later in these runs.
