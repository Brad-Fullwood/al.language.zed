# T404: Agentic Debugger — Design Spec

## Goal

Expose headless DAP control through the al-lsp daemon so CLI and MCP can drive AL debugging without a UI. All debug logic lives in a new `al-dap-client` crate.

## Architecture

### New Crate: `al-dap-client`

Analysis-level library. All debug logic lives here. This is analogous to `al-semantic` (which also manages a subprocess — the .NET CLR bridge). Both are specialized runtime engines that al-core orchestrates.

```
al-dap-client -> al-protocol (types only: AlToolchain, BcServerConfig)
al-core -> al-dap-client (stores session in Workspace)
al-lsp -> al-core (daemon dispatches to session)
al-cli -> al-protocol (thin JSON-RPC client, as always)
```

### Crate Internal Structure

```
crates/al-dap-client/
  Cargo.toml          # deps: al-protocol, tokio, serde, serde_json, tracing
  src/
    lib.rs            # pub mods, re-exports
    protocol.rs       # DAP message types: DapMessage, DapRequest, DapResponse, DapEvent
    framing.rs        # Content-Length read/write, ensure_seq() patch
    client.rs         # DapClient: spawn subprocess, send request, read response, background event reader
    session.rs        # DebugSession: full AL debug lifecycle
    editor_services.rs # find EditorServices.Host binary (moved from al-lsp::dap)
    types.rs          # AL result types: DebugState, StackFrame, Variable, BreakpointInfo, EvalResult, BreakpointHit
```

### What Each Module Does

**protocol.rs** — DAP wire types:
```rust
pub struct DapMessage { pub seq: i64, pub type_: String, ... }
pub struct DapRequest { pub seq: i64, pub command: String, pub arguments: Option<Value> }
pub struct DapResponse { pub request_seq: i64, pub success: bool, pub command: String, pub body: Option<Value> }
pub struct DapEvent { pub event: String, pub body: Option<Value> }
```

**framing.rs** — DAP wire format (`Content-Length: N\r\n\r\n<body>`):
- `read_dap_message(reader) -> Result<DapMessage>` — reads one DAP frame
- `write_dap_frame(writer, body) -> Result<()>` — writes one DAP frame
- `ensure_seq(body, counter) -> Vec<u8>` — patches missing `seq` field (EditorServices bug)

**client.rs** — Low-level DAP client:
```rust
pub struct DapClient {
    child: tokio::process::Child,
    stdin: tokio::io::BufWriter<ChildStdin>,
    seq_counter: AtomicI64,
    events: tokio::sync::mpsc::UnboundedReceiver<DapEvent>,
    event_task: tokio::task::JoinHandle<()>,
}

impl DapClient {
    pub fn spawn(binary: &Path, args: &[&str]) -> Result<Self>;
    pub async fn send_request(&mut self, command: &str, arguments: Option<Value>) -> Result<DapResponse>;
    pub fn drain_events(&mut self) -> Vec<DapEvent>;
    pub async fn next_event(&mut self) -> Option<DapEvent>;
    pub async fn wait_for_event(&mut self, event_name: &str, timeout: Duration) -> Result<DapEvent>;
    pub async fn kill(&mut self) -> Result<()>;
}
```

The constructor spawns a background tokio task that reads from the child's stdout, parses DAP frames, patches `seq`, and sends `DapEvent`s through an `mpsc` channel. Responses to requests are matched by `request_seq` and returned directly from `send_request`.

**editor_services.rs** — EditorServices.Host discovery (moved from al-lsp::dap):
```rust
pub fn find_editor_services(toolchain: &AlToolchain) -> Result<PathBuf, DapError>;
```

Search order (unchanged):
1. `$AL_EDITOR_SERVICES_PATH` env var
2. Next to `alc.dll` in toolchain
3. `~/.cache/al-lsp/editor-services/`
4. VS Code/Cursor/VSCodium extension directories

**types.rs** — AL-specific result types:
```rust
pub struct DebugState {
    pub status: SessionStatus, // Running, Paused, Stopped
    pub session_id: String,
    pub location: Option<Location>,
    pub stack: Vec<StackFrame>,
    pub variables: Vec<Variable>,
    pub thread_id: Option<i64>,
}

pub enum SessionStatus { Compiling, Running, Paused, Stopped }

pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub source: Option<String>,
    pub line: u32,
    pub column: u32,
}

pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    pub fields: Vec<Variable>, // Record expansion (1 level)
}

pub struct BreakpointInfo {
    pub id: i64,
    pub file: String,
    pub line: u32,
    pub condition: Option<String>,
    pub verified: bool,
}

pub struct EvalResult {
    pub result: String,
    pub type_name: String,
}

pub struct Location {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub procedure: Option<String>,
}

pub struct BreakpointHit {
    pub seq: u32,
    pub breakpoint_id: i64,
    pub timestamp: String,
    pub location: Location,
    pub variables: Vec<Variable>,
}
```

All types derive `Debug, Clone, Serialize, Deserialize` with `#[serde(rename_all = "camelCase")]`.

**session.rs** — The AL debug engine:
```rust
pub struct DebugSession {
    client: DapClient,
    state: DebugState,
    breakpoints: HashMap<String, Vec<BreakpointInfo>>, // file -> breakpoints
    history: Vec<BreakpointHit>, // filled by T405
    toolchain: AlToolchain,
    project_root: PathBuf,
    launch_config: BcServerConfig,
}

impl DebugSession {
    /// Compile project, spawn EditorServices.Host, initialize DAP, launch debug session.
    pub async fn start(
        toolchain: &AlToolchain,
        project_root: &Path,
        config: Option<&str>, // named config from launch.json
    ) -> Result<Self, DapError>;

    /// Set breakpoints for a file (replaces previous breakpoints for that file).
    pub async fn set_breakpoints(
        &mut self,
        file: &str,
        breakpoints: &[(u32, Option<&str>)], // (line, condition)
    ) -> Result<Vec<BreakpointInfo>, DapError>;

    /// Get current debug state (cached, refreshed if paused).
    pub async fn state(&mut self) -> Result<&DebugState, DapError>;

    /// Evaluate an expression at the current frame.
    pub async fn eval(&mut self, expr: &str) -> Result<EvalResult, DapError>;

    /// Continue execution until next breakpoint.
    pub async fn continue_(&mut self) -> Result<&DebugState, DapError>;

    /// Step (over, into, out).
    pub async fn step(&mut self, step_type: &str) -> Result<&DebugState, DapError>;

    /// Get breakpoint hit history.
    pub fn history(&self, var_filter: Option<&str>) -> Vec<&BreakpointHit>;

    /// Disconnect and kill EditorServices.Host.
    pub async fn stop(&mut self) -> Result<(), DapError>;
}

impl Drop for DebugSession {
    fn drop(&mut self) {
        // Kill child process to prevent orphans
    }
}
```

### `start()` Sequence

1. Resolve launch config from `.zed/debug.json` or `.vscode/launch.json` (using `al_protocol::launch::find_launch_config`)
2. Compile project: `dotnet <alc.dll> /project:<root> /out:<root> /packagecachepath:<packages>`
3. If compilation fails → return `DapError::CompilationFailed`
4. Find EditorServices.Host binary
5. Spawn via `DapClient::spawn(host_path, &["/startDebugging", &format!("/projectRoot:{}", root)])`
6. Send `initialize` request with capabilities
7. Wait for `initialized` event
8. Send `configurationDone`
9. Send `launch` request with server URL, auth, patched args from launch config
10. Set state to `Running`

### State Refresh on `state()`

When called:
1. Drain pending events from the background reader
2. If a `stopped` event was received, update state to `Paused` with reason and thread ID
3. If paused: send `threads` → `stackTrace` → `scopes` → `variables` requests
4. For each variable with `variablesReference > 0` (Records), send another `variables` request (1 level expansion)
5. Cache and return the updated `DebugState`

### Error Type

```rust
pub enum DapError {
    Io(std::io::Error),
    EditorServicesNotFound(String),
    SpawnFailed(String),
    CompilationFailed(String),
    DapProtocolError { command: String, message: String },
    SessionNotPaused,
    NoActiveSession,
    Timeout(Duration),
}
```

## Integration Points

### al-core Changes

```rust
// workspace.rs — add field:
pub debug_session: tokio::sync::Mutex<Option<al_dap_client::DebugSession>>,

// lib.rs — no new module needed, just the workspace field
```

### al-lsp::daemon Changes

Add `"debug"` arm in `dispatch_request()`:
```rust
"debug" => dispatch_debug(workspace, id, &params),
```

`dispatch_debug()` matches `params["cmd"]`:
- `"start"` → creates session, stores in `workspace.debug_session`
- `"breakpoint"` → calls `session.set_breakpoints()`
- `"state"` → calls `session.state()`
- `"eval"` → calls `session.eval()`
- `"continue"` → calls `session.continue_()`
- `"step"` → calls `session.step()`
- `"history"` → calls `session.history()`
- `"stop"` → calls `session.stop()`, clears `workspace.debug_session`

Uses `tokio::task::spawn_blocking` for long-running async calls (especially `start` which includes compilation). Unlike `block_in_place` (used by `dispatch_compile`), `spawn_blocking` avoids starving the tokio worker pool during multi-minute compilations.

### Idle Timeout Protection

In the daemon's idle timeout checker, skip shutdown if `workspace.debug_session` is `Some`.

### al-cli Changes

Add `Commands::Debug { subcommand: DebugCommands }` with subcommands:
```
al debug start [--config <name>]
al debug breakpoint <file> <line> [--condition <expr>]
al debug breakpoint --clear [--all]
al debug state
al debug eval <expr>
al debug continue
al debug step [over|into|out]
al debug history [--var <name>]
al debug stop
```

All send JSON-RPC `"debug"` method with `{"cmd":"...", ...}` params. The `start` subcommand uses a 120-second read timeout (compilation can be slow).

### al-lsp::dap Cleanup

`find_editor_services()` moves to `al-dap-client`. The existing `al-lsp::dap` module imports it from there (via al-core's transitive dependency). The proxy mode (`run_dap_proxy`) stays in al-lsp since it's transport-specific.

## Architecture Rule Updates

### code-boundaries.md — Updated Table

| Crate | MAY import | MUST NOT import |
|-------|-----------|-----------------|
| al-dap-client | al-protocol, tokio, serde | al-core, al-lsp, al-syntax, al-symbols, al-semantic |

The old `al-dap` row is removed (crate deleted in T401).

### CLAUDE.md Architecture Diagram

```
al-cli / al-explorer / al-mcp  ->  al-lsp daemon (Unix socket)  ->  al-core  ->  al-syntax
zed-al (WASM)                   ->  al-lsp (stdio)               ->           ->  al-symbols
                                                                              ->  al-semantic
                                                                              ->  al-diag
                                                                              ->  al-dap-client
```

## Sub-task Breakdown

### T404a: al-dap-client crate — DAP protocol + DapClient
- **Files**: `crates/al-dap-client/` (new crate, 6 source files)
- **Dependencies**: None (new crate)
- **Pass criteria**: `DapClient::spawn()` can launch a subprocess, send a DAP request, receive a response. Background event reader works. `cargo test -p al-dap-client` passes. `cargo tree -p al-dap-client` shows no forbidden deps.
- **Fail criteria**: DapClient requires al-core types. Framing helpers can't parse standard DAP frames.

### T404b: DebugSession lifecycle — start + stop
- **Files**: `crates/al-dap-client/src/session.rs`, `crates/al-dap-client/src/editor_services.rs`
- **Dependencies**: T404a
- **Pass criteria**: `DebugSession::start()` compiles project, spawns EditorServices.Host, completes DAP handshake (initialize → configurationDone → launch). `stop()` sends disconnect and kills child. Drop guard prevents orphan processes.
- **Fail criteria**: EditorServices.Host process orphaned on stop/crash. Compilation error not surfaced.

### T404c: Breakpoints + execution control
- **Files**: `crates/al-dap-client/src/session.rs`
- **Dependencies**: T404b
- **Pass criteria**: `set_breakpoints()` sends DAP `setBreakpoints` with correct source/line/condition. `continue_()` sends `continue`. `step("over"|"into"|"out")` sends `next`/`stepIn`/`stepOut`. All return updated state after the next `stopped` event.
- **Fail criteria**: Conditional breakpoints silently ignored. Step type not mapped correctly.

### T404d: State inspection + eval
- **Files**: `crates/al-dap-client/src/session.rs`, `crates/al-dap-client/src/types.rs`
- **Dependencies**: T404b
- **Pass criteria**: `state()` returns threads, stack frames, scoped variables. Records with `variablesReference > 0` expanded 1 level to show field values. `eval()` evaluates AL expressions at current frame. State correctly reports Running vs Paused.
- **Fail criteria**: Record fields not expanded. Variables from wrong scope. Eval fails on valid AL expressions.

### T404e: Wire-up — daemon + CLI + architecture updates
- **Files**: `crates/al-core/src/workspace.rs`, `crates/al-lsp/src/daemon.rs`, `crates/al-cli/src/main.rs`, `.claude/rules/code-boundaries.md`, `CLAUDE.md`
- **Dependencies**: T404a-d
- **Pass criteria**: `al debug start` through `al debug stop` work end-to-end via daemon. JSON output matches `docs/agentic-schemas.md` schemas. Daemon doesn't auto-shutdown during active debug session. `cargo tree -p al-dap-client` shows no forbidden deps. Architecture rules updated.
- **Fail criteria**: CLI timeout on `debug start`. Daemon shuts down during active session. JSON output doesn't match schemas.

## Testing Strategy

Unit tests in `al-dap-client`:
- **protocol.rs**: Serialization/deserialization round-trips for all DAP message types
- **framing.rs**: Parse `Content-Length` headers, handle edge cases (no seq, malformed headers)
- **client.rs**: Spawn a mock subprocess (echo server), verify send/receive cycle
- **types.rs**: Serialization of all AL result types, camelCase field names
- **session.rs**: State machine transitions (compile → running → paused → stopped)

Integration tests require EditorServices.Host (may not be available in CI). Mark as `#[ignore]` with clear instructions for manual validation.

## Design Decisions

### Single-response `start` (not two-phase)

The `agentic-schemas.md` shows a two-phase response for `debug start` (compiling → running). However, the daemon protocol is synchronous request/response — one request, one response. The `start` command blocks until the session is running (or fails) and returns a single JSON response:
```json
{"cmd":"start","status":"running","session":"s1","pid":12345}
```
On failure: `{"error":"Compilation failed: ...","code":-32000}`. The CLI uses a 120-second timeout. The schema should be updated in T404e to match.

### al-lsp Cargo.toml dependency

`al-lsp` needs `al-dap-client` in its `Cargo.toml` if `al-lsp::dap` imports `find_editor_services()` from it. Alternatively, `al-lsp::dap` can keep its own copy until a future cleanup. T404e should add `al-dap-client` to al-lsp's deps.

### Invalid step type handling

`step()` returns `DapError::DapProtocolError` with message `"Invalid step type: <value>. Use over, into, or out."` for unrecognized step types.

### Blocking I/O in async start()

`find_launch_config()` does sync filesystem reads. Since this reads a single small JSON file (<1KB), wrapping in `spawn_blocking` is unnecessary overhead. The `compile_project()` subprocess is the real blocking operation and is already async (uses `tokio::process::Command`).

### Files to update in T404e (complete list)

- `crates/al-core/src/workspace.rs` — add debug_session field
- `crates/al-lsp/src/daemon.rs` — add dispatch_debug() + idle timeout check
- `crates/al-lsp/Cargo.toml` — add al-dap-client dependency
- `crates/al-cli/src/main.rs` — add Debug subcommands
- `.claude/rules/code-boundaries.md` — al-dap-client row (already done)
- `CLAUDE.md` — architecture table (already done)
- `docs/architecture.md` — add al-dap-client to crate table
- `docs/crates-map.md` — add al-dap-client entry
- `docs/agentic-schemas.md` — update debug start schema to single-response

## Out of Scope

- **T405 (History recording)**: `DebugSession` includes the `history` Vec but T404 does not fill it. T405 adds the recording logic.
- **MCP `al/debug` tool**: al-mcp already delegates to `al --json`. Once CLI works, MCP works automatically.
- **Refactoring al-lsp::dap proxy to use al-dap-client**: Future cleanup. The proxy mode is different enough (passthrough pipe vs programmatic control) that sharing code is optional.
