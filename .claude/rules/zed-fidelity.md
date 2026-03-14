---
paths:
  - "crates/al-lsp/**"
  - "crates/al-test-harness/**"
  - "crates/zed-al/**"
---
# Zed Fidelity

If the harness claims a feature works but it fails in Zed, that is a Priority-0 blocking issue. Update the harness before continuing.

## Known Fidelity Risks
| Risk | Mitigation |
|---|---|
| URI encoding (spaces, unicode) | `file_uri()` percent-encodes spaces |
| Semantic token delta encoding | Compare against Zed's expected encoding |
| Diagnostic lifecycle | Publish empty array when errors fixed |
| Initialization race | Queue/reject requests during init |
| WASM memory limits | Extension init < 10MB |
| DAP adapter path | Test with multiple launch.json configs |

## Requirements
- Golden file comparison for semantic tokens and document symbols.
- Zed simulation tests cover every LSP method in `docs/lsp-feature-matrix.md`.
- If harness passes but Zed fails, fix the harness first (Priority-0).
