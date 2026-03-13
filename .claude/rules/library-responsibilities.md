# Library Responsibilities & Code Reuse
**Status: Non-Negotiable**

Every crate in the workspace has a strictly defined set of responsibilities as documented in `crates-map.md`.

### Rules:
1. **Strict Boundary Adherence**: A library must never implement logic that falls under the jurisdiction of another crate.
2. **Reuse over Duplication**: If functionality is needed that already exists in another library, that library must be imported and reused. Duplicating logic across library boundaries is a critical architectural failure.
3. **Single Source of Truth**: Shared data types and business logic must live in their designated owner crate only.

### Ownership Map (Quick Reference):
| Data / Logic | Owner Crate | Violation Example |
|---|---|---|
| `AppDependency`, `NuGetFeed` | `al-symbols` | Defining a `Dependency` struct in `al-core` |
| `AlProject`, `AppManifest` | `al-core::project` | Parsing `app.json` in `al-cli` |
| `AlToolchain`, analyzer paths | `al-core::toolchain` | Toolchain discovery in `al-lsp` |
| `SymbolEntry`, `ObjectKind`, `SymbolPackage` | `al-symbols` | Re-defining `ObjectKind` in `al-syntax` |
| Query implementations (hover, def, completions) | `al-core::queries` | Building hover markdown in `al-lsp` |
| Parse tree operations, AST navigation | `al-syntax` | Tree-sitter node traversal in `al-core` |
| `.app` file parsing, ZIP extraction | `al-symbols` | Reading .app files in `al-core` |
| .NET bridge process management | `al-semantic` (process), `al-core::semantic` (lifecycle) | Spawning .NET process in `al-lsp` |
| Lint rules (syntax-based) | `al-syntax::analyzer_rules` | Writing lint rules in `al-core` |
| Formatting | `al-syntax::formatting` (logic), `al-core::formatting` (orchestration) | Formatting logic in `al-lsp` |
| LSP transport, daemon socket | `al-lsp` | Tower-lsp wiring in al-core |
| JSON-RPC client logic | `al-cli`, `al-explorer`, `al-mcp` | Daemon connection code in al-core |

### Dependency Direction (Enforced):
```
Compile-time:
  al-lsp → al-core → al-syntax, al-symbols, al-semantic, al-diag

Runtime (JSON-RPC, no compile-time dep):
  al-cli, al-explorer, al-mcp → al-lsp daemon (socket)
  zed-al → al-lsp process (stdio)

Analysis libraries (no upward deps):
  al-symbols → (standalone)
  al-syntax  → (standalone)
  al-semantic → (standalone)
```

**Circular dependencies are forbidden.** If `al-syntax` needs data from `al-symbols`, extract the shared type to a common location or restructure the dependency.

### Verification:
During every **Surgical Audit**:
1. Run `cargo tree -p al-lsp` and verify it depends on al-core (and transitively on analysis libs).
2. Check `Cargo.toml` of al-cli, al-explorer, al-mcp — they must NOT depend on al-core.
3. Check for duplicate struct/enum definitions across crate boundaries.
4. Any duplicated code must be deleted and replaced with a dependency on the authoritative crate.
