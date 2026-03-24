# Consolidate Publish/Deploy to Single Code Path

**Date:** 2026-03-24
**Status:** Approved
**Branch:** v3

## Problem

The publish/deploy pipeline has duplicated business logic in `al-dap-client` that violates the project's core architecture rule: all business logic flows through `al-lsp` (daemon) into `al-core`. CLI and Zed are thin transport adapters.

Three functions in `al-dap-client` duplicate what `al-core` already provides:

| Duplicate (al-dap-client) | Canonical (al-core) | Bug in duplicate |
|---|---|---|
| `native_dap::compile_project()` | `al_core::build::compile_project()` | Missing `/out:`, analyzers, structured diagnostics |
| `native_dap::find_app_file()` | `al_core::build::find_app_file()` | Returns first `.app` found (random), not manifest-matched |
| `bc_debug::publish_app()` | `al_core::bc_client::publish_extension()` | Different endpoint format, no 422 duplicate handling |

Additionally:
- `al publish` CLI command is referenced in `.zed/tasks.json` but does not exist
- `al-core::publish` module is fully implemented but not wired to daemon or CLI
- `al debug start` (daemon path) skips compile+publish entirely
- 422 "duplicate packageId" errors crash the deploy with no recovery

## Architecture Rule

```
Zed (DAP stdio) ─┐
CLI (JSON-RPC)   ─┤─→ al-lsp daemon ─→ al-core (business logic)
                  │
                  └─ al-dap-client: transport only (DAP framing, SignalR debug hub)
```

`al-dap-client` must never contain compile, publish, or file discovery logic.

## Design

### 1. Wire `al-core::publish` to daemon dispatch

Add `"publish"` to the daemon dispatch table in `al-lsp/src/daemon/mod.rs`.

Create `dispatch_publish` in `build_dispatch.rs` that calls `al_core::publish::publish()` with parameters from the JSON-RPC request:
- `project_root` (from workspace)
- `config_name` (optional, which launch config to use)
- `no_debug` (bool, skip debug session after publish)
- `incremental` (bool, use RAD deploy)

Returns `PublishResult` as JSON.

### 2. Wire `al publish` CLI command

Add `Commands::Publish` to al-cli with flags:
- `--config <name>` — launch config name
- `--no-debug` — publish without launching debugger
- `--incremental` — use RAD deploy

Sends `"publish"` JSON-RPC request to daemon. Prints result or error.

### 3. Fix 422 duplicate handling in `al-core::bc_client`

Consolidate the publish endpoint first. Currently:
- `bc_client::publish_extension()` uses `POST {base}/dev/extensions` (raw binary, `Content-Type: application/octet-stream`)
- `bc_debug::publish_app()` uses `POST {base}/dev/apps?tenant=...` (multipart form)

The correct BC cloud endpoint is `POST {base_url}/apps?tenant={tenant}&SchemaUpdateMode={mode}&DependencyPublishingOption={option}` with multipart form body. Fix `bc_client` to use this endpoint and format. Remove `bc_debug::publish_app()`.

Add `unpublish_extension()` method to `BcClient`:
- `DELETE {base_url}/apps?appId={app_id}&appVersion={version}&tenant={tenant}`
- `app_id` comes from `app.json` `"id"` field (already extracted by `publish.rs::extract_app_id_from_manifest()`)
- `version` comes from `app.json` `"version"` field

422 retry logic in `publish_extension()`:
1. POST to publish endpoint
2. If 422 with "duplicate package" in response body:
   - Parse the response body JSON to extract the error message (currently the body is read as a string — deserialize it as `{"Message": "...", "ErrorType": "..."}`)
   - Call `unpublish_extension(app_id, version)` using the app_id/version from `app.json`
   - Retry POST once
3. If retry still fails, return the real error (not triple-wrapped)

### 4. Rewrite DAP launch handler

Replace the inline compile+publish in `native_dap.rs` launch handler with a JSON-RPC call to the daemon.

The DAP server needs a daemon client connection. Options:
- **Option A:** Pass a `DaemonClient` into `run_native_dap()` — the DAP server connects to the daemon socket and sends JSON-RPC requests.
- **Option B:** Pass callback closures that the al-lsp binary provides, bridging to al-core directly.

**Choose Option A** — it matches the CLI pattern and keeps the DAP server as a pure transport adapter. The al-lsp binary already knows the daemon socket path.

**Async/sync bridging:** `DaemonClient` is synchronous (blocking `UnixStream` I/O). `run_native_dap()` is async (Tokio). Use `tokio::task::spawn_blocking` to wrap the `DaemonClient::request()` call. This avoids blocking the Tokio executor during the potentially long compile+publish operation. The connected `DaemonClient` is moved into the closure.

**Timeout:** `DaemonClient` defaults to 30s read timeout, which is too short for compile+publish (can exceed 60s for large projects, plus 300s upload to BC). Call `client.set_read_timeout(Duration::from_secs(300))` before sending the `"publish"` request. This matches the 300s timeout already used in `bc_client.rs` for the HTTP upload.

**Token forwarding:** The DAP server acquires an OAuth Bearer token via the `acquire_token` callback (browser-based device code flow). This token must reach `bc_client` on the daemon side. Pass it as an `"accessToken"` field in the `"publish"` JSON-RPC params. The `dispatch_publish` handler injects it into the `PublishConfig` (add an `access_token: Option<String>` field). `bc_client::apply_auth()` checks for this override token before falling back to `BC_TOKEN` env var.

New launch flow:
```
Zed sends DAP "launch"
  → native_dap acquires OAuth token via acquire_token callback
  → native_dap sends "publish" JSON-RPC to daemon (via spawn_blocking + DaemonClient)
    params: { accessToken, configName, incremental }
  → daemon calls al_core::publish::publish() with token override
  → daemon returns PublishResult (success/failure, diagnostics, app_path)
  → native_dap streams diagnostics to Zed as DAP output events
  → native_dap proceeds to SignalR debug connection (this stays in al-dap-client)
```

### 5. Delete duplicated code

Remove from `al-dap-client`:
- `native_dap::compile_project()` (lines 546-578)
- `native_dap::find_app_file()` (lines 580-591)
- `bc_debug::publish_app()` (lines 206-253)

### 6. Fix `al debug start` daemon path

Currently `dispatch_debug` cmd=start goes straight to `NativeDebugSession::start()` (SignalR connect + attach) without compiling or publishing.

Add compile+publish step before debug session start:
1. Extract project root via `require_project_root(workspace, id)?`
2. Build `PublishConfig` from params + project root
3. Call `al_core::publish::publish()` first
4. If publish succeeds, proceed to `NativeDebugSession::start()`
5. Pass the access token (from `params["accessToken"]` or from the publish config) through to the debug session

### 7. Fix tasks.json

`"AL: Publish"` task now works because `al publish` exists. No changes needed to the task definition — it already has the right args.

### 8. Fix error wrapping

The triple "Publish failed: Publish failed: Publish failed" comes from:
1. `DapError::PublishFailed(format!("Publish failed (HTTP ...): ..."))`
2. `DapError` Display impl prepends "Publish failed:"
3. DAP handler prepends "Publish failed:"

Fix: `PublishFailed` variant stores the raw detail message. Display shows `"Publish failed: {detail}"` once. Callers don't prepend.

## What stays in al-dap-client

- `BcDebugConfig` + `from_dap_args()` — DAP argument parsing
- `BcDebugSession` — SignalR debug hub client (transport-specific)
- `BcDebugConfigBuilder` — fluent config builder
- DAP message framing (`framing.rs`)
- `native_dap::run_native_dap()` — DAP protocol loop (but gutted of business logic)
- `get_metadata()` — may be useful for debug session setup

## What moves to al-core

- The correct publish endpoint format (multipart POST to `/dev/apps`) merges into `bc_client::publish_extension()`
- 422 duplicate retry logic goes into `bc_client`

## Testing

- Existing e2e tests in al-test-harness verify LSP features (unchanged)
- Add integration test for daemon `"publish"` handler: mock BC server, send publish request, verify compile+upload flow
- Add test for 422 retry: first publish returns 422, unpublish succeeds, retry succeeds
- Add test for `find_app_file` manifest matching (already covered in al-core unit tests, verify)

## Files Changed

| File | Change |
|---|---|
| `crates/al-core/src/bc_client.rs` | Fix publish endpoint format, add 422 retry, add unpublish method |
| `crates/al-core/src/publish.rs` | Ensure wired correctly, fix error types |
| `crates/al-lsp/src/daemon/mod.rs` | Add `"publish"` to dispatch table |
| `crates/al-lsp/src/daemon/build_dispatch.rs` | Add `dispatch_publish` |
| `crates/al-lsp/src/daemon/debug_dispatch.rs` | Add compile+publish before debug start |
| `crates/al-cli/src/main.rs` | Add `Commands::Publish` |
| `crates/al-cli/src/commands/build.rs` | Add `cmd_publish()` |
| `crates/al-dap-client/src/native_dap.rs` | Remove compile/find_app/publish, add daemon client call |
| `crates/al-dap-client/src/bc_debug.rs` | Remove `publish_app()` |
| `crates/al-dap-client/src/lib.rs` | Fix `DapError::PublishFailed` display |
| `.zed/tasks.json` | No changes needed (already correct) |

## Dependency Rules (verified)

- `al-dap-client` does NOT import `al-core` (unchanged)
- `al-dap-client` uses `al-daemon-client::DaemonClient` for JSON-RPC (new dependency, allowed — `al-daemon-client` is the IPC types crate)
- `al-lsp` imports both `al-core` and `al-dap-client` (unchanged)
- `al-cli` uses `al-daemon-client::DaemonClient` (unchanged)
