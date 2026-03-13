# Persistent Adversarial Evolution
**Status: Continuous Deployment**

The `al-test-harness` must be treated as a **Persistent Adversarial System**.

### Mandate:
1. **WPX Continuity**: A sub-agent is permanently deployed to hunt for regressions and edge cases. See `docs/adversarial-atlas.md` for the full stress test catalog.
2. **Find the Gap**: Identifying any discrepancy between CLI/MCP success and real-world Zed-fidelity.
3. **Adversarial Updates**: Update the harness alongside every feature to include deliberate failure cases.

### Stress Test Catalog:
The full catalog is in `docs/adversarial-atlas.md`. Key categories:
- **ST-01 through ST-15**: Stress tests (large files, concurrent requests, crash recovery, Unicode, etc.)
- **FG-01 through FG-07**: Fidelity gap register (URI encoding, semantic tokens, diagnostic lifecycle, etc.)

### When to Add New Tests:
| Trigger | Required Action |
|---|---|
| New LSP handler implemented | Add Zed simulation test + stress test for concurrent requests |
| New CLI command added | Add integration test + adversarial test (bad args, missing project) |
| Bug fixed | Add regression test that reproduces the original bug |
| Symbol parsing change | Add test with corrupt/edge-case .app file |
| Grammar change | Add test with new syntax + adversarial (malformed input) |
| Performance optimization | Add benchmark test with before/after latency measurement |

### Feedback Loop:
1. Discovered gaps or "Success-only" trends (insufficient negative testing) are reported to the PM as immediate Priority-0 tasks.
2. If the log stops showing failures, the harness is brittle — add more adversarial inputs.
3. Every audit must check the adversarial-to-fidelity ratio in PoF. Target: at least 1 adversarial test per fidelity test.

### Test Infrastructure Requirements:
- **Harness rebuild**: `cargo` doesn't always detect transitive dependency changes — `touch` source files to force rebuild when needed.
- **Fixture management**: Real-world fixtures (Debar project) must be checked into the repo or downloadable via a deterministic script.
- **Timeout enforcement**: All async tests must have explicit timeouts (30s default) to prevent hanging on CI.
