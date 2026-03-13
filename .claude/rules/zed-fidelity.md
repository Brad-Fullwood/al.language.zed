# Zed-Fidelity & Zero Regression
**Status: Absolute Mandate**

It is a **critical project failure** if an agent or the test harness claims a feature works (even in CLI/MCP) but it fails in the real Zed environment.

### Rules:
1. **LSP Simulation**: All changes affecting Zed must be verified by the `al-test-harness` LSP simulator, mimicking Zed's JSON-RPC protocol exactly.
2. **Project Fixtures**: Use real-world `.al` project structures for testing.
3. **Regression Proofing**: Any regression in Zed-fidelity must be reported as a **Priority-0 blocking issue**.

### Verification:
The harness must achieve near-perfect "Zed Fidelity." If the harness passes but Zed fails, the harness must be updated immediately before any further work on the feature.
