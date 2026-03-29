Verify that the crate dependency rules are not violated.

Rules:
- al-syntax, al-symbols, al-semantic must NOT depend on each other or on al-core
- al-daemon-client must NOT depend on al-core
- zed-al must NOT depend on any native crate (it's WASM-only)
- Dependencies flow downward only: al-lsp → al-core → {al-syntax, al-symbols, al-semantic, al-dap-client, al-daemon-client}

Steps:
1. Read all `Cargo.toml` files in `crates/*/Cargo.toml` and `./Cargo.toml`
2. Build a dependency graph
3. Check for violations of the rules above
4. Report any violations with specific file paths

Also check for:
- Unnecessary dependencies (imported but not used)
- Version mismatches between crates using the same dependency
- Dependencies that should use `workspace = true` but don't
