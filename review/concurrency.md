# Concurrency Issues — Deadlocks, Race Conditions, Lock Contention

## CRITICAL

### CONC-C01: al-semantic — Mutex poisoning recovery allows unsound CLR state
- **File:** `crates/al-semantic/src/lib.rs:222-225`
- **Impact:** After a panic during an FFI call, `e.into_inner()` recovers the poisoned Mutex value. The CLR bridge may have unfreed buffers or be in an intermediate state. Subsequent calls risk use-after-free across FFI boundary.
- **Fix:** Return `SemanticError::HostInit("Bridge state corrupt after panic")` instead of recovering.

## HIGH

### CONC-H01: al-semantic — `timeout` does not cancel `spawn_blocking`; lock stays held
- **File:** `crates/al-semantic/src/lib.rs:219-228`
- **Impact:** `tokio::time::timeout` drops the future but `spawn_blocking` continues running. The `std::sync::Mutex` on `DotNetHost` remains locked for the full CLR call duration. All subsequent calls wait behind the timed-out call. Documented 30s timeout is a lie — callers get `Timeout` error but the lock is still held.
- **Fix:** Document limitation clearly; consider condvar-based cancellation flag.

### CONC-H02: al-semantic — `unsafe impl Sync for DotNetHost` with `&self` `call()`
- **File:** `crates/al-semantic/src/host.rs:36-37`
- **Impact:** `DotNetHost` is `pub` and `Sync`. `call(&self, ...)` takes shared reference. Any caller bypassing the Mutex can call `DotNetHost::call` concurrently, violating the serialization invariant.
- **Fix:** Make `call` take `&mut self`, or make `DotNetHost` `pub(crate)`.

### CONC-H03: al-dap-client — Session mutex held across full `invoke()` duration (up to 60s)
- **File:** `crates/al-dap-client/src/native_dap.rs:560-574, 642-645, 703-706, 737-750`
- **Impact:** Background event-forwarding task blocked by outer `session.lock()` for the entire `invoke()` duration. Break events delayed up to 60 seconds.
- **Fix:** Clone session Arc and drop the guard before awaiting `invoke()`.

### CONC-H04: al-dap-client — `setBreakpoints` holds mutex across N invoke() calls
- **File:** `crates/al-dap-client/src/native_dap.rs:560-637`
- **Impact:** With 10 breakpoints, mutex held for up to 600 seconds. All other DAP requests and event forwarding blocked. Zed may timeout.
- **Fix:** Process breakpoints with mutex released between each invoke, or batch into single BC call.

## MEDIUM

### CONC-M01: al-core — `get_or_build_call_graph` returns read guard; deadlock if caller then writes
- **File:** `crates/al-core/src/workspace.rs:163-218`
- **Impact:** Returns `(Arc<InsightGraph>, RwLockReadGuard<...>)`. If any caller holds this guard and calls `invalidate_insight_graph()` (write lock), deadlock on non-reentrant `std::sync::RwLock`.
- **Fix:** Document constraint on callers, or return `Arc<CallGraph>` (cloned) to avoid exposing guard.

### CONC-M02: al-daemon-client — `wait_for_daemon` probes socket then discards connection; TOCTOU race
- **File:** `crates/al-daemon-client/src/client.rs:172-180`
- **Impact:** Probe succeeds, daemon crashes, second `connect()` fails. First connection should be reused.
- **Fix:** Return the live `UnixStream` from `wait_for_daemon`.

### CONC-M03: al-daemon-client — `MAX_RESPONSE_LINE` check fires after allocation
- **File:** `crates/al-daemon-client/src/client.rs:138-155`
- **Impact:** `BufReader::read_line` allocates full buffer before 64MB guard triggers. Misbehaving daemon forces large allocation.
- **Fix:** Use `take(MAX_RESPONSE_LINE + 1)` around the reader.

### CONC-M04: al-daemon-client — JSON-RPC response ID not validated against request ID
- **File:** `crates/al-daemon-client/src/jsonrpc.rs:9-27`, `client.rs:137-155`
- **Impact:** Late/unsolicited responses silently accepted. Wrong data returned to caller.
- **Fix:** Validate `response.id == expected_id` in `read_response`.
