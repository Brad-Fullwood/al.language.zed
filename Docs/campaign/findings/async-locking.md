# Async locking and correctness lints (desloppify batch 10)

Branch: `campaign/slop-async-locking`. Scope is batch 10 in section 4 of `desloppify.md`, the
detectors `rust_async_locking` (36 open), `smells` (55 open), `rust_error_boundary` (13) and
`rust_unsafe_api` (2).

The desloppify state predates the moves of test modules and the file splits, so its line numbers are
stale. Every finding was located again in the current tree. A second pass, independent of the
tool, looked for each `.lock().await`, `.read().await`, `.write().await`, `get_or_init_bridge()`
and `await_ready()` guard in `al-lsp`, `al-dap`, `al-protocol`, `al-workspace`, `al-symbols` and
`al-analysis` whose scope reaches an `.await`, including guards that live on as temporaries in a
`match`, `if let` or `for` head. It found five problems the tool did not report (AL-2 to AL-5 and AL-D1). Line numbers below
are for the branch head.

A guard across an await was judged unsafe when something inside the awaited operation can wait
for the same lock, or for a lock that a holder of this one waits for, or when the awaited
operation can take seconds (a compile, a network call, a client send) and the guard blocks
unrelated work. Tokio's `RwLock` is fair: a reader queues behind a waiting writer. So a task that
reads the same lock twice deadlocks against a writer that queues between the two reads.

Counts: 1 fixed with a test, 6 fixed without a test, 1 deferred. Accepted: 50 guard sites in
production code, 11 in test code, and 5 detector false positives. Of the tool's 36 reports, 4
are fixed (AL-1 twice, AL-6, AL-7), 19 are accepted production sites, 10 are test code and 3 are
false positives. The other smells are at the end.

## Fixed

### [AL-1] Diagnostics syntax pass and configuration publish take config and project in opposite order
- where: `crates/al-lsp/src/server/diagnostics.rs:99` (`syntax_diagnostics`, shared by
  `compute_diagnostics` :75 and `publish_diagnostics` :371) against
  `crates/al-lsp/src/server/lsp/mod.rs:1653` (`did_change_configuration`)
- detector: `rust_async_locking` on `compute_diagnostics` and `publish_diagnostics`
- scenario: the syntax pass took `config.read()` and then waited for `project.read()`.
  `did_change_configuration`, when the symbol paths change, holds the generation write guard,
  takes `project.write()` and then waits for `config.write()`. A pull `textDocument/diagnostic`
  or a `didSave`/`didOpen` publish that interleaved with a settings change left the two waiting
  on each other for good. Every other request then queued behind the generation write guard,
  so the server stopped answering.
- fix: the project root is read and its guard released before the config guard is taken, in one
  helper both paths share. The publisher's lock order (project, then config) is now written at
  the site.
- test: `lsp::tests::lock_order_tests`, one test per path. Each holds the project write guard,
  runs the diagnostics path, and asks for the config write guard with a 5 s timeout. Both
  failed on the old code and pass on the new.
- status: fixed b0748792

### [AL-2] Semantic analysis reads the bridge lock twice from one task
- where: `crates/al-lsp/src/server/diagnostics.rs:620` (`run_semantic_analysis`)
- detector: none. Found by the independent pass.
- scenario: the bridge read guard from `get_or_init_bridge` stayed held after `analyze`
  returned, through `ensure_error_codes_loaded`, which calls `get_or_init_bridge` again and so
  reads `workspace.semantic` a second time. A restart (`restart_bridge_if_current` after a
  timeout on another file) or `shutdown_bridge` queued for the write lock in between blocks that
  second read, and the writer waits for the first read. Both tasks hang, the writer holds the
  lifecycle lock, and every later bridge request queues behind it. The error code catalog stays
  empty when its first load fails, so this path runs on every analysis after such a failure.
- fix: the guard is dropped as soon as `analyze` returns. Neither result branch uses the bridge.
  The `get_or_init_bridge` doc comment now says a caller must drop its guard before it
  re-enters the bridge from the same task.
- test: none. Reaching the analyze result needs a live CLR bridge.
- status: fixed ddbcc0ad

### [AL-3] Bridge guard held while a warning waits on the client
- where: `crates/al-lsp/src/server/lsp/mod.rs:335` (`ensure_builtins_loaded`), :379
  (`ensure_error_codes_loaded`)
- detector: none.
- scenario: on a bridge error both functions sent `window/showMessage` with the bridge read
  guard still held, so a restart or shutdown waited on the client transport.
- fix: the bridge result is taken out of the guard before the message is sent.
- status: fixed ddbcc0ad

### [AL-4] Shutdown awaits aborted tasks with their slot mutex held
- where: `crates/al-lsp/src/server/lsp/mod.rs:1171`, :1182, :1193 (`shutdown`)
- detector: none. The guards were temporaries in a `for` and two `if let` heads.
- scenario: none today, because no task takes its own slot lock. A task that did would wait for
  shutdown to release the slot while shutdown waited for the task to end.
- fix: each handle is taken out of its slot before it is awaited.
- status: fixed 5cbec6f8

### [AL-5] Config write guard held across an await in `initialize`
- where: `crates/al-lsp/src/server/lsp/mod.rs:885`, :894
- detector: none.
- scenario: `gate_repository_settings` was an `async fn` with no await in it, called with the
  config write guard held. Harmless, but it read as a guard across an await.
- fix: it is a plain `fn`.
- status: fixed 5cbec6f8

### [AL-6] DAP proxy holds the stdout lock through a compile
- where: `crates/al-lsp/src/server/dap_mode/mod.rs:234` (`run_dap_proxy`), :299
  (`patch_outgoing`), :374 (`send_output_event`)
- detector: `rust_async_locking` on `run_dap_proxy`
- scenario: the legacy EditorServices proxy locked the shared stdout writer before
  `patch_outgoing` and kept it for the whole call. For `launch` that call runs the `alc`
  compile, so `child_to_stdout` could not forward any EditorServices frame to the editor until
  the compile finished.
- fix: `send_output_event` takes the lock for one frame. The comment on the writer already said
  that was the rule.
- test: none. The delay needs a real toolchain and compile.
- status: fixed b868cf9a

### [AL-7] `expecting_step` read and reset under two lock acquisitions
- where: `crates/al-dap/src/dap/bc_debug/session/mod.rs:508` (`convert_event`)
- detector: `rust_async_locking` on `convert_event`. No guard crossed an await: the tool
  matched the two separate acquisitions.
- scenario: the event forwarder converts Breaks while DAP requests run. A step request that set
  the flag between the read and the reset had it cleared by the older Break, and the step's own
  Break was then reported as a breakpoint.
- fix: one guard covers the read and the reset.
- test: none. The window lies between two acquisitions with no await point a test can stop at.
- note: `session/commands.rs`, `connect.rs`, `invoke.rs` and `test_support.rs` are left over
  from the session split in 354ec882, which a later merge undid. `mod.rs` does not declare them,
  so they are not compiled, and `invoke.rs` still has the old `convert_event`. If the split is
  restored, this fix has to be carried into it.
- status: fixed 91b899dc

## Deferred

### [AL-D1] `did_change_watched_files` reads files from disk under the generation write guard
- where: `crates/al-lsp/src/server/lsp/mod.rs:1111`
- detector: none.
- scenario: a burst of `didChangeWatchedFiles` events (a branch switch) is read on the blocking
  pool while the generation write guard is held, so every request and edit waits for the reads.
  On a slow or network file system that wait can be long. `did_close` releases the same guard
  for its disk read for exactly this reason.
- why deferred: the guard is there on purpose, so that two notifications for one file apply in
  order (the comment at the site says so). Moving the reads out needs a second lock that orders
  those notifications, and a test needs a way to slow a file read that the code does not
  have. Each read is bounded: regular files only, at most `MAX_AL_FILE_BYTES`.
- status: open

## Accepted: guards held across an await

Each row names the guard, what it is held across, and why that neither deadlocks nor stalls
other work. "In order" means the inner lock is only taken after the outer one, and not the
other way round. Rows marked "comment added" got a line at the site in 0490e4a4.

### Generation lock (`Workspace::generation_lock`)

The lock order across the workspace is generation, then project, then config, then the
server's own mutexes (`semantic_diagnostic_cache`, `workspace_diagnostic_uris`, `diag_tasks`).
After AL-1 no path takes them in another order.

| where | held across | why it is safe |
|---|---|---|
| `crates/al-lsp/src/server/diagnostics.rs:492` `publish_if_current` | `client.publish_diagnostics` | By design (LOG 2026-09-24 06:30): read guard from the currency check through the send, so a `didClose` clear cannot land between them. |
| `crates/al-lsp/src/server/lsp/mod.rs:695` debounced diagnostics task | `workspace_diagnostic_uris` lock, publish | Same design as `publish_if_current`. Existing comment. |
| `crates/al-lsp/src/server/lsp/mod.rs:553` `refresh_diagnostics_after_configuration` | uris lock, clearing publishes, `config.read()` | Same design, in lock order. Comment added. |
| `crates/al-lsp/src/server/diagnostics.rs:296` `publish_workspace_diagnostics_parts` | syntax pass over the whole workspace on the blocking pool, error message | Read guard for one coherent snapshot. The long existing comment explains the choice. Released before the publish phase takes it again. |
| `crates/al-lsp/src/server/diagnostics.rs:336` same function, `published` | publishes | Uris lock under the generation read guard, in order. |
| `crates/al-lsp/src/server/lsp/mod.rs:1257` `did_change` | cache lock, reindex on the blocking pool, `config.read()` | Write guard keeps the document store and file index in step. The reindex is CPU only. Existing comment. |
| `crates/al-lsp/src/server/lsp/mod.rs:1209` `did_open` | cache lock | In order, short. |
| `crates/al-lsp/src/server/lsp/mod.rs:1370`, :1409 `did_close` | cache and `diag_tasks` locks, `config.read()`, clearing publish | In order. The clear is sent under the write guard so no `publish_if_current` lands after it. Released for the disk read. Comment added. |
| `crates/al-lsp/src/server/lsp/mod.rs:1554`, :1568, :1577, :1642, :1653 `did_change_configuration` | project, config and cache locks | In order. Released for the package staging. Lock order comment added in b0748792. |
| `crates/al-lsp/src/server/workspace/mod.rs:528` `publish_complete_generation` | `project.write()` | In order. Existing comment explains why the project guard is taken before the first swap. |
| `crates/al-lsp/src/server/workspace/mod.rs:560` `refresh_current_symbol_generation` | project and config reads (temporaries), then `project.write()` | In order. Released for the staging. |
| `crates/al-lsp/src/server/workspace/mod.rs:1168` `download_symbols_command` | `project.read()` temporary | In order. Released before the downloads. |
| `crates/al-lsp/src/server/lsp/mod.rs:499` `runnables`, :1850 `formatting`, :1870 `range_formatting` | a project or config read (temporary) | In order, short. |

### Semantic bridge (`Workspace::semantic`, `semantic_lifecycle_lock`)

| where | held across | why it is safe |
|---|---|---|
| `crates/al-workspace/src/semantic_lifecycle.rs:195` `get_or_init_bridge`, :282 `restart_bridge_inner`, :383 `shutdown_bridge` | CLR init on the blocking pool, bridge writes | The lifecycle mutex makes concurrent callers start one bridge. No path takes it while holding a bridge read guard: every caller drops its guard before `restart_bridge*` (after AL-2). Comment added. |
| `crates/al-workspace/src/semantic_lifecycle.rs:81`, :108 `ensure_*_loaded` | the bridge call | The read guard keeps the bridge alive for its own call. Nothing inside re-enters the bridge. |
| `crates/al-analysis/src/queries/hover.rs:371` `hover_full`, `crates/al-analysis/src/queries/completions.rs:242` `completions_full` | config and project reads, the bridge call | Same. Neither writer (`did_change_configuration`, publication) waits on the bridge, so there is no cycle. Both drop the guard before a restart. |
| `crates/al-lsp/src/server/diagnostics.rs:602` `run_semantic_analysis` | the analyze call | After AL-2, only that call. |

### Locks that serialise one operation

| where | held across | why it is safe |
|---|---|---|
| `crates/al-symbols/src/bc_server.rs:157` `download_one` | the package download | One mutex per package, so a second caller waits and reuses the file. The download has a timeout. Comment added. |
| `crates/al-symbols/src/bc_server.rs:405` `add_auth` | interactive OAuth sign-in | Deliberate: concurrent callers wait for one sign-in. Existing comment. Nothing in `acquire_token` touches `cached_token`. |
| `crates/al-symbols/src/nuget.rs:293` `download` | the package download | One mutex per package. Existing comment. |
| `crates/al-lsp/src/server/daemon/mod.rs:825` `refresh_workspace_files` | metadata walk on the blocking pool | A static mutex that makes refreshes run one at a time. The doc comment says so, and nothing in the walk takes it. |
| `crates/al-workspace/src/test_results.rs:48`, :51 `append` | local file reads, rewrite, append | Serialises writers of one file. `bucket_counts` is only taken inside `write_lock`. Comment added. |

### Writers and channels

| where | held across | why it is safe |
|---|---|---|
| `crates/al-lsp/src/server/mcp/mod.rs:1642` `write_mcp_frame` | one frame's write and flush | Must be held for the frame, or responses interleave bytes. Comment added. |
| `crates/al-lsp/src/server/dap_mode/mod.rs:265` `child_to_stdout`, :374 `send_output_event` | one frame's write | Same, after AL-6. |
| `crates/al-dap/src/dap/bc_debug/session/mod.rs:578` `wait_for_break_event` | `recv()` on the break channel | The mutex only gives `&self` access to the receiver, and nothing else locks it. Comment added. |
| `crates/al-dap/src/dap/bc_debug/session/mod.rs:411` `invoke_with_timeout` | send and receive of one invocation | Serialises request/response pairing. Every wait is bounded by the deadline taken before the lock. Not reported by the tool. |
| `crates/al-dap/src/dap/bc_debug/session/mod.rs:552` `try_drain_push_events` | callback handling | `try_lock`, so it never waits. The callbacks take other locks. |

### Debug session (`Workspace::debug_session`)

| where | held across | why it is safe |
|---|---|---|
| `crates/al-lsp/src/server/daemon/debug_dispatch.rs:551`, :586, :607, :643, :692, :738, :769, :806, :858 (the nine `debug_*` commands, reported as `dispatch_debug`) | one BC debugger call each | `NativeDebugSession` methods take `&mut self`, so the mutex owns the session. Each call ends at the SignalR invoke timeout, and nothing it awaits takes `debug_session`. `continue_exec` returns once BC accepts it and does not wait for the next break. The idle reaper uses `try_lock` on purpose (comment at `daemon/mod.rs:289`). Field doc added. |
| `crates/al-dap/src/dap/native_dap/breakpoints.rs:125` `apply_breakpoints_to_session` | BC remove and add calls | Deliberate: makes remove, add and store one step per source. Existing comment. Each call is bounded by the invoke timeout. |

### Detector false positives

| where | why |
|---|---|
| `crates/al-lsp/src/server/lsp/mod.rs:609` `schedule_diagnostics`, :738 `schedule_workspace_diagnostics` | The awaits are inside the spawned task, not under the slot guard. Comment added to the second. The first already explains why the slot is held from abort to store. |
| `crates/al-lsp/src/server/commands.rs:448` `publish_compile_result` | `drop(last)` comes before the first await. |
| `crates/al-lsp/src/server/lsp/mod.rs:1500` `did_change_configuration` | The guard exists only in the `Ok` arm, which drops it at once. |
| `crates/al-lsp/src/server/daemon/build_dispatch/tests_dispatch/tests.rs:898` `ws_with_project` | Dropped before any await. |

### Test code (holding the guard is the point of the test)

- `crates/al-lsp/src/server/lsp/tests.rs:83`, :133 (single task, no contention), :540
  (`closing_a_document_cancels_only_its_own_task`), :1494
  (`project_diagnostics_publish_while_the_workspace_keeps_changing`), :1648 and :1682 (the AL-1
  tests)
- `crates/al-lsp/src/server/daemon/tests.rs:153`, :1251
- `crates/al-lsp/src/server/workspace/tests.rs:1229`
- `crates/al-dap/src/dap/bc_debug/session/tests.rs:857`
- `crates/al-symbols/src/nuget.rs:1539`

## Other batch 10 smells

- Unsafe without a SAFETY comment: every `unsafe` block and `unsafe impl` in `crates/` now has
  one in front of it (0490e4a4). New comments: seven blocks in the `AL_COMPILE_TIMEOUT_SECS`
  tests in `al-compile/src/lib.rs`, `PeekNamedPipe` in `al-protocol/src/client/mod.rs:203`, two
  `atexit` registrations in `al-test-harness/src/daemon_reaper.rs`, two environment variable
  blocks in `al-workspace/src/test_results.rs`, and `unsafe impl Sync` in
  `al-semantic/src/host.rs`. The SIGPIPE comment in `al-explorer/src/main.rs` moved from inside
  the block to before it.
- `rust_unsafe_api`, `crates/al-semantic/src/host.rs:151` and :275 (`slice::from_raw_parts`):
  accepted. The first checks non-null and a length in `1..=1 MiB`, the second non-null,
  non-negative and at most `MAX_RESPONSE_BYTES`, and each carries a SAFETY comment. Both CLR
  buffers are freed on every path, the second through the `ClrBuf` drop guard.
- Blocking `thread::sleep` (20 sites): none is in async code. `al-protocol`'s client is fully
  synchronous (no async fn, no tokio). `al-explorer/src/app/mod.rs:143` runs on a `std::thread`
  worker. The rest are sync tests. Accepted.
- `mem::forget` (4) and `Box::leak` (1): all in test code, keeping a TempDir or a channel
  receiver alive for the test. Accepted.
- `std::process::exit` (21 calls): all in `crates/al-lsp/src/bin/al-lsp.rs`, the binary entry.
  Each reports a startup or run failure with an exit code, or ends the process on a signal or
  parent death. No library crate calls it. Accepted.
- `#[allow]` in production (42): each was removed and clippy rerun. 15 suppressed nothing and
  are gone (fedf0b23). The 27 that still suppress a warning stay, each with a reason comment
  (11 added).
- `Result<_, String>` (40 in 12 `al-analysis` files): none converted. The three modules with an
  error enum do not fit. `bulk_fix.rs` helpers return a message that `build_plan` prefixes with
  the file path and wraps as `BulkFixError::Refused`, so the public API is already typed.
  `audit.rs:850` turns the message into an audit issue, not an error. `source.rs:900`
  (`event_source`) fails in ways `SourceLookupError` has no variant for. The rest have no error
  type in their module. Left for the typed error work across crates (theme 3 in `desloppify.md`).
- `Result<_, ()>` (3): the two in `bc_debug/session/mod.rs` belong to the `#[cfg(test)]`
  `TestSignalRTx`. The one in `lsp/mod.rs` is the `()` inside `RwLockReadGuard<'_, ()>`, a
  false positive. Accepted.
- `rust_error_boundary` (13): 10 in `al-test-harness` (test zone), one in
  `al-runtime/src/test_support.rs` (`#[cfg(test)]`), and `run_daemon`/`run_mcp`, whose
  `Box<dyn Error>` goes straight to the binary, which logs it and exits 1. Accepted.
- Public glob re-exports (13 in 5 files): API hygiene, not a correctness lint. Left for batch 11.
