# Validation Protocol & Evidence Log
**Status: Mandatory Execution**

Agents must not ask the user for testing. Every task must be verified via an automated **Proof of Functionality (PoF)**.

### Dual-Pass Requirement:
For every feature, provide two structured results in `docs/proof_of_functionality.toml`:
1. **Adversarial Pass (Negative)**: Intentionally break the code or provide invalid input to prove the `al-test-harness` detects the failure. **No Red, No Merge.**
2. **Fidelity Pass (Positive)**: Demonstrate full feature success across LSP (Zed-fidelity), CLI, and MCP.

### Evidence Standard:
- Logs must be structured (TOML).
- Evidence must be surgical: show only the relevant logs needed to prove the failure and the success.
- If a test cannot fail, it is not a test.
