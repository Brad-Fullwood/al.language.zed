# BC SignalR Break-Event Subscription Model

**Date:** 2026-04-28
**Scope:** Phase 4 snapshot recording — waiting for BC Break events without polling
**Status:** Research / pre-implementation

---

## 1. Background

The Phase 4 snapshot recorder (`crates/al-core/src/test_snapshots/`) needs to:

1. Start a test run via the dev API REST endpoint.
2. Connect to the BC SignalR debug hub and set breakpoints.
3. **Wait** for the BC server to push a `Break` event when the test hits a breakpoint.
4. Call `GetVariables` / `ExpandNode` to capture state.
5. Call `SetBreakpointResponse` to resume execution.
6. Repeat until the test finishes.

Step 3 is the gap this document investigates.

---

## 2. SignalR Hub URL Pattern

**Source:** reverse-engineered from EditorServices.Protocol.dll; URL patterns confirmed by
`crates/al-core/src/dap/bc_debug.rs` (lines 173–185).

### Cloud (SaaS)

```
POST  https://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/dev/DebuggerHub/negotiate?negotiateVersion=1
WSS   wss://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/dev/DebuggerHub?id={connectionToken}
```

### On-Premises

```
POST  http://{server}:{port}/{serverInstance}/dev/DebuggerHub/negotiate?negotiateVersion=1
WSS   ws://{server}:{port}/{serverInstance}/dev/DebuggerHub?id={connectionToken}
```

Both patterns percent-encode the `connectionToken` returned by the `/negotiate` step.
The path suffix is always `/dev/DebuggerHub`.

---

## 3. Hub Method Inventory

### 3a. Client → Server invocations (type-1 messages with `invocationId`)

| Method | Arguments | Returns | Notes |
|--------|-----------|---------|-------|
| `Attach` | `{BreakOnError: bool, BreakOnRecordWrite: bool}` | — | Attach to BC debug session |
| `DebugAdapterConfigurationDone` | `debugOptions` object (newer BC ≥ 2.0); no args fallback for older | — | Signal config complete; BC starts execution |
| `AddBreakpoint` | `ApplicationObjectIdWrapper`, `SourcePosition`, `condition: string` | breakpoint JSON (contains `Id`) | `ObjectType` is integer enum (Newtonsoft.Json default) |
| `RemoveBreakpoint` | `breakpointId: long` | — | |
| `UpdateBreakpoint` | `id: long`, `condition: string` | — | |
| `SetBreakpointResponse` | `breakpointExitReason: int` | — | **Resume from break**: 0=continue, 1=step-over, 2=step-in, 3=step-out |
| `GetStackTrace` | — | `StackFrame[]` | Each frame: `{ApplicationObjectId, SourcePosition, DisplayName}` |
| `GetVariables` | `frameId: int` | `LocalNode[]` | Per-frame locals |
| `ExpandGlobals` | `frameId: int` | `LocalNode[]` | Global variables |
| `ExpandNode` | `frameId: int`, `path: string` | `LocalNode[]` | Expand a nested variable |
| `GetWatchNode` | `frameId: int`, `expression: string` | `LocalNode` | Evaluate expression |
| `GetSource` | `ApplicationObjectIdWrapper` | `string` | Decompile source |
| `TerminateSession` | — | — | Graceful end |
| `IsAlive` | — | — | Client-initiated ping (also server-initiated, see below) |

**Source:** `crates/al-core/src/dap/bc_debug.rs` lines 14–15 (doc comment) and individual
method implementations at lines 718–937.

Note: There is no `Pause`/`Break` method that the client can call to interrupt running
execution — see `native_dap.rs` line 774 (comment: "BC's SignalR debug hub does not expose
a 'pause while running' method").

### 3b. Server → Client push callbacks (type-1 messages without `invocationId`)

| Callback | Arguments | Meaning |
|----------|-----------|---------|
| `Break` | `ApplicationObjectIdWrapper`, `StackFrame[]`, `message: string` | Execution stopped (breakpoint hit, step complete, or exception) |
| `IsAlive` | — | Server-initiated keep-alive ping; client must respond with `AcknowledgeIsAlive` |
| `OnAttachedToConnection` | — | Confirms successful attach |
| `OnDetachedFromConnection` | `terminateSession: bool` | Session ended |
| `OnFatalDebuggerException` | `message: string` | Fatal server-side error |

**Source:** `crates/al-core/src/dap/bc_debug.rs` lines 600–663 (`handle_server_callback`).

The `Break` event is pushed by BC at any time that execution stops. It is not a response to
any client invocation. This is the **only** mechanism by which a client can know that
execution has stopped — BC does not poll-support any "am I stopped yet?" RPC.

---

## 4. Event Subscription Model: Server-Pushed, Not Client-Polled

The BC DebuggerHub uses **ASP.NET Core SignalR** (JSON hub protocol, record-separator
`\x1e` framing). The event model is entirely **server-pushed**:

- The server sends type-1 `Invocation` messages to the client **at any time** as callbacks.
- The client does not need to poll or re-subscribe; the WebSocket connection carries
  all events for the lifetime of the session.
- Specifically, `Break` events arrive on the same WebSocket as command responses. There
  is no separate event channel.

**Protocol framing (confirmed from source):**

```
// bc_debug.rs line 439: handshake
"{\"protocol\":\"json\",\"version\":1}\x1e"

// Each message is terminated by \x1e (ASCII record separator 0x1E)
// Type 1 = Invocation (both client→server with invocationId and server→client callbacks)
// Type 3 = Completion (server response to client invocation with matching invocationId)
// Type 6 = Ping (server keep-alive; can be ignored)
```

**Why true async notification is possible:**
Since SignalR uses a persistent WebSocket, the server `Break` message arrives as an
unsolicited type-1 message on the WebSocket. A Rust client that keeps a background reader
task alive can receive this message via `channel.recv().await` — a genuinely
blocking-until-data-arrives await, **not a poll**.

---

## 5. What the Existing DAP Module Already Does

### 5a. BcDebugSession (bc_debug.rs)

| Capability | Implementation | File:Line |
|------------|---------------|-----------|
| SignalR negotiate + WebSocket connect | `BcDebugSession::connect()` | bc_debug.rs:345 |
| JSON protocol handshake (`\x1e`) | `connect()` writer/reader tasks | bc_debug.rs:439–518 |
| Background reader task | Unbounded `mpsc` channel; all type-1 and type-3 messages forwarded | bc_debug.rs:475–518 |
| `Break` event recognized | `handle_server_callback()` match arm `"Break"` | bc_debug.rs:611 |
| `is_stopped` flag set on Break | `*self.is_stopped.lock().await = true` | bc_debug.rs:614 |
| Non-blocking drain of push events | `try_drain_push_events()` via `try_lock` | bc_debug.rs:695–713 |
| Buffering push events during invoke() | `pending_events: Mutex<VecDeque<SignalRMessage>>` | bc_debug.rs:339, 585–589 |
| Flushing buffered events after invoke | `flush_pending_events()` | bc_debug.rs:674–687 |
| `BcEvent::Break` variant | Public enum | bc_debug.rs:201 |

### 5b. BcDebugSessionAdapter (bc_debug_bridge.rs)

The `wait_for_break()` implementation in `BcDebugSessionAdapter` currently **polls**
`try_drain_push_events()` with a 100 ms sleep, up to 300 iterations (30 s total):

```rust
// bc_debug_bridge.rs lines 119–141
async fn wait_for_break(&self) -> Result<bool, ReplayerError> {
    for _ in 0..MAX_POLLS {
        let events = self.session.try_drain_push_events().await;
        for event in events {
            match event {
                BcEvent::Break { .. } => return Ok(true),
                BcEvent::Detached { .. } | BcEvent::FatalError { .. } => return Ok(false),
                BcEvent::Other { .. } => {}
            }
        }
        tokio::time::sleep(POLL_DELAY).await;
    }
    Err(ReplayerError::NotYetWired(...))
}
```

This is documented as a known limitation in both the module doc comment
(`bc_debug_bridge.rs` lines 24–28) and the `wait_for_break` rustdoc (lines 109–116):

> "TODO: Replace the polling loop once `BcDebugSession::wait_break_notify()` is implemented."

The `TODO` appears in two explicit form: `bc_debug_bridge.rs:25` and `:115`.

---

## 6. Why Polling Is Unnecessary — the Notify Pattern

`BcDebugSession` already has an unbounded `mpsc::UnboundedReceiver<SignalRMessage>` wired
from the WebSocket reader task. The issue is that `invoke()` holds
`event_rx: Mutex<UnboundedReceiver<SignalRMessage>>` for its entire duration, so
`wait_for_break` cannot call `rx.recv().await` concurrently without deadlocking or
starving invocations.

The fix is to separate the break-notification path from the main event channel using
`tokio::sync::Notify`. The WebSocket reader task (or `handle_server_callback`) calls
`notify.notify_one()` whenever a `Break` event arrives. The `wait_for_break` function
awaits `notify.notified()` — which is a true async await, not a poll.

---

## 7. Proposed `wait_for_break()` Design

### 7a. Change to `BcDebugSession`

Add a `break_notify: Arc<Notify>` field to `BcDebugSession` (or expose a
`subscribe_break()` method that returns a cloneable `Arc<Notify>`).

The existing reader task at `bc_debug.rs:475` already dispatches to
`handle_server_callback`. The `"Break"` arm at line 611 sets `is_stopped = true`. It
should also call `self.break_notify.notify_one()`.

No locking is needed — `Notify::notify_one()` is lock-free and can be called from within
the reader task without the `event_rx` mutex.

### 7b. `wait_for_break` signature

```rust
/// Wait until the next BC Break event (or session end) arrives.
///
/// Returns `true` if execution stopped (a Break event arrived).
/// Returns `false` if the session ended (`OnDetachedFromConnection` or
/// `OnFatalDebuggerException` was received first).
///
/// This is a true async wait backed by `tokio::sync::Notify`; it does not poll.
/// Timeout is the caller's responsibility (wrap with `tokio::time::timeout`).
pub async fn wait_for_break(session: &Arc<BcDebugSession>) -> Result<bool, DapError>;
```

The function awaits `session.break_notify.notified()` and then checks `session.is_stopped()`
or a new `session.last_event()` accessor to distinguish Break from Detach/FatalError.

Because `Notify::notified()` does not consume `event_rx`, there is no mutex contention
with concurrent `invoke()` calls. Both can proceed independently.

### 7c. Integration into `BcDebugSessionAdapter`

Replace the polling loop in `bc_debug_bridge.rs:wait_for_break()` with a call to
`wait_for_break(session)`. Apply a per-test timeout (e.g. 30 s) using
`tokio::time::timeout`.

### 7d. Sketch

```rust
// In BcDebugSession (bc_debug.rs)
use std::sync::Arc;
use tokio::sync::Notify;

pub struct BcDebugSession {
    // ... existing fields ...
    /// Fired by handle_server_callback whenever a Break event arrives.
    break_notify: Arc<Notify>,
    /// Set true on Break, false on Detach/FatalError — written by handle_server_callback.
    last_stop_was_break: std::sync::atomic::AtomicBool,
}

// In handle_server_callback, "Break" arm:
"Break" => {
    *self.is_stopped.lock().await = true;
    self.last_stop_was_break.store(true, Ordering::Release);
    self.break_notify.notify_one();
}
"OnDetachedFromConnection" | "OnFatalDebuggerException" => {
    self.last_stop_was_break.store(false, Ordering::Release);
    self.break_notify.notify_one(); // wake any waiter so it can detect end-of-session
}

// Free function (or method) in bc_debug.rs:
pub async fn wait_for_break_notify(
    session: &BcDebugSession,
    timeout: Duration,
) -> Result<bool, DapError> {
    tokio::time::timeout(timeout, session.break_notify.notified())
        .await
        .map_err(|_| DapError::Timeout(timeout))?;
    Ok(session.last_stop_was_break.load(Ordering::Acquire))
}
```

No mutex is held across the `.await`, so there is no deadlock risk with `invoke()`.

---

## 8. Open Questions / Unknowns

| # | Question | Status |
|---|----------|--------|
| 1 | **Break event argument structure**: Are the `ApplicationObjectIdWrapper` and `StackFrame[]` arguments present in all BC versions, or only newer ones? The current code only reads `args[2]` (message string) and ignores args[0]/[1]. | Not verified against live BC |
| 2 | **Break reason discrimination**: Is there a reliable way to distinguish "breakpoint hit" vs "step complete" vs "error break" from the Break event arguments, rather than always reporting `"breakpoint"` as the reason? | Not verified |
| 3 | **Test-runner + debugger co-existence**: Can the dev API test run endpoint (`POST /dev/tests/{id}/run`) be invoked while a SignalR debug session is already attached? Or does attaching a debug session block the test run? | Not verified against live BC |
| 4 | **Cloud SaaS timing**: On cloud BC, the debug session attach may have a shorter idle timeout than on-prem. The current 30-minute idle shutdown in daemon mode may interact. | Not verified |
| 5 | **`OpenConnectionAsync` method**: The module doc comment at `bc_debug.rs:13` lists `OpenConnectionAsync` but the actual `attach()` implementation calls `Attach` (line 723). Whether `OpenConnectionAsync` is a prerequisite or alias for `Attach` is unclear. | Not verified |
| 6 | **`Notify` lost-notification race**: If BC sends `Break` between `configuration_done()` returning and `wait_for_break_notify()` being awaited, `Notify::notified()` might miss it. Fix: call `notified()` to get the future **before** `configuration_done()`, then `.await` it after. This is the standard `Notify` pattern. | Design-time concern; must be addressed in implementation |
| 7 | **Multiple concurrent Break events**: If BC fires two Break events before the client calls `SetBreakpointResponse`, does it queue them or coalesce them? `notify_one()` coalesces — consider `Semaphore` or counting if that matters. | Not verified |

---

## 9. Summary

- BC SignalR debug events are **server-pushed** over the persistent WebSocket; no
  client-side polling of the protocol is required.
- The existing `BcDebugSession` already has an unbounded receiver that captures all events
  including `Break`. The `"Break"` callback is recognized at `bc_debug.rs:611`.
- The current `wait_for_break` in `BcDebugSessionAdapter` (`bc_debug_bridge.rs:119`) polls
  with a 100 ms sleep because `invoke()` holds `event_rx`. The TODO is already documented
  in the source.
- The canonical fix is to add a `tokio::sync::Notify` field to `BcDebugSession`, fire it
  from `handle_server_callback` on every `Break` (and session-end) event, and replace the
  polling loop with a single `notified().await` — eliminating all polling overhead.
- No public Microsoft documentation for the `DebuggerHub` SignalR protocol exists; all
  method names and types in this document are sourced from the existing reverse-engineered
  implementation in `crates/al-core/src/dap/bc_debug.rs`.

---

## References

| Source | Location |
|--------|----------|
| Hub URL construction | `crates/al-core/src/dap/bc_debug.rs:149–185` |
| Full hub method inventory (doc comment) | `crates/al-core/src/dap/bc_debug.rs:13–15` |
| Server callback handler | `crates/al-core/src/dap/bc_debug.rs:608–663` |
| `BcEvent` enum | `crates/al-core/src/dap/bc_debug.rs:196–208` |
| Pending-event buffering | `crates/al-core/src/dap/bc_debug.rs:339, 574–599` |
| `try_drain_push_events` | `crates/al-core/src/dap/bc_debug.rs:695–713` |
| Polling `wait_for_break` + TODO | `crates/al-core/src/test_snapshots/bc_debug_bridge.rs:109–141` |
| TODO design gap (module doc) | `crates/al-core/src/test_snapshots/bc_debug_bridge.rs:24–28` |
| `DebuggerSession` trait | `crates/al-core/src/test_snapshots/replayer.rs:64–79` |
| Pause not supported note | `crates/al-core/src/dap/native_dap.rs:773–793` |
| `tokio::sync::Notify` docs | https://docs.rs/tokio/latest/tokio/sync/struct.Notify.html |
| BC AL debugging docs (general) | https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/devenv-debugging |
