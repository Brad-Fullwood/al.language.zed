# Daemon Protocol

**Modules:** `crates/al-lsp/src/server/daemon/` + `crates/al-protocol/` · **Status:** ✅ shipped
(Linux, macOS, and Windows)

`al-lsp daemon --project <path>` is the shared backend whose dispatcher is reused by the CLI, the MCP
bridge, and Zed tasks. Daemon mode serves JSON-RPC 2.0 over a Unix-domain socket on Linux/macOS and a
named pipe on Windows; MCP mode calls that same dispatcher in-process over stdio. (The editor LSP
path does **not** use the daemon — it uses LSP handlers directly. See
[architecture](../architecture.md).)

## Transport (`al-protocol/`)

- **Wire format:** newline-delimited JSON-RPC 2.0 (`Request { jsonrpc, id, method, params? }`,
  `Response { jsonrpc, id, result? , error? }`, `RpcError { code, message }`). Standard error codes
  plus `-32000` (code analysis) and `-32001` (file not found).
- **Socket path (`socket.rs`):** deterministic — FNV-1a hash of the canonicalized project root →
  `$XDG_RUNTIME_DIR/al-lsp/{hash}.sock`, with a `/run/user/{uid}` fallback. Directory `0700`, socket
  `0600`.
- **Auto-start and locking (`client.rs`):** `DaemonClient::connect` tries the socket, else takes
  a per-socket `.lock` (atomic `create_new`) and spawns the daemon while losers wait; stale locks
  (>30 s) are reclaimed. Client timeouts: 2 s socket poll (not the request deadline), 30 s default
  request timeout, 60 s init wait with 250 ms retries while the daemon reports "initializing".
- **Lifecycle (`daemon/mod.rs`):** ≤64 concurrent connections (semaphore); 30-minute idle timeout
  (skipped while a debug session is active); graceful 10 s drain on shutdown; 64 MB max request line.

## Dispatch

`dispatch_request()` in `daemon/mod.rs` routes a method string to a handler. The handlers live in
focused submodules:

- `lsp_dispatch.rs` — the LSP-style queries (hover, definition, references, implementations,
  completions, signatureHelp, rename, documentSymbols, foldingRanges, semanticTokens, inlayHints,
  codeActions, search, object, byId, events, subscribers, composed, packages, deps).
- `build_dispatch/` — build, analysis, codegen, fixes, symbol/auth, tests, and XLIFF
  (`mod.rs`, `build.rs`, `codegen.rs`, `symbols_auth.rs`, `tests_dispatch.rs`, `fixes.rs`, `xliff.rs`).
- `insight_dispatch.rs` — trace, traceChain, entrypoints, graphExport, insightStats, deadCode, impact,
  tableImpact, suggestEvent, eventMap.
- `debug_dispatch.rs` — stateful `debug` session control (start, breakpoint, stack/variables/globals,
  expand/eval, continue/step, history, stop), used by both CLI and MCP `al_debug`.

The complete method list is in the [daemon method reference](../reference/daemon-methods.md). Notable
hardening: duplicate-detection `minTokens`/`minSimilarity` are clamped to safe ranges;
graph export is capped at 50k nodes+edges; trace depth is bounded; JSON-RPC `null` results are
serialized explicitly.

## One dispatcher, three front ends

This is the architectural point of the daemon: **CLI, MCP, and Zed tasks converge here.** `al-explorer`
sends these methods directly; MCP's `al_call` forwards any method and parameter object to the same
dispatcher, with named aliases for common agent workflows; Zed tasks shell out to `al-explorer`.
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
`al_call`; CLI commands and Zed task shortcuts can then add purpose-built argument UX where useful.

## How to use

You rarely talk to it directly — `al-explorer` and MCP do. To run it explicitly:

```
al-lsp daemon --project /path/to/project
```

Then any `al-explorer <command>` in that project connects to it (auto-starting it if needed).

## Limitations & roadmap

- Local-only transport on every supported platform: Unix-domain sockets on Linux/macOS and named
  pipes on Windows.
- `ROADMAP.md` (Architecture Unification) calls for deciding whether LSP execute commands should call
  the daemon dispatcher, a shared service layer, or remain direct LSP handlers — and documenting/
  testing that boundary — plus unifying compile behavior across daemon `compile`/`package`, LSP
  `al.compile`, publish, and DAP launch.
