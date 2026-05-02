Pre-commit checklist. Run this before every commit.

## Checklist

1. **Format**: `cargo fmt --all`
2. **Lint (also catches compile errors via -D warnings)**: `cargo clippy --workspace --exclude zed-al -- -D warnings`
3. **Test**: `cargo test --workspace --exclude zed-al`
4. **Unused deps** (if any `Cargo.toml` was touched): `cargo machete`
5. **Scope check**: Run `git diff --stat` and verify every changed file is relevant to the task
6. **No hardcoded AL values**: `grep -rn "const AL_\|const BUILTIN_\|const TRIGGER_\|const GLOBAL_" crates/` should return nothing
7. **No WIP**: The commit message must be descriptive, not "WIP"

Notes:
- We do NOT run a separate `cargo check` here — `cargo clippy` runs the same compiler frontend, so if clippy passes, check passes too.
- Step 4 (`cargo machete`) used to run on every PostToolUse Cargo.toml edit; it was moved here to avoid burning tokens on every dependency tweak. Run it only when you've actually changed a `Cargo.toml`.

If any check fails, fix it before committing. If the scope check shows unrelated files, revert them with `git checkout -- <file>`.

Only proceed with the commit if ALL checks pass.
