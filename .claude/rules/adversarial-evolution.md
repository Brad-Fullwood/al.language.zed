# Persistent Adversarial Evolution
**Status: Continuous Deployment**

The `al-test-harness` must be treated as a **Persistent Adversarial System**.

### Mandate:
1. **WPX Continuity**: A sub-agent is permanently deployed to hunt for regressions and edge cases.
2. **Find the Gap**: Identifying any discrepancy between CLI/MCP success and real-world Zed-fidelity.
3. **Adversarial Updates**: Update the harness alongside every feature to include deliberate failure cases.

### Feedback Loop:
Discovered gaps or "Success-only" trends (insufficient negative testing) are reported to the PM as immediate Priority-0 tasks. If the log stops showing failures, the harness is brittle.
