Pre-commit checklist. Run this before every commit.

## Checklist

1. **Format**: `cargo fmt --all`
2. **Compile**: `cargo check --workspace --exclude zed-al`
3. **Lint**: `cargo clippy --workspace --exclude zed-al -- -D warnings`
4. **Test**: `cargo test --workspace --exclude zed-al`
5. **Scope check**: Run `git diff --stat` and verify every changed file is relevant to the task
6. **No hardcoded AL values**: `grep -rn "const AL_\|const BUILTIN_\|const TRIGGER_\|const GLOBAL_" crates/` should return nothing
7. **No WIP**: The commit message must be descriptive, not "WIP"

If any check fails, fix it before committing. If the scope check shows unrelated files, revert them with `git checkout -- <file>`.

Only proceed with the commit if ALL checks pass.
