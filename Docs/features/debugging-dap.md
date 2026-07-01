# Debugging (DAP) & Business Central Runtime

**Modules:** `crates/al-dap/src/dap/`, `native_debug.rs`, `bc_client.rs`, `http_auth.rs`,
`profiling.rs`, `snapshot.rs` + `src/dap.rs` (Zed glue) + `debug_adapter_schemas/al.json` ·
**Status:** ✅ shipped (core flow); some schema fields parsed-but-unused

The debugger is a **native Rust Debug Adapter Protocol (DAP) server** that talks directly to a
Business Central server over SignalR + REST — no Microsoft `EditorServices.Host` required (it remains
available as a legacy fallback). Zed launches `al-lsp --dap`; the adapter compiles (natively),
publishes the `.app`, connects to BC's debug hub, and drives breakpoints/stepping/inspection.

## Architecture

```
Zed ──DAP/stdio──► al-lsp --dap ──SignalR + REST──► Business Central server
                   (native_dap.rs)                  (/dev/apps, /dev/DebuggerHub)
```

| Layer | File | Role |
| --- | --- | --- |
| Native DAP server | `dap/native_dap.rs` | the Zed-facing stdio DAP server; compile→publish→attach→drive |
| DAP wire types & framing | `dap/protocol.rs`, `dap/framing.rs` | `Content-Length` framing (8 KiB header cap, 20 MB body cap), `seq` patching |
| DAP subprocess client | `dap/client.rs` | (legacy path) spawn + route a DAP subprocess by `request_seq` |
| BC debug session | `dap/bc_debug.rs` | SignalR (WebSocket) client to `/dev/DebuggerHub` |
| Config | `dap/config.rs` | parse launch config (on-prem vs cloud, auth) |
| JSONC helpers | `dap/json_util.rs` | strip comments/trailing commas from `launch.json` |
| High-level session | `native_debug.rs` | wrap session, breakpoint registry, 10k-entry hit history |
| BC REST | `bc_client.rs` | publish `.app`, RAD delta, status (size caps + secret redaction) |
| TLS helper | `http_auth.rs` | client builder; warns loudly when cert validation is disabled |
| Profiling | `profiling.rs` | start/stop CPU profiling, parse `.alcpuprofile` hotspots |
| Snapshots | `snapshot.rs` | start/list/download snapshot (`.alvsc`) |

## DAP requests implemented

`initialize`, `configurationDone`, `launch`, `attach`, `setBreakpoints` (incl. **conditional**),
`continue`, `next`, `stepIn`, `stepOut`, `threads` (single "AL Thread"), `stackTrace`, `scopes`
(Locals + Globals), `variables`, `evaluate` (watch), `disconnect`, `terminate`. `pause` returns an
explicit error — **BC's debug hub has no pause-while-running API**. Events emitted: `initialized`,
`stopped`, `output`, `al/openUri` (browser launch), `terminated`.

Advertised capabilities include conditional breakpoints, evaluate-for-hovers, terminate, and delayed
stack-trace loading; not supported: function breakpoints, set-variable, completions, restart, step-back.

## How launch/attach works

**Launch** (`native_dap.rs`):
1. **Compile** natively (`build::native_compile`) — no `alc`.
2. **Authenticate** (OAuth callback / env token).
3. **Publish** the `.app` to BC (`POST …/dev/apps`); a missing `.app` fails the launch with a clear
   error rather than silently debugging a stale build (F-013).
4. **Connect** SignalR to `/dev/DebuggerHub`, **Attach** (`breakOnError`, `breakOnRecordWrite`).
5. Optionally **open the browser** at the debug-context URL when `launchBrowser` is set.

**Attach** skips compile/publish and connects to an existing session.

## BC SignalR integration (`bc_debug.rs`)

A from-scratch SignalR (protocol v1) client: negotiates (`/negotiate?negotiateVersion=1`, validating
the negotiated version and `connectionToken`), opens a WebSocket with the record-separator handshake,
and runs reader/writer tasks. Hub methods invoked include `Attach`,
`DebugAdapterConfigurationDone`, `AddBreakpoint`/`RemoveBreakpoint`/`UpdateBreakpoint`,
`GetStackTrace`, `GetVariables`/`ExpandGlobals`/`GetWatchNode`, `SetBreakpointResponse`
(0=continue, 1=step-over, 2=step-in, 3=step-out), `StopDebugging`/`TerminateSession`. Server
callbacks handled: `Break` (→ `stopped`), `IsAlive` (→ ack), `OnAttachedToConnection`,
`OnDetachedFromConnection` (→ `terminated`), `OnFatalDebuggerException` (→ `output`). All
field access tolerates both PascalCase and camelCase from different BC versions.

### Engineering details worth knowing

- **Breakpoint serialization (F-OPEN-014):** the breakpoint mutex is held across remove→add→store so
  concurrent `setBreakpoints` can't orphan BC breakpoints; AL file paths resolve to (ObjectType,
  ObjectId) via the workspace index.
- **Per-operation timeouts (F-OPEN-015):** step/continue 10 s, IsAlive 5 s, variables/stack 30 s,
  attach/config 120 s — instead of one blanket timeout.
- **Lock discipline (T-023):** clone the session `Arc` inside the lock, drop the lock, then await.
- **Bounded event channel** (1024) with a dedicated unbounded channel for `Break` events so a paused
  breakpoint is never dropped under back-pressure.
- **Security:** REST size caps (500 MB upload / 16 MB JSON / 500 MB binary), error-body redaction of
  bearer tokens/passwords/secrets, and a loud warning whenever TLS cert validation is disabled.

## Profiling & snapshots

- **Profiling (`profiling.rs`):** start/stop CPU profiling against the BC dev endpoint, download an
  `.alcpuprofile` (Chrome DevTools format), and analyze hotspots (self-time ≈ hit count). See also
  [analysis-and-insight](./analysis-and-insight.md) for mapping hotspots to source.
- **Snapshots (`snapshot.rs`):** start/list/download snapshot debugging data (`.alvsc`), with response
  shapes for both bare arrays and OData envelopes and filename sanitization against path traversal.

## Zed integration

`src/dap.rs` builds the adapter command: default `--dap` (native), `--dap-legacy` for the Microsoft
proxy via `al.useOfficialDap`. Debug configs are authored with the bundled snippets and validated
against `debug_adapter_schemas/al.json`, which mirrors Microsoft's AL launch schema (authentication,
breakOnError/Next/RecordWrite, environmentType/Name, tenant, server/serverInstance/port,
launchBrowser, startupObjectType/Id, schemaUpdateMode, dependencyPublishingOption,
validateServerCertificate). A build step (`al-explorer compile`) is attached to launch flows.

## Microsoft comparison

| Aspect | This project | Microsoft AL debugger |
| --- | --- | --- |
| Transport | native Rust DAP over stdio | external C# `EditorServices.Host` |
| Compile for launch | pure-Rust emitter | `alc` + C# bridge |
| BC connection | direct SignalR + REST | EditorServices proxy |
| Conditional breakpoints | ✅ | ✅ |
| Pause-while-running | ❌ (BC limitation) | ❌ (BC limitation) |
| Timeouts | per-operation | blanket |
| Error bodies | capped + redacted | unbounded |
| Fallback | `al.useOfficialDap` keeps the Microsoft proxy | n/a |

## Why this approach

Owning the DAP server in Rust means Zed gets first-class launch/attach without being a VS Code clone,
the launch path reuses the fast native emitter, and the BC protocol handling can be hardened
(per-operation timeouts, secret redaction, back-pressure-safe break events) in ways a black-box proxy
can't. The BC runtime remains the source of truth for *executing* AL — this is a better control plane
around it, not a reimplementation of it.

## How to use

- **In Zed:** create a debug config from the snippets, then launch/attach from the debugger UI.
- **CLI:** `al-explorer debug start|breakpoint|state|eval|continue|step|history|stop`;
  `al-explorer profile start|stop|analyze`; `al-explorer snapshot start|list|download`;
  `al-explorer init-debug` to scaffold `.zed/debug.json`.

## Limitations & roadmap

- ❌ No pause, function breakpoints, set-variable, completions, restart, or step-back.
- 🟡 Structured value expansion is shallow (`variablesReference` is 0; drilling into records isn't
  wired yet).
- ✅ Schema reconciled (gap A7): `sessionId` and `breakOnNext` are now forwarded to the BC `Attach`
  payload; the fields with no native behavior (`useMcpServerForDebugging`, `mcpServicePort`, `userId`,
  `useVsCodeAuthentication`, `primaryTenantDomain`, snapshot/profiling config) were **removed** from
  `debug_adapter_schemas/al.json` (with a `$comment` pointing to `al-explorer snapshot`/`profile`).
- `ROADMAP.md` (Debugging): consume or remove unsupported fields, harden stack/scopes/variables/
  evaluate against current BC contracts, add explicit tests for the full launch→publish→attach→step→
  evaluate→disconnect flow, and bring DAP compile/deploy into the shared build service.
