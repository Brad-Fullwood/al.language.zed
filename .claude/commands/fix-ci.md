The CI pipeline is failing. Diagnose and fix the issues.

Steps:
1. Run the full check pipeline locally:
   - `cargo fmt --all -- --check` (formatting)
   - `cargo check --workspace --exclude zed-al` (compilation)
   - `cargo clippy --workspace --exclude zed-al -- -D warnings` (linting)
   - `cargo test --workspace --exclude zed-al` (tests)

2. For each failure:
   - Identify the root cause
   - Fix it with minimal changes (do NOT refactor surrounding code)
   - Re-run the failing step to confirm the fix

3. After all fixes, run the full pipeline once more to verify everything passes.

Rules:
- Fix ONLY what's broken — no opportunistic cleanup
- If a fix requires changing a public API, flag it for human review
- If a test is flaky (passes sometimes), investigate the race condition rather than adding retries
