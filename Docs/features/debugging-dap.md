# Debugging (DAP) & Business Central Runtime

**Modules:** `crates/al-dap/src/dap/`, `native_debug.rs`, `bc_client.rs`, `http_auth.rs`,
`profiling.rs`, `snapshot.rs` + `crates/al-lsp/src/server/daemon/debug_dispatch.rs` (CLI/MCP
control plane) + `src/dap.rs` (Zed glue) + `debug_adapter_schemas/al.json` ·
**Status:** ✅ shipped (core flow); every field advertised by the native schema is
consumed by Zed or the native adapter

The debugger is a **native Rust Debug Adapter Protocol (DAP) server** that talks directly to a
Business Central server over SignalR + REST — no Microsoft `EditorServices.Host` required (it remains
available as a legacy fallback). Zed launches `al-lsp --dap`; the adapter selects the shared verified
native build by default (or the explicit persisted `al.useOfficialCompiler` backend), publishes the
manifest-selected `.app`, connects to BC's debug hub, and drives breakpoints/stepping/inspection.

The `al-lsp mcp` server exposes the debugger through the stateful `al_debug` tool. An MCP client can
retain a debug session
across tool calls, attach, set conditional breakpoints, inspect the call stack, locals, globals, and
structured values, evaluate AL expressions in a selected frame, continue, step in/over/out, inspect
breakpoint history, and stop the session. MCP does not tunnel opaque DAP frames: both entry points
share the same native BC debug implementation, so clients receive structured JSON results and the
runtime behavior does not fork into a second debugger.

## Architecture

```
Zed ─────── DAP/stdio ──────► native_dap.rs ─────────► BC REST + SignalR
                                                         (/dev/apps, /dev/DebuggerHub)
MCP client ── al_debug ─────► debug_dispatch.rs
                                  │
                                  └── persistent NativeDebugSession ──► BC SignalR
                                                                      (/dev/DebuggerHub)
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
| MCP control plane | `server/daemon/debug_dispatch.rs` | stateful structured commands shared by CLI and MCP |
| MCP tool | `server/mcp.rs` (`al_debug`) | schema mapped to daemon method `debug` |
| BC REST | `bc_client.rs` | publish `.app`, RAD delta, status (size caps + secret redaction) |
| TLS helper | `http_auth.rs` | client builder; warns loudly when cert validation is disabled |
| Profiling | `profiling.rs` | start/stop CPU profiling, parse `.alcpuprofile` hotspots |
| Snapshots | `snapshot.rs` | start/list/download snapshot (`.alvsc`) |

## DAP requests implemented

`initialize`, `configurationDone`, `launch`, `attach`, `setBreakpoints` (incl. **conditional**),
`continue`, `next`, `stepIn`, `stepOut`, `threads` (single "AL Thread"), `stackTrace`, `scopes`
(Locals + Globals), `variables`, `evaluate` (watch), `disconnect`, `terminate`. `pause` returns an
explicit error — **BC's debug hub has no pause-while-running API**. Requests for
`setFunctionBreakpoints`, `setVariable`, `completions`, `restart`, and `stepBack` also receive
command-specific failure responses; they are never silently acknowledged. Events emitted: `initialized`,
`stopped`, `output`, `al/openUri` (browser launch), `terminated`.

Advertised capabilities include conditional breakpoints, evaluate-for-hovers, terminate, and delayed
stack-trace loading. Function breakpoints, set-variable, completions, restart/restart-frame, and
step-back remain explicitly `false`.

That capability boundary is based on the installed Microsoft AL 17.0.2273547 EditorServices protocol
assembly and its live `HubBasedDebuggerService` contract, not on DAP type names alone. Its protocol
metadata contains the stack/variables/watch request family used here (`GetVariablesAsync`,
`GetWatchNodeAsync`, and stack-frame conversion), while the reference adapter's completions
request is an editor-workspace/LSP round trip rather than a Business Central hub operation. Its
initialize result also advertises restart even though that assembly registers no restart request
handler and exposes no live restart hub method. The native adapter therefore does not advertise
either feature until it can provide the full behavior itself.

## How launch/attach works

**Launch** (`native_dap.rs`):
1. **Compile** through the shared build service: verified native by default, or `alc` only when
   `al.useOfficialCompiler` is explicitly enabled in persisted project settings.
2. **Authenticate** (OAuth callback / env token).
3. **Publish** the `.app` to BC (`POST …/dev/apps`); a missing `.app` fails the launch with a clear
   error rather than silently debugging a stale build.
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

- **Breakpoint serialization:** the breakpoint mutex is held across remove→add→store so
  concurrent `setBreakpoints` can't orphan BC breakpoints; AL file paths resolve to (ObjectType,
  ObjectId) via the workspace index.
- **Per-operation timeouts:** step/continue 10 s, IsAlive 5 s, variables/stack 30 s,
  attach/config 120 s — instead of one blanket timeout.
- **Lock discipline:** clone the session `Arc` inside the lock, drop the lock, then await.
- **Bounded event channel** (1024) with a dedicated unbounded channel for `Break` events so a paused
  breakpoint is never dropped under back-pressure.
- **Security:** REST size caps (500 MB upload / 16 MB JSON / 500 MB binary), error-body redaction of
  bearer tokens/passwords/secrets, and a loud warning whenever TLS cert validation is disabled.

## Profiling & snapshots

- **Profiling (`profiling.rs`):** start/stop CPU profiling against the BC dev endpoint, download an
  `.alcpuprofile` (Chrome DevTools format), and analyze hotspots using sampled `timeDeltas`, with
  hit counts as a fallback when timing data is absent. See also
  [analysis-and-insight](./analysis-and-insight.md) for mapping hotspots to source.
- **Snapshots (`snapshot.rs`):** start/list/download snapshot debugging data (`.alvsc`), with response
  shapes for both bare arrays and OData envelopes and filename sanitization against path traversal.

## Zed integration

`src/dap.rs` builds the adapter command: default `--dap` (native), `--dap-legacy` for the Microsoft
proxy via `al.useOfficialDap`. Debug configs are authored with the bundled snippets and validated
against `debug_adapter_schemas/al.json`, which exposes the native adapter's consumed launch fields
(authentication, breakOnError/Next/RecordWrite, environmentType/Name, tenant,
server/serverInstance/port, launchBrowser, startupObjectType/Id, schemaUpdateMode,
dependencyPublishingOption, and validateServerCertificate). Zed consumes the schema's optional
`build` field when a user explicitly supplies one. Generated launch scenarios leave it empty:
the adapter's launch request already compiles through the shared build service and selects the
manifest-derived artifact atomically, while attach intentionally performs no build.

## MCP debug control

`al_debug` maps directly to daemon method `debug`. The MCP server owns one `Workspace` for its
lifetime, and that workspace retains the `NativeDebugSession`, so separate `tools/call` requests are
one continuous debugging session rather than isolated commands.

| `cmd` | Relevant parameters | Result/effect |
| --- | --- | --- |
| `start` | `config?`, `accessToken?`, or inline BC connection fields | Attach using the named project debug configuration (or the first AL configuration) and retain the session. Callers can instead supply `tenant`/`environmentName` for BC online or `server`/`serverInstance` for on-prem, plus authentication and attach selectors. |
| `breakpoint` | `file`, `line`, `condition?`, `objectType?`, `objectId?` | Replace the breakpoints for that file and return BC verification details. Object metadata is normally resolved from the workspace index. |
| `state` | — | Return running/paused status, current location, variables, session ID, and thread ID. Calling it also drains pending BC break events. |
| `stack` | — | Return the complete BC call-stack payload for the current stop, enriched with a zero-based `frameId` for follow-up inspection. |
| `variables` | `frameId?` | Return parsed locals for a stack frame (frame 0 by default). |
| `globals` | `frameId?` | Return parsed globals for a stack frame. |
| `expand` | `path`, `frameId?` | Expand a structured variable path in a stack frame. |
| `eval` | `expr`, `frameId?` | Evaluate an AL watch expression in the selected paused frame. |
| `continue` | — | Resume execution. |
| `step` | `stepType: over\|in\|out` | Resume with the selected BC step mode. |
| `history` | `var?` | Return the bounded breakpoint-hit history, optionally filtered by variable name. |
| `stop` | — | Stop and terminate the BC debug session and clear it from the MCP workspace. |

A typical client loop is `start` → `breakpoint` → poll `state` until paused → inspect `stack`
and `variables`/`globals` → `eval` and/or `step` → `continue` → `stop`. All results use MCP's normal
structured tool response and daemon errors are returned with `isError: true`, so clients do not need
to scrape terminal output.

For BC online and AAD targets, `accessToken` is optional. When it is absent, `al_debug start` uses the
same keyring-backed OAuth cache and refresh flow as `al-explorer authenticate login`; clients do not
need to retrieve, print, or relay a bearer token. An explicit token remains available for headless
automation. Some BC online environments accept `Attach` but reject the known
`DebugAdapterConfigurationDone` signatures; as in the editor DAP path, that compatibility call is
warned and treated as non-fatal after a successful attach.

This is a structured MCP control plane, not a second wire-level DAP endpoint. Zed's `launch`
request owns the compile-and-publish convenience flow; `al_debug start` attaches to the configured
BC debug service. A client can call `al_build` to produce a fresh artifact, but publishing that
artifact is not part of `al_debug start` today. The target app must already be published, and the BC
runtime must be available and authenticated. Connection details can come from `.zed/debug.json`,
`.vscode/launch.json`, or the inline `al_debug start` arguments.

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

## Design rationale

The native DAP server gives Zed launch/attach support, reuses the native emitter, and allows the BC
protocol handling to enforce
(per-operation timeouts, secret redaction, back-pressure-safe break events) in ways a black-box proxy
can't. The BC runtime remains the source of truth for *executing* AL — this is a better control plane
around it, not a reimplementation of it.

The service-backed contract is `make live-bc-contracts`. It drives the real
DAP framing and BC service through launch compilation, publication, attach,
breakpoint verification, stack/scopes/locals, evaluation, step, continue, and
disconnect. The profile requires explicit AAD configuration, test identity,
breakpoint, evaluation expression, BC version, and bearer token; missing inputs
exit as `UNAVAILABLE`, never passed.

## How to use

- **In Zed:** create a debug config from the snippets, then launch/attach from the debugger UI.
- **From an MCP client:** connect to **AL Tools**, then call `al_debug` with `cmd: "start"` and continue
  with `breakpoint`, `state`, `stack`, `variables`/`globals`/`expand`, `eval`, `continue`/`step`, and
  `stop` calls in the same MCP process.
- **CLI:** `al-explorer debug start|breakpoint|state|eval|continue|step|history|stop`;
  `al-explorer profile start|stop|analyze`; `al-explorer snapshot start|list|download`;
  `al-explorer init-debug` to scaffold `.zed/debug.json`.

## Limitations

- No pause, function breakpoints, set-variable, completions, restart, or step-back. Each request is
  rejected with a specific explanation, and every corresponding optional DAP capability remains false.
- Nested DAP variables use bounded adapter-owned `variablesReference` handles and lazy
  `ExpandNode` requests; MCP clients can also expand values explicitly by `path`.
- MCP exposes structured equivalents of the native runtime control and inspection loop rather than
  raw DAP request/event frames.
- `sessionId` and `breakOnNext` are forwarded to the BC `Attach` payload. Fields with no native
  behavior (`useMcpServerForDebugging`, `mcpServicePort`, `userId`,
  `useVsCodeAuthentication`, `primaryTenantDomain`, snapshot/profiling config) were **removed** from
  `debug_adapter_schemas/al.json` (with a `$comment` pointing to `al-explorer snapshot`/`profile`).
