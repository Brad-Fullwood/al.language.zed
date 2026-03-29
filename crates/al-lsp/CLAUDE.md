# al-lsp — Transport Layer (~7K lines)

**TRANSPORT ONLY.** All business logic lives in al-core/src/queries/.

## The Pattern (every handler)

```
1. Receive LSP/JSON-RPC request
2. Convert params to al-core types
3. Call al_core::queries::* with &workspace
4. Convert result to LSP/JSON-RPC response
5. Return
```

If you're writing >10 lines of non-trivial logic here, it belongs in al-core.

## Server Modes (main.rs)

| Mode | Arg | Transport | Client |
|------|-----|-----------|--------|
| LSP | `--stdio` (default) | tower-lsp stdin/stdout | Zed |
| Daemon | `daemon --project <path>` | JSON-RPC Unix socket | al-cli, al-explorer |
| DAP | `--dap` | DAP over stdio | Zed debugger |

## Source Layout

| File | Purpose |
|------|---------|
| main.rs | Entry point, arg parsing |
| server.rs | tower-lsp setup, capability registration |
| handlers.rs | LSP request/notification dispatch |
| workspace.rs | Workspace lifecycle (init, open, close, change) |
| completions.rs | Completion handler (thin wrapper over al-core) |
| definition.rs | Go-to-definition handler |
| hover.rs | Hover handler |
| formatting.rs | Format handler |
| diagnostics.rs | Diagnostic publishing |

## Daemon (daemon/)

| File | Purpose |
|------|---------|
| mod.rs | Unix socket server, JSON-RPC framing, idle timeout |
| lsp_dispatch.rs | Routes JSON-RPC → al-core queries |
| build_dispatch.rs | Build/compile commands |
| debug_dispatch.rs | Debug adapter dispatch |
| insight_dispatch.rs | InsightGraph queries |

## DAP (dap/)

| File | Purpose |
|------|---------|
| mod.rs | Debug Adapter Protocol server |
| editor_services.rs | Editor service integration |

## Key Constraints

- No tree-sitter operations — that's al-core territory
- No blocking I/O in async handlers — use tokio::fs or spawn_blocking
- No unwrap() — return LSP error responses
- Convert UTF-16 positions to byte offsets before passing to al-core
