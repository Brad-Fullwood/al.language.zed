---
name: pof
description: Create a Proof of Functionality entry in docs/proof_of_functionality.toml with real test output
user_invocable: true
args: "<task_id> <wp_name>"
---

# Proof of Functionality

Create a PoF entry for a completed task with real, fresh test output.

## Arguments

- `task_id`: The task ID (e.g., T101)
- `wp_name`: The work package name (e.g., "WP1: al-core Skeleton & Discovery Migration")

## Steps

### 1. Run Tests Fresh

```bash
cargo test --workspace --exclude zed-al 2>&1 | tail -40
```

You MUST run this now and read the real output. Do not reuse old output or fabricate results.

### 2. Identify Adversarial Evidence

From the test output, find tests that validate negative/edge-case scenarios:
- Tests that verify error handling works
- Tests that check malformed input is rejected
- Tests that confirm boundary conditions

If no adversarial tests exist for this task, note that in the entry.

### 3. Write the Entry

Append a `[[entries]]` block to `docs/proof_of_functionality.toml` under the appropriate WP section:

```toml
[[entries]]
date = "YYYY-MM-DD"
work_package = "WPX: Name"
task_id = "TXXX: Description"

[entries.adversarial_pass]
context = "What negative scenario was tested"
expected = "Expected failure behavior"
actual_log = """
<paste REAL test output here — from the cargo test run above>
"""
status = "FAILED_AS_EXPECTED"

[entries.fidelity_pass]
context = "What positive scenario was tested"
expected = "Expected success behavior"
actual_log = """
<paste REAL test output here — from the cargo test run above>
"""
status = "SUCCESS"
```

### 4. Verify

Read back the entry to confirm it parses as valid TOML and contains real output.

## Rules

- **NEVER fabricate test output.** Every `actual_log` must come from a command you just ran.
- Use today's date.
- Both adversarial_pass and fidelity_pass are required.
- If a task has no natural adversarial test, write one before creating the PoF entry.
