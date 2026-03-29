# al-daemon-client — Shared IPC Types (~450 lines)

**Leaf crate** — must NOT depend on al-core or any other al-* crate. Used by al-cli, al-explorer, and al-lsp's daemon-mode tests.

## Key Types

- `DaemonClient` — synchronous Unix socket client with auto-start and 3× retry on "initializing" errors
- `Request` / `Response` / `RpcError` — canonical JSON-RPC wire types
- `socket_path(project_root)` — deterministic path via FNV-1a 64-bit hash: `$XDG_RUNTIME_DIR/al-lsp/<16-hex>.sock`

## Modules

| File | Purpose |
|------|---------|
| lib.rs | Re-exports `socket_path` and `DaemonClient` |
| socket.rs | FNV-1a hash, `socket_path()`, hash stability tests |
| jsonrpc.rs | `Request`, `Response`, `RpcError`, `error_codes` |
| client.rs | `DaemonClient`, `find_al_lsp_binary()`, mock server tests |

## Constraints

- **Unix-only**: `DaemonClient` is `#[cfg(unix)]` — compiles on Windows but client is absent
- **Synchronous**: `std::os::unix::net::UnixStream` with 30s read timeout
- Dependencies: `serde`, `serde_json` only — no tokio, no async
- Max response: 64 MiB line buffer
- Retry: 3 retries × 500 ms for workspace-initializing errors
