# Thin Adapter Mandate
**Status: Non-Negotiable**

All binary crates (`al-cli`, `al-lsp`, `al-explorer`) and the WASM entry point (`zed-al`) must be **Thin Adapters**.

### Rules:
1. **Zero Business Logic**: Binaries are strictly for I/O, transport, and protocol glue.
2. **Unified Core**: 100% of analysis, indexing, and orchestration logic must reside in `al-core`.
3. **Consistency**: Logic must not be duplicated. A query executed via the CLI must return the exact same analytical result as the LSP or TUI.

### Verification:
Any logic found in a binary crate that is not strictly related to its specific transport/UI layer must be refactored into `al-core` immediately.
