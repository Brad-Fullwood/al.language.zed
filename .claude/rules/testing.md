---
paths:
  - "crates/al-test-harness/**"
  - "docs/proof_of_functionality.toml"
  - "**/tests/**"
---
# Testing & Evidence Protocol

## Proof of Functionality (PoF)
Every completed task requires a dual-pass entry in `docs/proof_of_functionality.toml`:
1. **Adversarial pass**: Intentionally invalid input proves the system detects failure. Status: `FAILED_AS_EXPECTED`.
2. **Fidelity pass**: Correct behavior demonstrated. Status: `SUCCESS`.

Entries with only a fidelity pass, empty `actual_log`, or placeholder text are invalid.

## TOML Format
```toml
[[entries]]
date = "YYYY-MM-DD"
work_package = "WPX: Name"
task_id = "TXXX: Description"

[entries.adversarial_pass]
context = "What was the adversarial input/condition"
expected = "What the system SHOULD do (reject, error, empty result)"
actual_log = """
[relevant log lines]
"""
status = "FAILED_AS_EXPECTED"

[entries.fidelity_pass]
context = "What was tested"
expected = "What the system SHOULD return"
actual_log = """
[relevant log lines]
"""
status = "SUCCESS"
```

## Harness Requirements
- `open_file()` waits for `publishDiagnostics` (not sleep). `initialize()` polls `workspace/symbol` (30s timeout).
- All tests run against real Debar project fixture with real `.app` packages.
- Every new LSP handler needs a Zed simulation test before merge.
- Every bug fix needs a regression test reproducing the original bug.
- `touch` source files when cargo doesn't detect transitive dep changes.
