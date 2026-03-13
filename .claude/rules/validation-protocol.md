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

### TOML Entry Format:
```toml
[[entries]]
date = "YYYY-MM-DD"
work_package = "WPX: Name"
task_id = "TXXX: Description"

[entries.adversarial_pass]
context = "What was the adversarial input/condition"
expected = "What the system SHOULD do (reject, error, empty result)"
actual_log = """
[relevant log lines proving the failure was detected]
"""
status = "FAILED_AS_EXPECTED"  # ONLY valid value for adversarial pass

[entries.fidelity_pass]
context = "What was tested (include file, position, symbol)"
expected = "What the system SHOULD return"
actual_log = """
[relevant log lines proving correct behavior]
"""
status = "SUCCESS"  # ONLY valid value for fidelity pass
```

### Test Categories Required Per Feature:
| Feature Type | Required Tests |
|---|---|
| LSP handler (hover, def, etc.) | Zed simulation test + adversarial (bad position, missing file) |
| CLI command | Integration test with `--json` + adversarial (bad args, missing project) |
| Symbol parsing | Real .app fixture test + adversarial (corrupt .app, missing JSON) |
| Formatting/linting | Before/after comparison + adversarial (malformed input) |
| Insight graph query | Cross-file trace test + adversarial (cyclic events, missing symbols) |

### Prohibited:
- PoF entries with only a fidelity pass (no adversarial).
- PoF entries with `actual_log = ""` or placeholder text.
- Marking a task complete without a PoF entry.
