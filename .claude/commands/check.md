Run the full CI check pipeline locally. Fix any issues found.

Steps:
1. `cargo fmt --all` — format all code
2. `cargo check --workspace --exclude zed-al` — compile check
3. `cargo clippy --workspace --exclude zed-al -- -D warnings` — lint
4. `cargo test --workspace --exclude zed-al` — run all tests

If any step fails, fix the issue and re-run that step. Do NOT skip steps or ignore warnings.

Report a summary of what passed and what (if anything) needed fixing.
