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
- [ ] crates/al-lsp/src/lib.rs + toolchain.rs
- [ ] crates/al-lsp/src/bin/al-lsp.rs
- [ ] crates/al-lsp/src/server/mod.rs
- [x] crates/al-lsp/src/server/lsp.rs
- [ ] crates/al-lsp/src/server/workspace.rs
- [ ] crates/al-lsp/src/server/diagnostics.rs
- [ ] crates/al-lsp/src/server/commands.rs
- [x] crates/al-lsp/src/server/handlers.rs
- [x] crates/al-lsp/src/server/formatting.rs
- [ ] crates/al-lsp/src/server/completions.rs + definition.rs + hover.rs
- [ ] crates/al-lsp/src/server/dap_mode/
- [x] crates/al-lsp/src/server/mcp.rs
- [x] crates/al-lsp/src/server/daemon/mod.rs
- [ ] crates/al-lsp/src/server/daemon/lsp_dispatch.rs
- [ ] crates/al-lsp/src/server/daemon/debug_dispatch.rs
- [ ] crates/al-lsp/src/server/daemon/insight_dispatch.rs
- [ ] crates/al-lsp/src/server/daemon/build_dispatch/ (mod, build/, fixes, codegen, xliff, symbols_auth)
- [ ] crates/al-lsp/src/semantic/ + syntax/
- [ ] crates/al-lsp/tests/

## Findings

### [BUG] Generation read guard is still held across long awaits in five request handlers [STILL-OPEN]
- where: crates/al-lsp/src/server/lsp.rs:1656 (goto_implementation), 1699 (references), 1747 (document_symbol), 1825 (semantic_tokens_full), 2015 (symbol)
- severity: high
- scenario: The audit's fix for this class went only into `hover`/`completion`/`inlay_hint`, which now use `snapshot_after_ready` to drop the guard (lsp.rs:622). The five handlers above still do `let _generation = self.await_ready().await?;` and then `tokio::task::spawn_blocking(...).await`, so the `generation_lock` read guard is alive for the whole blocking walk. `tokio::sync::RwLock` is fair, so a queued writer blocks all later readers. Sequence: invoke "Find All References" on a symbol in a large workspace (the walk in `al_analysis::queries::references::references` runs seconds); type a key, so `did_change` (lsp.rs:1222) calls `generation_lock.write().await` and queues; every following `hover`, `completion`, `diagnostic`, `code_lens` calls `await_ready`, whose final line is `self.workspace.generation_lock.read().await` (lsp.rs:525) and queues behind the writer. That read is not covered by the 30 s `timeout` above it, so the requests hang with no deadline until the walk finishes. The document store is not updated in the meantime, so the editor's text and the server's text diverge for the duration.
- fix: Capture what the handler needs under the guard, drop it before the `spawn_blocking(...).await`, and revalidate with `generation_revision()` / the document snapshot afterwards, as `diagnostic` (lsp.rs:1902) and `workspace_diagnostic` (lsp.rs:1944) already do. A shared helper would make the pattern uniform.
- status: open

### [PERF] did_change parses and re-indexes the whole file on the async executor under the write lock
- where: crates/al-lsp/src/server/lsp.rs:1256 (`al_workspace::on_document_change`), implementation at crates/al-workspace/src/lib.rs:918
- severity: medium
- scenario: `on_document_change` runs `AlParser::parse_quick(text)`, `text.to_string()` (a full copy of the document), `procedures_snapshot`, `add_file_with_tree` and symbol-cache invalidation. All of it is synchronous, on the tokio worker, while `did_change` holds `generation_lock.write()`. On a 10k-line AL file every keystroke pays a full reparse plus a full-document clone on the executor thread that also drives all other connections' futures. The crate is otherwise careful to push exactly this kind of CPU work into `spawn_blocking` (references, documentSymbol, semanticTokens, workspaceSymbol).
- fix: Move the parse and re-index into `spawn_blocking`, keeping the write guard only around the store mutation, or debounce the file-index refresh the same way diagnostics are debounced.
- status: open

### [BUG] A partially read or partially written frame desynchronizes the daemon client
- where: crates/al-protocol/src/client.rs:46 (`read_bounded_line`), 192 (`write_all_bounded`), 501 (`read_response`)
- severity: medium
- scenario: The abandoned-id drain only resynchronizes when a *whole* response frame arrives late. Two paths break that assumption. (1) Read side: `read_bounded_line` accumulates into a local `buf` and `consume`s the bytes it took from the `BufReader`. When the request deadline expires mid-frame (a multi-megabyte `workspace/symbol` or `al_build` result still streaming), the function returns `Err`, `buf` is dropped, and the already-consumed prefix is gone. The remainder of that JSON line stays in the socket, so the next `request` reads a truncated fragment and fails with "Failed to parse response" even though the frame was well formed. (2) Write side: `write_all_bounded` can return `Err` after a partial write; `send_request` propagates the error but leaves the connection open, so the next `send_request` appends a second JSON object to the half-written line and the daemon sees one corrupt frame.
- fix: Mark the connection poisoned on any partial-frame read or write (a `desynced: bool` on `DaemonClient`), and either reconnect transparently or fail subsequent requests with a clear "connection out of sync, reconnect" error instead of a parse error.
- status: open

### [SECURITY] Daemon and MCP file dispatchers accept any absolute path, with no project-root containment
- where: crates/al-lsp/src/server/daemon/mod.rs:1123 (`file_uri_from_params`), 1078 (`ensure_document`), crates/al-lsp/src/server/daemon/build_dispatch/fixes.rs:97 (`dispatch_format`), crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:104 (`dispatch_new_project`)
- severity: high
- scenario: `file_uri_from_params` validates that the path canonicalizes to an existing regular file and nothing else. No check ties it to the daemon's `project_root`. `al-lsp mcp` publishes `al_call`, which forwards any `method`/`params` pair to `dispatch_request` (mcp.rs:1114). So an MCP client sends `{"name":"al_call","arguments":{"method":"format","params":{"file":"/home/user/.bashrc"}}}`. `dispatch_format` loads that file through `ensure_document` (no extension check, only the size cap in `validate_size`), runs `al_syntax::format_al` over it, and because `check` defaults to false and the formatter output differs from shell script text, `write_al_file_and_refresh` atomically overwrites the user's `.bashrc` with mangled content. The read direction is the same: `{"method":"source","params":{"file":"…/id_rsa"}}` or `format` with `check:true` discloses file content and existence outside the project. `dispatch_new_project` takes a `dir` and creates a project tree at any absolute path; its comment claims requiring `is_absolute()` "prevents path traversal", which it does not.
- fix: Add one containment helper next to `file_uri_from_params` that canonicalizes both the candidate and the loaded project root and rejects anything not under the root (plus the configured package cache and `appLocalFolderPaths`), and route every dispatcher that takes `uri`/`file`/`dir`/`output` through it. At minimum apply it to the write paths (`format`, `fix*`, `sortMembers`, `organizeFiles`, `tests.mutate`, `xlf.*`, `newProject`) before the MCP surface is exposed to an agent.
- status: open

### [BUG] `dispatch_format` calls `block_in_place` without the current-thread-runtime guard every other call site has
- where: crates/al-lsp/src/server/daemon/build_dispatch/fixes.rs:191
- severity: low
- scenario: `tokio::task::block_in_place` panics on a current-thread runtime. Every other call site in the crate guards it by checking `Handle::try_current().runtime_flavor()` first: `ensure_document` (daemon/mod.rs:1088), `scan_current_workspace_symbols` (build_dispatch/mod.rs:276), `xliff::blocking` (xliff.rs:62), `symbols_auth` (symbols_auth.rs:605). This one does not. A `format` request that names a file and does not set `check` therefore aborts the process when the dispatcher is driven from a current-thread runtime, which is what a plain `#[tokio::test]` gives you and what an embedder may use. The shipped binaries build a multi-thread runtime (bin/al-lsp.rs:105, 128), so this is not reachable from the released daemon or MCP server today.
- fix: Reuse the same guarded helper. The `xliff::blocking` function already has the exact shape; promote it to `daemon::blocking` and call it from all five sites.
- status: open

### [GAP] Daemon connections still dispatch strictly sequentially [STILL-OPEN]
- where: crates/al-lsp/src/server/daemon/mod.rs:405 (`handle_connection`'s `while let Some(line)` loop)
- severity: medium
- scenario: The audit's other half of this item (silent drop at the connection limit) is fixed by `reject_connection_over_limit` (daemon/mod.rs:311). Per-request concurrency is not: the loop awaits `dispatch_request` to completion before reading the next line. A client that pipelines `ping` behind a `tests.run` or `downloadSymbols` on the same connection waits for the long call. `al-explorer`'s `DaemonClient` is one connection per process and the MCP path worked around this separately by spawning `tools/call` tasks (mcp.rs:1400), so the daemon transport is the only surface left without it.
- fix: Spawn each request into a task, cap the per-connection in-flight count, and serialize writes behind a `tokio::sync::Mutex` on the writer, mirroring `run_mcp`'s `write_mcp_frame`.
- status: open

### [SLOP] Incorrect rationale comment on the `newProject` absolute-path check
- where: crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:122
- severity: low
- scenario: The comment says requiring an absolute `dir` prevents path traversal, giving the example `"../../etc/malicious-dir"`. Rejecting relative paths does not prevent traversal: `/etc/malicious-dir` is absolute and passes. The check is still worth keeping (a relative path would resolve against the daemon's cwd, which is not the project), but the stated reason misleads the next reader into thinking containment is handled.
- fix: Rewrite the comment to say what the check actually buys ("a relative path would resolve against the daemon process cwd, not the project") and add the real containment check described in the SECURITY finding above.
- status: open

### [BUG] `al.compile` holds the generation read guard for the whole build
- where: crates/al-lsp/src/server/lsp.rs:2049-2060 (`execute_command`), crates/al-lsp/src/server/commands.rs:242 (`compile`)
- severity: high
- scenario: `execute_command` takes the guard with `let mut generation = Some(self.await_ready().await?)` and drops it only for `al.downloadSymbols*` and `al.reindex`. `al.compile` keeps it. `commands::compile` then runs `native_workspace_compile_diagnostics` (a whole-workspace semantic pass, synchronous, on the async task) and awaits `al_compile::build`, which for `al.useOfficialCompiler=true` spawns `dotnet alc` and waits for it. On a real project that is tens of seconds. For the whole time the read guard is held, so the first keystroke after clicking Compile blocks in `did_change`'s `generation_lock.write().await`, and every later request queues behind that writer. The editor stops accepting AL edits until the build finishes. The same holds for `al.findReferences`, `al.lintFile` and `al.runTest`, which also keep the guard across their awaits.
- fix: Invert the list: drop the guard for every command and let the few that genuinely need a stable generation re-acquire it around the short read they depend on. `commands::compile` already clones everything it needs (root, packages_dir, packages, config) before doing the work, so it does not need the guard at all.
- status: open

### [BUG] Project-scope push diagnostics can spin without ever publishing, stalling edits each pass
- where: crates/al-lsp/src/server/diagnostics.rs:249 (`publish_workspace_diagnostics_parts` retry loop)
- severity: medium
- scenario: The loop takes `generation_lock.read()`, runs `compute_workspace_push_diagnostics` over every indexed file, drops the guard, reacquires it, and if `generation_revision()` moved it calls `yield_now()` and starts over. `al_workspace::on_document_change` bumps the revision on every keystroke (al-workspace/src/lib.rs:986), so with `diagnosticsScope: "project"` on a workspace where the pass takes longer than the user's typing gaps, the loop discards its result and recomputes forever, never publishing. Each iteration also holds the read guard for the whole pass, so the `did_change` writer queued behind it blocks for that long, and the requests queued behind the writer block too. The same code runs from `did_save` (lsp.rs:1412) and `did_close` (lsp.rs:1388), where the user is not necessarily typing, but the debounced path (lsp.rs:892) re-arms on each keystroke and is exactly where it bites.
- fix: Bound the retry (recompute at most once, then publish the older generation with its recorded versions and let the next debounce correct it), and compute from a cloned snapshot rather than under the read guard so the pass never queues a writer.
- status: open

