Verify that the crate dependency rules are not violated.

Rules:
- `al-protocol` must NOT depend on `al-core` (server pulls in core; clients must not)
- `al-explorer` must depend ONLY on `al-protocol` (never on `al-core` directly)
- `zed-al` must NOT depend on any native crate (WASM-only)
- Dependencies flow downward only: `al-explorer → al-protocol`, `al-core → al-protocol`

Steps:
1. Read all `Cargo.toml` files in `crates/*/Cargo.toml` and `./Cargo.toml`
2. Build a dependency graph
3. Check for violations of the rules above
4. Report any violations with specific file paths

Also check for:
- Unnecessary dependencies (imported but not used)
- Version mismatches between crates using the same dependency
- Dependencies that should use `workspace = true` but don't
