# Daemon Protocol

**Modules:** `crates/al-lsp/src/server/daemon/` + `crates/al-protocol/`. **Status:** ✅ shipped
(Linux, macOS, and Windows)

`al-lsp daemon --project <path>` is the shared backend whose dispatcher is reused by the CLI,
contributor tasks, and the MCP bridge. Daemon mode serves JSON-RPC 2.0 over a Unix-domain socket on
Linux/macOS and a named pipe on Windows. MCP mode calls that same dispatcher in-process over stdio. (The editor LSP
path does **not** use the daemon — it uses LSP handlers directly. See
[architecture](../architecture.md).)

## Transport (`al-protocol/`)

- **Wire format:** newline-delimited JSON-RPC 2.0 (`Request { jsonrpc, id, method, params? }`,
  `Response { jsonrpc, id, result? , error? }`, `RpcError { code, message }`). Standard error codes
  plus `-32000` (code analysis), `-32001` (file not found) and `-32002` (a path outside the loaded
  project).
- **Request ids:** string, number, and `null` ids are all accepted per the spec, and the response
  echoes the id exactly as received. A message with **no** `id` is a notification: it is dispatched
  but never answered. Text that is not valid JSON returns `-32700` (parse error) with a `null` id.
  Valid JSON that is not a valid request object returns `-32600` (invalid request), echoing the id
  when one is present.
- **Endpoint name (`socket.rs`):** deterministic — an FNV-1a hash of the canonicalized project root.
  Linux uses `$XDG_RUNTIME_DIR/al-lsp/{hash}.sock` with a `/run/user/{uid}` fallback. MacOS uses its
  per-user `$TMPDIR` when XDG is unset. Windows uses
  `\\.\pipe\al-lsp-{user-scope-hash}-{project-hash}`. Unix directories are `0700` and sockets `0600`.
- **Auto-start and locking (`client/mod.rs`):** `DaemonClient::connect` tries the local endpoint,
  else takes a per-project filesystem `.lock` (atomic `create_new`) and spawns the daemon while
  losers wait. Stale locks
  (>30 s) are reclaimed. Client timeouts: 2 s socket poll (not the request deadline), 30 s default
  request timeout, 60 s init wait with 250 ms retries while the daemon reports "initializing".
  When a request deadline expires the client remembers that id and drains the daemon's late answer
  before reading the next response, so one slow request does not skew the connection.
- **Build identity (`identity.rs`, `handshake`):** a daemon outlives the command that started it, so
  `connect` asks a daemon it did not start which build it came from and compares that with its own.
  A mismatch, or a daemon too old to answer `handshake`, is asked to shut down. The client waits for
  the endpoint to stop accepting, starts the `al-lsp` beside its own executable, and retries once,
  never in a loop. `AL_ALLOW_MISMATCHED_DAEMON` keeps the running daemon instead. The identity is the
  `al-lsp` version plus the git commit with a dirty marker, and a hash of the `al-lsp` executable's
  size and mtime when the tree is dirty or is not a git checkout — a rebuild of uncommitted work
  does not change the commit, and that is the case this catches. `connect_existing` does no
  handshake, so lifecycle tooling can reach a daemon without replacing it.

  The identity answers which build, not who. It is not an access control and nothing may be
  built on it as one: what decides whether a daemon may be talked to is the endpoint check in
  `endpoint.rs`, which walks the socket directory's owners, refuses an endpoint that is a
  symlink or not a socket, and compares the peer's uid (`SO_PEERCRED`, `getpeereid`) with this
  user's before a byte is sent.

  Within that, the answer is still bound to a process that can read this user's runtime
  directory. The client sends a nonce. The daemon answers with an HMAC-SHA256 over the nonce
  and the identity, keyed by `handshake.key` in that directory (32 random bytes, mode 0600,
  created with `create_new` by whichever side looks first). The client verifies it. Without
  that, every input to the identity is world-readable — the commit is in the binary and the
  file tag hashes a length and an mtime anyone can `stat` — and a one-line answer passed as
  a matching build. A daemon that answers without a proof is treated as a mismatch and
  replaced, which is what a daemon predating this needs anyway.
- **Binary resolution (`find_al_lsp_binary`):** the `al-lsp` beside the running executable wins. A
  fallback to PATH is logged at warn level with the path and version, and refused when that version
  differs from the client's, with an error naming both and how to install a matching pair.
  `AL_ALLOW_MISMATCHED_DAEMON` allows it. `al-lsp --version` prints `al-lsp <version> (<build>)`,
  which is what the client runs for the check.
- **Lifecycle (`daemon/mod.rs`):** ≤64 concurrent connections (semaphore) — a connection over the
  limit receives a JSON-RPC "server busy" error frame before the socket is closed, rather than being
  dropped silently. Graceful 10 s drain on shutdown. 64 MB max request line. A lifecycle task polls
  once a second and stops the daemon in two cases:
  - **Idle:** 30 minutes by default, measured from the *start* as well as the end of each request and
    suspended entirely while any request is in flight, so a long build/download/live-BC capture
    cannot be reaped mid-request (it is also skipped while a debug session is active). Both skips
    are logged at warn with the in-flight count, because past the idle window they are the two
    reasons a daemon outlives the session that started it. `--idle-timeout-secs` or
    `AL_DAEMON_IDLE_SECS` change the window, and `0` keeps a daemon that an editor session owns.
  - **Project root gone:** the daemon stops once its project directory has been missing for two
    consecutive polls, whatever the idle window and whatever is in flight. Nothing can use such a
    daemon, and deleted git worktrees were the largest source of resident ones. Two polls, so a
    momentary filesystem failure does not stop a live daemon.
- **Memory:** `status` reports `memory.residentBytes` and `memory.peakResidentBytes`, and
  `diag/summary` the same pair under `process`. The per-structure totals beside them count
  allocations the workspace owns. These are what the operating system sees, which is the number that
  decides whether a daemon is worth restarting. Current resident size comes from `/proc/self/status`
  on Linux and is null elsewhere. The peak comes from `getrusage` on every Unix.
- **Startup resilience:** a workspace file the document store rejects (over `maxDocumentSizeBytes`,
  unreadable, or without a file URI) is skipped with a warning. It no longer aborts daemon and MCP
  startup.
- **Per-connection ordering:** requests on one connection are served one at a time, in order, which
  matches the shipped synchronous client (`DaemonClient` sends one request and waits for its
  response). A client that wants concurrent work — or wants to keep issuing cheap queries while a
  build runs — opens a second connection. Up to 64 are served simultaneously. There is no
  per-request cancellation, so a request already dispatched runs to completion even if its client
  gives up waiting.

## Dispatch

`dispatch_request()` in `daemon/mod.rs` routes a method string to a handler. The handlers live in
focused submodules:

- `lsp_dispatch.rs` — the LSP-style queries (hover, definition, references, implementations,
  completions, signatureHelp, rename, documentSymbols, foldingRanges, semanticTokens, inlayHints,
  codeActions, search, object, byId, events, subscribers, composed, packages, deps).
- `build_dispatch/` — build, analysis, codegen, fixes, ID allocation, symbol/auth, tests, and XLIFF
  (`mod.rs`, `build/`, `codegen.rs`, `fixes.rs`, `free_ids.rs`, `symbols_auth.rs`, `tests_dispatch/`,
  `xliff.rs`).
- `insight_dispatch.rs` — trace, traceChain, entrypoints, graphExport, insightStats, deadCode,
  nativeCheck, impact, tableImpact, suggestEvent, eventMap.
- `mod.rs` itself answers `diag`, `ping`, `status`, `handshake` and `shutdown`. The
  `dispatch_table!` list there declares, for every method, whether it reads or rewrites a path the
  caller names and whether it can spend a Business Central credential, and generates the dispatch
  match from that list.
- `debug_dispatch.rs` — stateful `debug` session control (start, breakpoint, stack/variables/globals,
  expand/eval, continue/step, history, stop), used by both CLI and MCP `al_debug`.

The complete method list is in the [daemon method reference](../reference/daemon-methods.md). Notable
hardening: duplicate-detection `minTokens`/`minSimilarity` are clamped to safe ranges.
Graph export is capped at 50k nodes+edges. Trace depth is bounded. JSON-RPC `null` results are
serialized explicitly. Workspace-scale walks (`deadCode`, `trace`, `traceChain`, `entrypoints`,
`graphExport`, `insightStats`, `impact`, `tableImpact`, `suggestEvent`, `eventMap`) run on the
blocking pool so they cannot stall the async worker driving
every connection's I/O.

## One dispatcher, three front ends

This is the architectural point of the daemon: **CLI, MCP, and contributor tasks converge here.**
`al-explorer` sends these methods directly. MCP's `al_call` forwards any method and parameter object
to the same dispatcher, with named aliases for common agent workflows. Checkout-local `.zed` tasks
shell out to `al-explorer`.
There is therefore exactly one implementation of each operation, and its answer is identical
regardless of who asked. The generic MCP bridge also prevents a new dispatcher method from becoming
CLI-only because somebody forgot a second registration.

## Microsoft comparison

Microsoft's AL tooling has no equivalent public daemon/IPC surface — analysis is internal to the VS
Code extension and the .NET language server. The daemon exposes the whole engine as a documented,
scriptable JSON-RPC API, which is what makes CI integration and AI tooling possible.

## Why this approach

A persistent daemon amortizes the expensive indexing/graph-building work across many cheap requests,
which is what makes both the CLI and the TUI feel instant after the first call. Centralizing dispatch
guarantees consistency across surfaces and gives one place to enforce limits (concurrency, request
size, idle shutdown) and one place to add a new capability. MCP receives it immediately through
`al_call`. CLI commands and Zed task shortcuts can then add purpose-built argument UX where useful.

## How to use

You rarely talk to it directly — `al-explorer` and MCP do. To run it explicitly:

```
al-lsp daemon --project /path/to/project [--idle-timeout-secs 1800]
```

Then any `al-explorer <command>` in that project connects to it (auto-starting it if needed).

To stop one, `al-explorer daemon-shutdown` in the project directory. It returns only once the
endpoint has stopped accepting, so the next command cannot reach a dying daemon. The plugin runs it
from a `SessionEnd` hook.

## Platform verification

Cross-platform support is covered at three levels:

- `cargo test -p al-protocol` includes
  `client::cross_platform_tests::local_transport_round_trip_uses_real_platform_backend`. It binds
  the backend selected for the host, exchanges a framed JSON-RPC request and response, and therefore
  exercises a Unix-domain socket on Linux/macOS or a real named pipe on Windows.
- The native harness runs the compiled `al-lsp` and `al-explorer` binaries. Its `cli_smoke` and
  `extension_smoke` suites verify daemon auto-start plus a request/response round trip.
- `.github/workflows/ci.yml` runs both layers on `windows-latest`, after building both binaries.
  This matters because cross-compiling can prove that Windows code builds, but cannot exercise the
  named-pipe runtime. Repository consistency tests pin those Windows CI commands so that coverage
  cannot be silently removed while the documentation still claims support.

See the [testing guide](../testing-guide.md#daemon-ipc-on-linux-macos-and-windows) for the exact
commands and platform matrix.

## Compatibility boundaries

- Local-only transport on every supported platform: Unix-domain sockets on Linux/macOS and named
  pipes on Windows.
- Build boundary: daemon `compile` and `package` are JSON-RPC transport aliases over the shared
  `al_compile` service and return the same normalized build envelope. LSP `al.compile` stays a
  direct handler because it must publish and clear editor diagnostics. It calls the same service
  and backend selection rather than forwarding through daemon IPC. Publish and native DAP use the
  returned artifact path from that service, never a newest-file scan.
