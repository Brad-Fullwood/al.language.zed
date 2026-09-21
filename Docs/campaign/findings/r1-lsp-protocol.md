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
- [ ] crates/al-lsp/src/server/lsp.rs
- [ ] crates/al-lsp/src/server/workspace.rs
- [ ] crates/al-lsp/src/server/diagnostics.rs
- [ ] crates/al-lsp/src/server/commands.rs
- [ ] crates/al-lsp/src/server/handlers.rs
- [ ] crates/al-lsp/src/server/formatting.rs
- [ ] crates/al-lsp/src/server/completions.rs + definition.rs + hover.rs
- [ ] crates/al-lsp/src/server/dap_mode/
- [ ] crates/al-lsp/src/server/mcp.rs
- [ ] crates/al-lsp/src/server/daemon/mod.rs
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

