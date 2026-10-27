# Debugging (DAP) & Business Central Runtime

**Modules:** `crates/al-dap/src/dap/`, `crates/al-dap/src/native_debug.rs`, and
`crates/al-bc/src/` (`bc_client.rs`, `http_auth.rs`, `launch.rs`, `profiling.rs`, `snapshot/`) +
`crates/al-lsp/src/server/daemon/debug_dispatch.rs` (CLI/MCP control plane) + `src/dap.rs` (Zed
glue) + `debug_adapter_schemas/al.json`.
**Status:** ✅ shipped (core flow). Every field advertised by the native schema is
consumed by Zed or the native adapter

The debugger is a **native Rust Debug Adapter Protocol (DAP) server** that talks directly to a
Business Central server over SignalR + REST, with no Microsoft `EditorServices.Host` (it remains
available as a legacy fallback). Zed launches `al-lsp --dap`. The adapter selects the shared verified
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
Zed ─────── DAP/stdio ──────► native_dap/ ───────────► BC REST + SignalR
                                                         (/dev/apps, /dev/DebuggerHub)
MCP client ── al_debug ─────► debug_dispatch.rs
                                  │
                                  └── persistent NativeDebugSession ──► BC SignalR
                                                                      (/dev/DebuggerHub)
```

| Layer | File | Role |
| --- | --- | --- |
| Native DAP server | `dap/native_dap/` | the Zed-facing stdio DAP server. Compile→publish→connect→drive |
| DAP wire types & framing | `dap/protocol.rs`, `dap/framing.rs` | `Content-Length` framing (8 KiB header cap, 20 MB body cap), `seq` patching |
| DAP subprocess client | `dap/client.rs` | (legacy path) spawn + route a DAP subprocess by `request_seq` |
| BC debug session | `dap/bc_debug/` | SignalR (WebSocket) client to `/dev/DebuggerHub` |
| Config | `dap/config.rs` | parse launch config (on-prem vs cloud, auth) |
| Launch config reading | `al-bc/src/launch.rs`, `al-types/src/jsonc.rs` | read `launch.json` with comments and trailing commas |
| High-level session | `native_debug.rs` | wrap session, breakpoint registry, 10k-entry hit history |
| MCP control plane | `server/daemon/debug_dispatch.rs` | stateful structured commands shared by CLI and MCP |
| MCP tool | `server/mcp/mod.rs` (`al_debug`) | schema mapped to daemon method `debug` |
| BC REST | `bc_client.rs` | publish `.app`, RAD delta, status (size caps + secret redaction) |
| TLS helper | `http_auth.rs` | client builder. Warns loudly when cert validation is disabled |
| Profiling | `profiling.rs` | start/stop CPU profiling, parse `.alcpuprofile` hotspots |
| Snapshots | `snapshot/` | snapshot debugger zip reader, session store and saved zips |

## DAP requests implemented

`initialize`, `configurationDone`, `launch`, `attach`, `setBreakpoints` (incl. **conditional**),
`continue`, `next`, `stepIn`, `stepOut`, `threads` (single "AL Thread"), `stackTrace`, `scopes`
(Locals + Globals), `variables`, `evaluate` (watch), `disconnect`, `terminate`. `pause` returns an
explicit error, because **BC's debug hub has no pause-while-running API**. Requests for
`setFunctionBreakpoints`, `setVariable`, `completions`, `restart`, and `stepBack` also receive
command-specific failure responses. They are never silently acknowledged. Events emitted: `initialized`,
`stopped`, `output`, `al/openUri` (browser launch), `terminated`.

Advertised capabilities include conditional breakpoints, evaluate-for-hovers, terminate, and delayed
stack-trace loading (`stackTrace`'s `startFrame`/`levels` are honored: the response pages the real
stack instead of always returning every frame from 0). Function breakpoints, set-variable, completions,
restart/restart-frame, and step-back remain explicitly `false`.

`setBreakpoints` calls that arrive before `launch`/`attach` (Zed sends these during the DAP
configuration phase, right after `initialized`) are queued per source rather than answered as
permanently failed: the response reports `verified: false` with a "not started yet" message, and once
the session starts each queued breakpoint is resolved/added on BC and a `breakpoint` event updates its
verification (matched by `source.path` + `line`, since no BC id exists until then).

That capability boundary is based on the installed Microsoft AL 17.0.2273547 EditorServices protocol
assembly and its live `HubBasedDebuggerService` contract, not on DAP type names alone. Its protocol
metadata contains the stack/variables/watch request family used here (`GetVariablesAsync`,
`GetWatchNodeAsync`, and stack-frame conversion), while the reference adapter's completions
request is an editor-workspace/LSP round trip rather than a Business Central hub operation. Its
initialize result also advertises restart even though that assembly registers no restart request
handler and exposes no live restart hub method. The native adapter therefore does not advertise
either feature until it can provide the full behavior itself.

## How launch/attach works

**Launch** (`native_dap/`):
1. **Compile** through the shared build service: verified native by default, or `alc` only when
   `al.useOfficialCompiler` is explicitly enabled in persisted project settings.
2. **Authenticate** (OAuth callback / env token).
3. **Publish** the `.app` to BC (`POST …/dev/apps`). A missing `.app` fails the launch with a clear
   error rather than silently debugging a stale build.
   The `tenant` must be the tenant GUID and listed in the tenant registry, see
   [project trust](project-trust.md#tenant-registry-and---tenant). `tenantPurpose` names the issue for a customer tenant.
4. **Connect** SignalR to `/dev/DebuggerHub`. Launch makes no `Attach` call: Microsoft's launch handler passes
   `attachOptions = null` (`DebugAdapterLaunchRequestHandlerBase.cs:89`) and the `debuggingcontext` and `sk`
   values of the browser URL bind the session (`HubBasedDebuggerService.cs:245-248`).
5. Optionally **open the browser** at the debug-context URL when `launchBrowser` is set.
6. When the hub reports `OnAttachedToConnection`, send the DAP `initialized` event
   (`DebuggerService.cs:119-122`). The client then sends its breakpoints and `configurationDone`.
   `breakOnError` and `breakOnRecordWrite` travel in `DebugAdapterConfigurationDone`, not in an attach.

**Attach** skips compile/publish and connects to an existing session. It calls the hub's `Attach` with
`breakOnNextClient`, `sessionId` and `userId`, then sends `breakOnError`/`breakOnRecordWrite` in
`ConfigurationDone` like launch.

## BC SignalR integration (`bc_debug/`)

The hub URL follows Microsoft's adapter. In the cloud it is
`https://api.businesscentral.dynamics.com/v2.0/{environment}/dev/DebuggerHub` with no tenant segment
(`CloudHttpClientFactory.cs:137-158`, `DebugAdapterStartRequestHandler.cs:133-137`), on premises
`{server}:{port}/{instance}/dev/DebuggerHub`. The query parameters are those of
`HubBasedDebuggerService.OpenConnectionAsync` (`HubBasedDebuggerService.cs:176-200`): `tenant` (on premises
only), `sessionId` (when set and not -1) and `userid` (both only for an attach, they come from the attach options),
and `Authentication=Bearer <token>`. Negotiate and the WebSocket both carry them, and the affinity cookie
that negotiate sets is sent on the WebSocket (`HubBasedService.cs:57-58`). Not sent: `deploymentId` (no
configuration field), application family and sovereign cloud hosts. Live check on CDX pending in the lead's contract run.

A from-scratch SignalR (protocol v1) client: negotiates (`/negotiate?negotiateVersion=1`, validating
the negotiated version and `connectionToken`), opens a WebSocket with the record-separator handshake,
and runs reader/writer tasks. Every hub request uses the C# property names (PascalCase, for example
`BreakOnNextClient`, `ObjectNumber`, `Line`) and integer enums, because Microsoft's client registers Newtonsoft
with `DefaultContractResolver` (`AspNetCoreSignalRFactory.cs:24-27`). A test compares the serialised frame bytes
with frames built from the decompiled types. Hub methods invoked are a subset of the ones in `HubBasedDebuggerService.cs:34-79`. Invoked: `Attach`,
`DebugAdapterConfigurationDone`, `AddBreakpoint`/`RemoveBreakpoint`/`UpdateBreakpoint`,
`GetVariables`/`ExpandNode`/`ExpandGlobals`/`GetWatchNode`, `SetBreakpointResponse`
(0=continue, 1=step-over, 2=step-in, 3=step-out), `StopDebugging`. Server
Not invoked: `AddBreakpointByName` and `AddBreakpointByNameAndNamespace` (Microsoft uses them for interface
objects only), `GetSource` and `GetSourceContent`, `GetNstSessionInfo`. There is no client call named
`GetStackTrace`, `TerminateSession` or `IsAlive`. Server
callbacks handled: `Break` (→ `stopped`), `IsAlive` (answered with the `AcknowledgeIsAlive` hub call), `OnAttachedToConnection`,
`OnDetachedFromConnection` (→ `terminated`), `OnFatalDebuggerException` (→ `output`).
`Break`'s stop reason is derived rather than hard-coded: a non-empty message argument means BC broke on a runtime
error (`stopped.reason = "exception"`, with the message surfaced in `stopped.text`). Otherwise the
reason is `"step"` when the last client action was step over/in/out, and `"breakpoint"` otherwise.
`stackTrace` answers from the `StackFrame[]` of the last `Break` callback, kept in the session, as
Microsoft's client does (`DebuggerService.SetExecutionState`). Microsoft has no `GetStackTrace` hub
method. A `StackFrame` has `MethodName`, `ObjectId` (`ObjectType`, `ObjectNumber`), `ObjectName` and
`StatementSpan` (`From`, `To`, each with `Line` and `Column`) and nothing else, and only those are
read. Lines and columns follow Microsoft's `ConvertClientLineToDebuggerLine` (DebugHelper.cs:105-112):
add 1 when the client sent `linesStartAt1` (or `columnsStartAt1`) and the value is above 0. Breakpoint
lines go the other way with `Position.Create` (subtract 1). In a file that declares two or more objects BC counts
lines inside each object's own compilation unit (the `using`/`namespace` header, then the object with its leading
comments), so the line also moves by an object offset: `DocumentHelper.cs:25-66` adds it to breakpoint lines
and `DebugAdapterStackTraceHandler.cs:80-101` takes it off stack frame lines. The offset is computed by
`object_line_shift` from `GetDebuggerLineOffsetForNode` (CodeAnalysis `SyntaxNodeExtensions`). A one-object file
has offset 0. Live check on CDX pending in the lead's contract run: break on a line in the second object. The MCP path treats its clients as
one-based.

**Debugger version.** `connect` reads `debuggerVersion` from `GET dev/metadata` (`ServerInfo.cs:11-12`,
`ServerInfoApiClient.cs:27-29`) and keeps its major part, as Microsoft does
(`DebugAdapterStartRequestHandler.cs:69-72`). It selects the call shape, with no second try: `Attach` is refused
below major 3 (`DebugAdapterAttachRequestHandler.cs:33-36`), `DebugAdapterConfigurationDone` has no
arguments up to major 1 and sends `DebugOptions` after (`HubBasedDebuggerService.cs:264-269`), `GetWatchNode`
has the `WatchOption` argument from major 4 (`:349-353`), and `DebugOptions` carries the legacy
`BreakOnError`/`BreakOnRecordWrite` booleans below major 5 and only the behaviour values from major 5
(`DebugAdapterStartRequestHandler.cs:69-119`). `SkipSystemTriggers` is always sent as true, the
`DebugOptions` default, and has no launch.json field. A metadata body without a parsable version uses the newest
shapes with a warning. Live check on CDX pending in the lead's contract run.

### Engineering details worth knowing

- **Breakpoint serialization:** the breakpoint mutex is held across remove→add→store so
  concurrent `setBreakpoints` can't orphan BC breakpoints. Each breakpoint resolves to (ObjectType,
  ObjectId) from its own line, the last object declared at or above it, via the workspace index, so
  a file that declares more than one object sets every breakpoint on the right one. A
  `setBreakpoints` call clears a file's previously tracked breakpoints even when none of its
  requested lines resolve to an object.
- **Per-operation timeouts:** `StopDebugging` 10 s,
  breakpoints and step/continue (`SetBreakpointResponse`) 30 s, stack and variables 30 s,
  `Attach`/`DebugAdapterConfigurationDone` 120 s, any other hub method 60 s.
- **Lock discipline:** clone the session `Arc` inside the lock, drop the lock, then await.
- **Bounded event channel** (4096 messages, overflow dropped with a warning) with a dedicated
  unbounded channel for `Break` events so a paused breakpoint is never dropped under back-pressure.
  Completion replies to `invoke` have their own 32-entry channel.
- **`configurationDone` order:** BC rejects `DebugAdapterConfigurationDone` before the hub reports
  `OnAttachedToConnection`. The adapter therefore sends `initialized` only after that callback, as
  Microsoft's does (`DebuggerService.cs:119-122`), and calls `configurationDone` once. It has no retry
  and no second call without arguments. A rejection is an error response to the client's request. The
  MCP path (`NativeDebugSession`) sends the call once after the callback and reports a rejection as an
  error on every later command.
- **Security:** REST size caps (500 MB upload / 16 MB JSON / 500 MB binary), error-body redaction of
  bearer tokens/passwords/secrets, and a loud warning whenever TLS cert validation is disabled.

## Profiling & snapshots

- **Profiling (`profiling.rs`):** start/stop CPU profiling against the BC dev endpoint, download an
  `.alcpuprofile` (Chrome DevTools format), and analyze hotspots using sampled `timeDeltas`, with
  hit counts as a fallback when timing data is absent. See also
  [analysis-and-insight](./analysis-and-insight.md) for mapping hotspots to source.
- **Snapshots (`snapshot/`):** the snapshot debugger zip reader, the session store that remembers a
  started session between commands, and `save_finished_snapshot`. The REST calls (attach, status,
  finish) are in `al-dap` (`bc_debug/snapshot_debugger.rs`).
- **Snapshot capture and replay:** an `attach` with `"snapshot": "capture"` records a snapshot
  instead of debugging, and a `launch` with `snapshotFileName` replays a zip. The CLI is
  `al-explorer snapshot start|status|finish|list|show` and the MCP tool is `al_snapshot`. See
  [snapshot-debugging](./snapshot-debugging.md). This is Business Central's snapshot debugger, not
  the `test-snapshot` feature in [native-test-runtime](./native-test-runtime.md).

## Zed integration

`src/dap.rs` builds the adapter command: default `--dap` (native), `--dap-legacy` for the Microsoft
proxy via `al.useOfficialDap`. Debug configs are authored with the bundled snippets and validated
against `debug_adapter_schemas/al.json`, which exposes the native adapter's consumed launch fields
(authentication, breakOnError/Next/RecordWrite, environmentType/Name, tenant,
server/serverInstance/port, launchBrowser, startupObjectType/Id, schemaUpdateMode,
dependencyPublishingOption, and validateServerCertificate) and the snapshot fields (`snapshot`,
`userId`, `snapshotVerbosity`, `snapshotOutputPath`, `snapshotFileName`, `snapshotPort`,
`allowProduction`, see [snapshot-debugging](./snapshot-debugging.md)). Zed consumes the schema's optional
`build` field when a user explicitly supplies one. Generated launch scenarios leave it empty:
the adapter's launch request already compiles through the shared build service and selects the
manifest-derived artifact atomically, while attach intentionally performs no build.

## MCP debug control

`al_debug` maps directly to daemon method `debug`. The MCP server owns one `Workspace` for its
lifetime, and that workspace retains the `NativeDebugSession`, so separate `tools/call` requests are
one continuous debugging session rather than isolated commands.

| `cmd` | Relevant parameters | Result/effect |
| --- | --- | --- |
| `start` | `config?`, `accessToken?`, or inline BC connection fields | Connect using the named project debug configuration (or the first AL configuration) and retain the session. Callers can instead supply `tenant`/`environmentName` for BC online or `server`/`serverInstance` for on-prem, plus authentication and attach selectors. |
| `breakpoint` | `file`, `line`, `condition?`, `objectType?`, `objectId?` | Replace the breakpoints for that file and return BC verification details. Object metadata is normally resolved from the workspace index. |
| `state` | | Return running/paused status, current location, variables, session ID, and thread ID. Calling it also drains pending BC break events. |
| `stack` | | Return the complete BC call-stack payload for the current stop, enriched with a zero-based `frameId` for follow-up inspection. |
| `variables` | `frameId?` | Return parsed locals for a stack frame (frame 0 by default). |
| `globals` | `frameId?` | Return parsed globals for a stack frame. |
| `expand` | `path`, `frameId?` | Expand a structured variable path in a stack frame. |
| `eval` | `expr`, `frameId?` | Evaluate an AL watch expression in the selected paused frame. |
| `continue` | | Resume execution. |
| `step` | `stepType: over\|in\|out` | Resume with the selected BC step mode. |
| `history` | `var?` | Return the bounded breakpoint-hit history, optionally filtered by variable name. |
| `stop` | | Stop and terminate the BC debug session and clear it from the MCP workspace. |

A typical client loop is `start` → `breakpoint` → poll `state` until paused → inspect `stack`
and `variables`/`globals` → `eval` and/or `step` → `continue` → `stop`. All results use MCP's normal
structured tool response and daemon errors are returned with `isError: true`, so clients do not need
to scrape terminal output.

For BC online and AAD targets, `accessToken` is optional. When it is absent, `al_debug start` uses the
same keyring-backed OAuth cache and refresh flow as `al-explorer authenticate login`. Clients do not
need to retrieve, print, or relay a bearer token. An explicit token remains available for headless
automation. `start` is launch-style and makes no `Attach` call. The adapter sends
`DebugAdapterConfigurationDone` once after BC reports `OnAttachedToConnection`. A rejection is
fatal for the session: `al_debug` reports it as an error on every later command.

This is a structured MCP control plane, not a second wire-level DAP endpoint. Zed's `launch`
request owns the compile-and-publish convenience flow. `al_debug start` attaches to the configured
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
can't. AL still *executes* on the BC runtime. The adapter controls and inspects that runtime and does
not reimplement it.

The service-backed contract is `make live-bc-contracts`. It drives the real
DAP framing and BC service through launch compilation, publication, attach,
breakpoint verification, stack/scopes/locals, evaluation, step, continue, and
disconnect. The profile requires explicit AAD configuration, test identity,
breakpoint, evaluation expression, BC version, and bearer token. The default
repository fixture supplies the test/breakpoint details from tenant and
environment inputs. Missing external inputs exit as `UNAVAILABLE`, never passed.

## How to use

- **In Zed:** create a debug config from the snippets, then launch/attach from the debugger UI.
- **From an MCP client:** connect to **AL Tools**, then call `al_debug` with `cmd: "start"` and continue
  with `breakpoint`, `state`, `stack`, `variables`/`globals`/`expand`, `eval`, `continue`/`step`, and
  `stop` calls in the same MCP process.
- **CLI:** `al-explorer debug start|breakpoint|state|eval|continue|step|history|stop`.
  `al-explorer profile start|stop|analyze`. `al-explorer snapshot start|status|finish|list|show`
  ([snapshot-debugging](./snapshot-debugging.md)). `al-explorer init-debug` to scaffold `.zed/debug.json`.

## Limitations

- No pause, function breakpoints, set-variable, completions, restart, or step-back. Each request is
  rejected with a specific explanation, and every corresponding optional DAP capability remains false.
- Nested DAP variables use bounded adapter-owned `variablesReference` handles and lazy
  `ExpandNode` requests. MCP clients can also expand values explicitly by `path`.
- MCP exposes structured equivalents of the native runtime control and inspection loop rather than
  raw DAP request/event frames.
- `sessionId`, `breakOnNext` and `userId` are forwarded to the BC `Attach` payload (`userId` as the
  string `AttachOptions.UserId`). An attach also sends `userid` and `sessionId` on the hub URL (see above). `Agent` maps to the
  decompiled `ClientService` value (3). Fields with no native
  behavior (`useMcpServerForDebugging`, `mcpServicePort`,
  `useVsCodeAuthentication`, `primaryTenantDomain`, profiling config) were **removed** from
  `debug_adapter_schemas/al.json`. The snapshot fields are back, because the native adapter now
  consumes them.
  When a launch config omits `breakOnNext`, attach defaults to `WebServiceClient`, the
  schema's documented default.
