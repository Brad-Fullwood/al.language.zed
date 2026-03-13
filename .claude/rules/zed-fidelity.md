# Zed-Fidelity & Zero Regression
**Status: Absolute Mandate**

It is a **critical project failure** if an agent or the test harness claims a feature works (even in CLI/MCP) but it fails in the real Zed environment.

### Rules:
1. **LSP Simulation**: All changes affecting Zed must be verified by the `al-test-harness` LSP simulator, mimicking Zed's JSON-RPC protocol exactly.
2. **Project Fixtures**: Use real-world `.al` project structures for testing (the Debar project fixture at minimum).
3. **Regression Proofing**: Any regression in Zed-fidelity must be reported as a **Priority-0 blocking issue**.

### Known Fidelity Risks (see `docs/adversarial-atlas.md` for full list):
| Risk | Mitigation |
|---|---|
| URI encoding (spaces, unicode) | `file_uri()` must percent-encode spaces; test with paths containing spaces |
| Semantic token delta encoding | Compare harness output against Zed's expected encoding |
| Diagnostic lifecycle | Verify empty diagnostic array published when errors are fixed |
| Initialization race | Harness must send requests during init and verify queue/reject behavior |
| WASM memory limits | Extension must initialize with <10MB WASM memory |
| DAP adapter path | Test `get_dap_binary` with multiple launch.json configurations |

### Harness Requirements:
1. `open_file()` must wait for `publishDiagnostics` notification (not fixed sleep).
2. `initialize()` must poll `workspace/symbol` until non-empty (30s timeout).
3. `buffered_notifications` must preserve notifications consumed during internal waits.
4. All harness tests run against the real Debar project fixture with real `.app` packages.

### Test Suite Minimums:
- **Zed simulation tests**: Must cover every LSP method listed in `docs/lsp-feature-matrix.md`.
- **Every new LSP handler** must have at least one Zed simulation test before merge.
- **Golden file comparison**: For semantic tokens and document symbols, compare output byte-for-byte against golden files.

### Verification:
If the harness passes but Zed fails, the harness must be updated **immediately** before any further work on the feature. The harness gap becomes a Priority-0 blocking issue.
