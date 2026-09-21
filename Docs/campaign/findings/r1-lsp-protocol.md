# R1 review: al-lsp and al-protocol

Adversarial read-only review of `crates/al-lsp` and `crates/al-protocol`.
Baseline: AUDIT-BACKLOG.md section "LSP & Protocol" (2026-07-31). Items from that
audit that are still present in current code are tagged [STILL-OPEN].

## Coverage

- [ ] AUDIT-BACKLOG.md "LSP & Protocol" section, verify each item against current code
- [ ] crates/al-protocol/src/lib.rs
- [ ] crates/al-protocol/src/jsonrpc.rs
- [ ] crates/al-protocol/src/socket.rs
- [ ] crates/al-protocol/src/client.rs
- [ ] crates/al-lsp/src/lib.rs + toolchain.rs
- [ ] crates/al-lsp/src/bin/al-lsp.rs
- [ ] crates/al-lsp/src/server/mod.rs
- [ ] crates/al-lsp/src/server/lsp.rs
- [ ] crates/al-lsp/src/server/workspace.rs
- [ ] crates/al-lsp/src/server/diagnostics.rs
- [ ] crates/al-lsp/src/server/commands.rs
- [ ] crates/al-lsp/src/server/handlers.rs
- [ ] crates/al-lsp/src/server/formatting.rs
- [ ] crates/al-lsp/src/server/completions.rs + definition.rs + hover.rs
- [ ] crates/al-lsp/src/server/dap_mode/
- [ ] crates/al-lsp/src/server/mcp.rs
- [ ] crates/al-lsp/src/server/daemon/mod.rs
- [ ] crates/al-lsp/src/server/daemon/lsp_dispatch.rs
- [ ] crates/al-lsp/src/server/daemon/debug_dispatch.rs
- [ ] crates/al-lsp/src/server/daemon/insight_dispatch.rs
- [ ] crates/al-lsp/src/server/daemon/build_dispatch/ (mod, build/, fixes, codegen, xliff, symbols_auth)
- [ ] crates/al-lsp/src/semantic/ + syntax/
- [ ] crates/al-lsp/tests/

## Findings

