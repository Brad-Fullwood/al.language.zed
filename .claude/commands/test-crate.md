Run tests for a specific crate and report results.

Usage: /test-crate <crate-name>
Example: /test-crate al-core

Steps:
1. Run `cargo test -p $ARGUMENTS` with output
2. If tests fail, analyze the failures and report:
   - Which tests failed
   - The error messages
   - Likely root cause
3. If all tests pass, report the count of passing tests

Do NOT fix failures automatically — just report them.
