# al-dap-client — Debug Adapter Protocol Client (~3K lines)

**Leaf crate** — must NOT depend on al-core, al-syntax, or al-symbols.

## Quick Reference

```sh
cargo test -p al-dap-client                 # all tests (~31 inline: framing, config parsing, protocol)
cargo check -p al-dap-client                # compile check
```

Two debug approaches coexist:
- **Native** (`bc_debug.rs`, `native_dap.rs`): Direct REST + SignalR to Business Central — no EditorServices.Host needed
- **Proxy** (`client.rs`): Low-level DAP framing for communicating with EditorServices.Host process

## Key Types

- `DapError` / `Result<T>` — error enum (IO, spawn, compilation, protocol, timeout, etc.)
- `BcDebugConfig` / `BcDebugSession` / `BcEvent` — native BC debug session
- `DapLaunchConfig` / `DebugConfigFile` — parsed `.zed/debug.json` or `.vscode/launch.json`
- `SessionStatus`, `DebugState`, `StackFrame`, `Variable`, `BreakpointInfo` — debug state types

## Modules

| File | Purpose |
|------|---------|
| lib.rs | `DapError`, `Result`, module declarations |
| bc_debug.rs | Native BC debug client (REST + SignalR), reverse-engineered from EditorServices.Protocol.dll |
| native_dap.rs | Native DAP server (stdio ↔ BC REST+SignalR translation), `bc_object_type` constants |
| client.rs | Low-level DAP client for EditorServices.Host |
| config.rs | Debug config parsing (Zed + VS Code formats) |
| framing.rs | DAP HTTP-framing (Content-Length headers) |
| protocol.rs | DAP protocol types |
| json_util.rs | `strip_json_comments()` for JSONC config parsing |
| types.rs | Serializable debug state result types |

## Gotchas

- `tokio-tungstenite` pinned to 0.26 with `native-tls` — changing requires validating BC's TLS handshake
- BC `ObjectTypeWrapper` integers are reverse-engineered from Newtonsoft.Json enum defaults — BC enum layout changes break the mapping
