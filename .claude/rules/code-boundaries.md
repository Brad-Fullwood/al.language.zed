---
paths:
  - "crates/*/src/**/*.rs"
  - "crates/*/Cargo.toml"
---
# Code Boundaries & Quality

## Library Ownership (authoritative source: `docs/crates-map.md`)
- **al-syntax**: Parse trees, AST navigation, formatting, lint rules, type resolution. No dependency on al-symbols.
- **al-symbols**: `.app` parsing, symbol index, composition, events. Owns `AppDependency`, `NuGetFeed`, `SymbolEntry`, `ObjectKind`.
- **al-semantic**: .NET bridge (in-process CLR). Owns bridge process management.
- **al-core**: Workspace state, queries, project/toolchain discovery. Orchestrates analysis libs.
- **al-lsp**: Transport only (tower-lsp, daemon socket, DAP proxy). No query logic.

Do not duplicate types or logic across crate boundaries. If it exists in the owner crate, import it.

## Error Handling: Fail Loudly
- Errors must surface to the user via `window/showMessage` or diagnostics. Silent failure is a bug.
- Banned: `.ok()?` without logging+notification, `unwrap_or_default()` on meaningful failures, `warn!()` as sole error reporting.
- Acceptable silent handling (with comment): tree-sitter `utf8_text()` failures, individual token classification misses, cache write failures.
