---
name: pof
description: "Create a Proof of Functionality entry in docs/proof_of_functionality.toml"
argument-hint: "<WP name> <task ID>"
disable-model-invocation: true
allowed-tools: Read, Edit
---

Create a dual-pass PoF entry. Arguments: $ARGUMENTS (expected format: "WPX: Name" "TXXX: Description")

## Steps

1. Read `docs/proof_of_functionality.toml` to see existing entries.
2. Parse the arguments: the work package name and task ID from `$ARGUMENTS`.
3. Append a new `[[entries]]` block at the bottom with today's date.
4. The adversarial_pass and fidelity_pass sections MUST have real `actual_log` content from test output — never use placeholder text.
5. Status values: adversarial = `FAILED_AS_EXPECTED`, fidelity = `SUCCESS`.

## Template
```toml
[[entries]]
date = "YYYY-MM-DD"
work_package = "<from arguments>"
task_id = "<from arguments>"

[entries.adversarial_pass]
context = ""
expected = ""
actual_log = """
"""
status = "FAILED_AS_EXPECTED"

[entries.fidelity_pass]
context = ""
expected = ""
actual_log = """
"""
status = "SUCCESS"
```
