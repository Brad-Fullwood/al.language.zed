---
description: Full security, dependency, and quality audit of the workspace
allowed-tools: Bash, Read, Grep, Glob
---

Run every audit check and report ALL findings:

1. `cargo audit 2>&1` — security vulnerabilities in dependencies
2. `cargo deny check 2>&1` — license compliance, duplicate deps, advisories
3. `cargo machete 2>&1` — unused dependencies
4. `cargo clippy --workspace --exclude zed-al --all-targets -- -W clippy::pedantic -W clippy::nursery 2>&1 | head -120` — extended lints
5. `cargo test --workspace --exclude zed-al 2>&1 | tail -20` — full test suite
6. Unwrap audit: `grep -rn '\.unwrap()' crates/*/src/ --include='*.rs' | grep -v '/tests/' | grep -v 'mod tests' | grep -v '_test\.rs' | head -40`
7. Hardcoded AL values: `grep -rn 'const.*\[.*str\]' crates/*/src/ --include='*.rs' | head -20`
8. Dependency direction: verify `crates/al-protocol/Cargo.toml` does not depend on `al-core`; verify `crates/al-explorer/Cargo.toml` only depends on `al-protocol` (never `al-core`)
9. `lsp_types` in queries: `grep -rn 'tower_lsp::lsp_types' crates/al-core/src/queries/ --include='*.rs' | head -20` — should be empty (transport boundary)

Group findings by severity: critical / high / medium / low. Include specific fix recommendations.
