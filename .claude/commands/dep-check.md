Verify that the crate dependency rules are not violated.

Rules (post-consolidation target — see CLAUDE.md banner for migration status):
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

**During the in-flight refactor**: extra crates (`al-syntax`, `al-symbols`, `al-semantic`, `al-lsp`, `al-cli`, `al-dap-client`, `al-daemon-client`) may still exist on disk. Until those stages land, also enforce the legacy rules for whichever crates remain:
- Foundation crates (`al-syntax`, `al-symbols`, `al-semantic`) must NOT depend on each other or on `al-core`
- `al-daemon-client` must NOT depend on `al-core`
